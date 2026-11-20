//! Secret-shaped substring redaction (AGENTS.md §3.4). Shared by the provider
//! HTTP clients (error bodies that reach `Display`, logs, and the UI) and the
//! diagnostics bundle. Pure and dependency-free so neither caller pulls in the
//! other's dependencies.

/// Display cap used by [`redact_for_display`].
pub const DISPLAY_MAX_CHARS: usize = 500;

struct Pattern {
    prefix: &'static str,
    /// Minimum token length after `prefix` for the run to count as a secret.
    min_len: usize,
    marker: &'static str,
    is_token_char: fn(char) -> bool,
}

fn key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn bearer_char(c: char) -> bool {
    key_char(c) || matches!(c, '.' | '~' | '+' | '/' | '=')
}

/// `sk-` covers OpenAI keys and Anthropic OAuth tokens (`sk-ant-oat01-...`).
const PATTERNS: &[Pattern] = &[
    Pattern {
        prefix: "sk-",
        min_len: 15,
        marker: "sk-...REDACTED",
        is_token_char: key_char,
    },
    Pattern {
        prefix: "Bearer ",
        min_len: 20,
        marker: "Bearer ...REDACTED",
        is_token_char: bearer_char,
    },
];

/// Replace every secret-shaped run in `text`. No length cap.
pub fn redact_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    scan(text, &mut out, None);
    out
}

/// Redact, then cap at [`DISPLAY_MAX_CHARS`] characters with a
/// `...[truncated, N bytes]` suffix. Only the displayed prefix is retained, so
/// an oversized body never costs a second body-sized allocation, and a key
/// crossing the display boundary is still classified as a whole.
pub fn redact_for_display(body: &str) -> String {
    let mut out = String::with_capacity(DISPLAY_MAX_CHARS);
    if scan(body, &mut out, Some(DISPLAY_MAX_CHARS)) {
        format!("{out}...[truncated, {} bytes]", body.len())
    } else {
        out
    }
}

/// Returns `true` when output stopped at `max_chars` with input remaining.
fn scan(text: &str, out: &mut String, max_chars: Option<usize>) -> bool {
    let mut count = 0usize;
    let mut rest = text;
    loop {
        if max_chars == Some(count) {
            return !rest.is_empty();
        }
        let next = PATTERNS
            .iter()
            .filter_map(|p| rest.find(p.prefix).map(|i| (i, p)))
            .min_by_key(|(i, _)| *i);
        let Some((idx, p)) = next else {
            return push_bounded(out, &mut count, rest, max_chars);
        };
        if push_bounded(out, &mut count, &rest[..idx], max_chars) {
            return true;
        }
        let after = &rest[idx + p.prefix.len()..];
        let len = after
            .find(|c: char| !(p.is_token_char)(c))
            .unwrap_or(after.len());
        let marker = if len >= p.min_len {
            rest = &after[len..];
            p.marker
        } else {
            // Not secret-shaped; emit the prefix verbatim and keep scanning.
            rest = after;
            p.prefix
        };
        if push_bounded(out, &mut count, marker, max_chars) {
            return true;
        }
    }
}

/// Append `segment`, stopping at `max` chars. Returns `true` only when the cap
/// was hit with part of `segment` still unwritten.
fn push_bounded(out: &mut String, count: &mut usize, segment: &str, max: Option<usize>) -> bool {
    let Some(max) = max else {
        out.push_str(segment);
        return false;
    };
    for ch in segment.chars() {
        if *count == max {
            return true;
        }
        out.push(ch);
        *count += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- redact_for_display: HTTP-error-body sanitizer ---

    #[test]
    fn redact_empty_body_is_empty() {
        assert_eq!(redact_for_display(""), "");
    }

    #[test]
    fn redact_body_without_sk_passes_through() {
        let body = r#"{"error":{"message":"Unauthorized","type":"invalid_request_error"}}"#;
        assert_eq!(redact_for_display(body), body);
    }

    #[test]
    fn redact_replaces_admin_key_lookalike() {
        let body = format!(
            "auth failed for sk-{}",
            "admin-AbCdEfGhIjKlMnOpQrStUvWxYz1234567890"
        );
        let out = redact_for_display(&body);
        assert!(
            out.contains("sk-...REDACTED"),
            "expected redaction marker; got: {out}"
        );
        assert!(
            !out.contains("AbCdEfGhIjKlMn"),
            "key body must not survive: {out}"
        );
    }

    #[test]
    fn redact_replaces_multiple_keys_in_one_body() {
        let body = format!(
            "key1 sk-{} mid sk-{} tail",
            "admin-AAAAAAAAAAAAAAAA", "proj-BBBBBBBBBBBBBBBB"
        );
        let out = redact_for_display(&body);
        assert_eq!(out.matches("sk-...REDACTED").count(), 2);
        assert!(!out.contains("AAAAAAAAAAAAAAAA"));
        assert!(!out.contains("BBBBBBBBBBBBBBBB"));
    }

    #[test]
    fn redact_leaves_short_sk_dash_strings_alone() {
        // "sk-" followed by fewer than 15 key chars isn't a real key; don't
        // mangle legitimate prose that happens to contain "sk-".
        let body = "see sk-foo bar";
        let out = redact_for_display(body);
        assert_eq!(out, "see sk-foo bar");
    }

    #[test]
    fn redact_masks_key_straddling_truncation_boundary() {
        let body = format!("{}sk-{}", "x".repeat(483), "A".repeat(40));
        let leaked_prefix = format!("sk-{}", "A".repeat(14));
        let out = redact_for_display(&body);
        assert!(
            !out.contains(&leaked_prefix),
            "key prefix survived truncation: {out}"
        );
        assert!(out.contains("REDACTED"), "expected redaction marker: {out}");
    }

    #[test]
    fn redact_skips_long_key_without_losing_displayable_tail() {
        let body = format!("sk-{} tail", "A".repeat(1_000_000));
        assert_eq!(redact_for_display(&body), "sk-...REDACTED tail");
    }

    #[test]
    fn redact_truncates_long_bodies_with_length_suffix() {
        let body: String = "x".repeat(800);
        let out = redact_for_display(&body);
        assert!(out.starts_with(&"x".repeat(500)));
        assert!(out.contains("truncated"));
        assert!(out.contains("800"));
        assert!(out.len() < body.len());
    }

    #[test]
    fn redact_masks_anthropic_oauth_token() {
        // A reflected Anthropic OAuth bearer is sk-ant-oat01-... shaped and
        // must never survive into the error string.
        let body = format!(
            "{{\"error\":\"bad token sk-{}\"}}",
            "ant-oat01-AbCdEf0123456789xyz used"
        );
        let out = redact_for_display(&body);
        assert!(!out.contains("AbCdEf0123456789xyz"), "token leaked: {out}");
        assert!(
            out.contains("sk-...REDACTED"),
            "expected redaction marker: {out}"
        );
    }

    #[test]
    fn redact_passes_short_non_key_sk_prefix() {
        // "sk-" not followed by 15+ key chars is ordinary text, not a secret.
        let body = "the sk-1 ticket is unrelated";
        assert_eq!(redact_for_display(body), body);
    }

    #[test]
    fn redact_truncates_overlong_body() {
        let body = "x".repeat(600);
        let out = redact_for_display(&body);
        assert!(out.contains("[truncated, 600 bytes]"), "got: {out}");
        assert!(out.chars().count() < 600, "should be shortened");
    }

    // --- redact_secrets: unbounded variant and Bearer rule ---

    #[test]
    fn redact_secrets_masks_bearer_token() {
        let token = format!("eyJ{}.payload.sig", "a".repeat(30));
        let text = format!("authorization: Bearer {token} next");
        let out = redact_secrets(&text);
        assert_eq!(out, "authorization: Bearer ...REDACTED next");
        assert!(!out.contains(&token));
    }

    #[test]
    fn redact_secrets_keeps_short_bearer_words() {
        let text = "Bearer token missing";
        assert_eq!(redact_secrets(text), text);
    }

    #[test]
    fn redact_secrets_is_unbounded() {
        let key = format!("sk-{}", "B".repeat(40));
        let text = format!("{}{key} tail", "x".repeat(2_000));
        let out = redact_secrets(&text);
        assert!(!out.contains(&key));
        assert!(out.ends_with("sk-...REDACTED tail"));
        assert!(!out.contains("truncated"));
    }

    #[test]
    fn redact_secrets_masks_adjacent_patterns_in_order() {
        let key = format!("sk-proj-{}", "C".repeat(30));
        let bearer = "D".repeat(25);
        let text = format!("a {key} b Bearer {bearer} c");
        assert_eq!(
            redact_secrets(&text),
            "a sk-...REDACTED b Bearer ...REDACTED c"
        );
    }
}
