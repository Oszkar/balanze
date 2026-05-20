use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, warn};

use crate::types::{CadenceBar, ClaudeOAuthSnapshot, ExtraUsage, OAuthError};

const BETA_HEADER: &str = "oauth-2025-04-20";

/// Parse a `Retry-After` header into a `Duration`, supporting both forms
/// allowed by RFC 7231 §7.1.3: delta-seconds (`"30"`) and HTTP-date
/// (`"Sun, 06 Nov 1994 08:49:37 GMT"`). HTTP-date values in the past
/// clamp to zero (treated as "retry immediately") so a stale server clock
/// can't park the retry indefinitely.
///
/// `now` is injected for testability; live callers pass `Utc::now()`.
pub(crate) fn parse_retry_after_at(
    headers: &reqwest::header::HeaderMap,
    now: DateTime<Utc>,
) -> Option<std::time::Duration> {
    let raw = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())?
        .trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(std::time::Duration::from_secs(secs));
    }
    // RFC 7231: HTTP-date is IMF-fixdate / RFC 5322. chrono's RFC 2822
    // parser accepts the IMF-fixdate shape; the `+0000`/`GMT` zone is
    // tolerated by both forms.
    DateTime::parse_from_rfc2822(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
        .map(|target| {
            let delta = target.signed_duration_since(now);
            std::time::Duration::from_secs(delta.num_seconds().max(0) as u64)
        })
}

/// Convenience wrapper: uses `Utc::now()` for the HTTP-date delta. Live
/// callers go through this; tests use `parse_retry_after_at` directly.
pub(crate) fn parse_retry_after(
    headers: &reqwest::header::HeaderMap,
) -> Option<std::time::Duration> {
    parse_retry_after_at(headers, Utc::now())
}

#[derive(Debug, Deserialize)]
struct RawCadence {
    utilization: f32,
    resets_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct RawExtraUsage {
    is_enabled: bool,
    monthly_limit: f64,
    used_credits: f64,
    utilization: f32,
    currency: String,
}

/// Call `GET {base_url}/api/oauth/usage` with the given bearer token.
///
/// `base_url` is the API root (typically `https://api.anthropic.com`). Tests
/// override it to point at a wiremock instance.
///
/// `subscription_type` and `rate_limit_tier` flow in from credentials.json
/// (the endpoint itself doesn't echo them, but they're useful to plumb into
/// the snapshot for display).
///
/// Returns `OAuthError::AuthExpired` on HTTP 401 (caller decides whether to
/// attempt a token refresh). Unknown future cadence keys are preserved
/// verbatim in the response so newly-added Anthropic meters render with a
/// titlecased fallback label.
///
/// `policy` controls backoff+retry for transient errors (429, 5xx, network).
/// Pass `BackoffPolicy::fail_fast()` from one-shot CLI callers so the user is
/// never blocked for minutes. The future background watcher will pass
/// `BackoffPolicy::standard()`.
pub async fn fetch_usage(
    client: &Client,
    base_url: &str,
    access_token: &str,
    subscription_type: Option<String>,
    rate_limit_tier: Option<String>,
    policy: &backoff::BackoffPolicy,
) -> Result<ClaudeOAuthSnapshot, OAuthError> {
    let url = format!("{}/api/oauth/usage", base_url.trim_end_matches('/'));

    let classify = |e: &OAuthError| match e {
        OAuthError::RateLimited { retry_after } => backoff::RetryDecision::RetryAfter(*retry_after),
        OAuthError::Network(_) => backoff::RetryDecision::RetryAfter(None),
        OAuthError::UnexpectedStatus { status, .. } if (500..=599).contains(status) => {
            backoff::RetryDecision::RetryAfter(None)
        }
        // AuthExpired / RefreshFailed / ResponseShape / CredentialsMissing / etc.
        // must NOT be retried — especially AuthExpired, which triggers the
        // caller's refresh+retry-once path (Track A).
        _ => backoff::RetryDecision::DoNotRetry,
    };

    backoff::retry(policy, classify, || async {
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("anthropic-beta", BETA_HEADER)
            .header("Accept", "application/json")
            .send()
            .await?;

        let status = resp.status();
        let org_uuid = resp
            .headers()
            .get("anthropic-organization-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        // Read Retry-After BEFORE consuming the body (headers are unavailable
        // after `resp.text()` takes ownership).
        let retry_after = parse_retry_after(resp.headers());

        let body = resp.text().await?;

        match status {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED => return Err(OAuthError::AuthExpired),
            StatusCode::TOO_MANY_REQUESTS => {
                return Err(OAuthError::RateLimited { retry_after });
            }
            _ => {
                return Err(OAuthError::UnexpectedStatus {
                    status: status.as_u16(),
                    body: redact_for_display(&body),
                });
            }
        }

        parse_response(
            &body,
            org_uuid,
            subscription_type.clone(),
            rate_limit_tier.clone(),
            Utc::now(),
        )
    })
    .await
}

/// Redact secret-shaped substrings before a response body is surfaced via
/// `OAuthError::UnexpectedStatus` (whose `Display` the CLI prints and logs).
/// Deliberately mirrors `openai_client::redact_for_display`: the two HTTP
/// clients are the only crates that touch provider response bodies
/// (AGENTS.md §4 #3), and a shared util crate for exactly two callers would
/// violate YAGNI (§2). The `sk-` rule also covers Anthropic OAuth tokens,
/// which are `sk-ant-oat01-…` / `sk-ant-ort01-…` shaped, so a reflected
/// bearer cannot leak into the error string.
pub(crate) fn redact_for_display(body: &str) -> String {
    const MAX_LEN: usize = 500;
    let truncated: String = if body.chars().count() > MAX_LEN {
        let head: String = body.chars().take(MAX_LEN).collect();
        format!("{head}…[truncated, {} bytes]", body.len())
    } else {
        body.to_string()
    };

    let mut out = String::with_capacity(truncated.len());
    let mut rest = truncated.as_str();
    while let Some(idx) = rest.find("sk-") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 3..];
        let key_len = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(after.len());
        if key_len >= 15 {
            out.push_str("sk-…REDACTED");
            rest = &after[key_len..];
        } else {
            // Not key-shaped; emit the literal "sk-" and continue scanning.
            out.push_str("sk-");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn parse_response(
    body: &str,
    org_uuid: Option<String>,
    subscription_type: Option<String>,
    rate_limit_tier: Option<String>,
    fetched_at: DateTime<Utc>,
) -> Result<ClaudeOAuthSnapshot, OAuthError> {
    let json: Value = serde_json::from_str(body)
        .map_err(|e| OAuthError::ResponseShape(format!("invalid JSON: {e}")))?;
    let obj = json
        .as_object()
        .ok_or_else(|| OAuthError::ResponseShape("response root is not an object".into()))?;

    let mut cadences = Vec::new();
    let mut extra_usage = None;

    for (key, value) in obj {
        if value.is_null() {
            continue;
        }
        if key == "extra_usage" {
            match serde_json::from_value::<RawExtraUsage>(value.clone()) {
                Ok(raw) => {
                    // Raw values are integer CENTS. Resolved first-hand by
                    // the 2026-05-19 reconciliation spike (Max-5x: OAuth
                    // 2500/2092 ↔ claude.ai "Extra usage" $25.00/$20.92/84%);
                    // see anthropic_oauth/src/types.rs ExtraUsage doc.
                    // Convert cents → micro-USD via × 10_000.
                    extra_usage = Some(ExtraUsage {
                        is_enabled: raw.is_enabled,
                        monthly_limit_micro_usd: (raw.monthly_limit * 10_000.0).round() as i64,
                        used_credits_micro_usd: (raw.used_credits * 10_000.0).round() as i64,
                        utilization_percent: raw.utilization,
                        currency: raw.currency,
                    });
                }
                Err(e) => {
                    // Log only serde's error *category*, never the Display
                    // string: for a type-mismatch the message can quote the
                    // offending value, and `extra_usage` carries the user's
                    // billing figures (monthly_limit / used_credits), which
                    // §3.4 treats as sensitive — never logged at any level.
                    warn!(
                        "oauth/usage: failed to parse extra_usage block \
                         (serde category: {:?}; raw values suppressed)",
                        e.classify()
                    );
                }
            }
            continue;
        }
        match serde_json::from_value::<RawCadence>(value.clone()) {
            Ok(raw) => {
                cadences.push(CadenceBar {
                    key: key.clone(),
                    display_label: cadence_label(key),
                    utilization_percent: raw.utilization,
                    resets_at: raw.resets_at,
                });
            }
            Err(e) => {
                debug!("oauth/usage: ignoring unexpected-shape field {key}: {e}");
            }
        }
    }

    cadences.sort_by(|a, b| {
        cadence_sort_key(&a.key)
            .cmp(&cadence_sort_key(&b.key))
            .then_with(|| a.key.cmp(&b.key))
    });

    Ok(ClaudeOAuthSnapshot {
        cadences,
        extra_usage,
        subscription_type,
        rate_limit_tier,
        org_uuid,
        fetched_at,
    })
}

fn cadence_label(key: &str) -> String {
    match key {
        "five_hour" => "Current 5-hour session".to_string(),
        "seven_day" => "All models (7 days)".to_string(),
        "seven_day_opus" => "Opus only (7 days)".to_string(),
        "seven_day_sonnet" => "Sonnet only (7 days)".to_string(),
        "seven_day_oauth_apps" => "OAuth apps (7 days)".to_string(),
        "seven_day_cowork" => "Cowork (7 days)".to_string(),
        "seven_day_omelette" => "Omelette (7 days)".to_string(),
        "tangelo" => "Tangelo".to_string(),
        "iguana_necktie" => "Iguana Necktie".to_string(),
        "omelette_promotional" => "Omelette Promotional".to_string(),
        other => titlecase_key(other),
    }
}

fn cadence_sort_key(key: &str) -> u8 {
    match key {
        "five_hour" => 0,
        "seven_day" => 1,
        "seven_day_sonnet" => 2,
        "seven_day_opus" => 3,
        "seven_day_oauth_apps" => 4,
        "seven_day_cowork" => 5,
        "seven_day_omelette" => 6,
        "tangelo" => 7,
        "iguana_necktie" => 8,
        "omelette_promotional" => 9,
        _ => 100,
    }
}

fn titlecase_key(key: &str) -> String {
    key.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().chain(chars).collect(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_ts() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-13T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn parses_typical_max_user_response() {
        // Synthetic shape based on the real Claude Max response (May 2026).
        // Values changed to avoid embedding personal usage in source.
        let body = r#"{
            "five_hour":  {"utilization": 23.0, "resets_at": "2026-05-13T18:00:00+00:00"},
            "seven_day":  {"utilization": 58.0, "resets_at": "2026-05-15T14:00:00+00:00"},
            "seven_day_oauth_apps": null,
            "seven_day_opus": null,
            "seven_day_sonnet": {"utilization": 11.0, "resets_at": "2026-05-15T14:00:00+00:00"},
            "seven_day_cowork": null,
            "seven_day_omelette": {"utilization": 93.0, "resets_at": "2026-05-15T14:00:00+00:00"},
            "tangelo": null,
            "iguana_necktie": null,
            "omelette_promotional": null,
            "extra_usage": {"is_enabled": true, "monthly_limit": 5000, "used_credits": 1234, "utilization": 24.68, "currency": "USD"}
        }"#;
        let snapshot = parse_response(
            body,
            Some("317afabc-aaaa".into()),
            Some("max".into()),
            Some("default_claude_max_5x".into()),
            fixed_ts(),
        )
        .unwrap();

        assert_eq!(snapshot.cadences.len(), 4);
        // Ordered: five_hour, seven_day, seven_day_sonnet, seven_day_omelette
        assert_eq!(snapshot.cadences[0].key, "five_hour");
        assert_eq!(snapshot.cadences[0].display_label, "Current 5-hour session");
        assert!((snapshot.cadences[0].utilization_percent - 23.0).abs() < 1e-5);

        assert_eq!(snapshot.cadences[1].key, "seven_day");
        assert_eq!(snapshot.cadences[2].key, "seven_day_sonnet");
        assert_eq!(snapshot.cadences[3].key, "seven_day_omelette");

        let extra = snapshot.extra_usage.expect("extra_usage present");
        assert!(extra.is_enabled);
        // Anthropic returns values in cents: 5000 cents = $50.00 = 50_000_000 micro-USD
        assert_eq!(extra.monthly_limit_micro_usd, 50_000_000);
        // 1234 cents = $12.34 = 12_340_000 micro-USD
        assert_eq!(extra.used_credits_micro_usd, 12_340_000);
        assert_eq!(extra.currency, "USD");

        assert_eq!(snapshot.subscription_type.as_deref(), Some("max"));
        assert_eq!(snapshot.org_uuid.as_deref(), Some("317afabc-aaaa"));
    }

    #[test]
    fn extra_usage_reconciled_cents_semantic() {
        // Regression pin for the 2026-05-19 reconciliation spike
        // (~/.gstack/projects/balanze/spike-extra-usage-reconciliation-20260519.md).
        // claude.ai/settings/usage "Extra usage" showed $20.92 / $25.00 / 84%
        // for a Max-5x account; OAuth returned monthly_limit=2500,
        // used_credits=2092, utilization=83.7. Raw ints are CENTS. This test
        // fails if anyone reverts the cents (× 10_000) interpretation.
        let body = r#"{
            "five_hour": {"utilization": 10.0, "resets_at": "2026-05-19T18:00:00Z"},
            "extra_usage": {"is_enabled": true, "monthly_limit": 2500, "used_credits": 2092, "utilization": 83.7, "currency": "USD"}
        }"#;
        let snap = parse_response(body, None, Some("max".into()), None, fixed_ts()).unwrap();
        let eu = snap.extra_usage.expect("extra_usage present");
        assert!(eu.is_enabled);
        // 2500 cents = $25.00 = 25_000_000 micro-USD
        assert_eq!(eu.monthly_limit_micro_usd, 25_000_000);
        // 2092 cents = $20.92 = 20_920_000 micro-USD
        assert_eq!(eu.used_credits_micro_usd, 20_920_000);
        assert!((eu.utilization_percent - 83.7).abs() < 1e-3);
        assert_eq!(eu.currency, "USD");
    }

    #[test]
    fn null_cadences_are_skipped_not_errored() {
        let body = r#"{
            "five_hour": {"utilization": 5.0, "resets_at": "2026-05-13T18:00:00Z"},
            "seven_day": null,
            "seven_day_opus": null,
            "extra_usage": null
        }"#;
        let snapshot = parse_response(body, None, None, None, fixed_ts()).unwrap();
        assert_eq!(snapshot.cadences.len(), 1);
        assert_eq!(snapshot.cadences[0].key, "five_hour");
        assert!(snapshot.extra_usage.is_none());
    }

    #[test]
    fn empty_response_is_ok_with_empty_cadences() {
        let snapshot = parse_response("{}", None, None, None, fixed_ts()).unwrap();
        assert!(snapshot.cadences.is_empty());
        assert!(snapshot.extra_usage.is_none());
    }

    #[test]
    fn unknown_cadence_key_renders_with_titlecased_fallback() {
        // Anthropic could add new meters at any time. We must preserve them.
        let body = r#"{
            "monthly_phoenix": {"utilization": 12.0, "resets_at": "2026-06-01T00:00:00Z"}
        }"#;
        let snapshot = parse_response(body, None, None, None, fixed_ts()).unwrap();
        assert_eq!(snapshot.cadences.len(), 1);
        assert_eq!(snapshot.cadences[0].key, "monthly_phoenix");
        assert_eq!(snapshot.cadences[0].display_label, "Monthly Phoenix");
    }

    #[test]
    fn unknown_keys_sort_after_known_keys() {
        let body = r#"{
            "monthly_phoenix": {"utilization": 1.0, "resets_at": "2026-06-01T00:00:00Z"},
            "five_hour":       {"utilization": 2.0, "resets_at": "2026-05-13T18:00:00Z"},
            "seven_day":       {"utilization": 3.0, "resets_at": "2026-05-15T14:00:00Z"}
        }"#;
        let snapshot = parse_response(body, None, None, None, fixed_ts()).unwrap();
        let keys: Vec<_> = snapshot.cadences.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, vec!["five_hour", "seven_day", "monthly_phoenix"]);
    }

    #[test]
    fn malformed_cadence_value_is_logged_not_fatal() {
        // If Anthropic emits a known key with a new shape we don't know, we
        // skip it rather than failing the whole snapshot. Other cadences still
        // render. (The warn-log call exercises the debug! branch.)
        let body = r#"{
            "five_hour": {"utilization": 5.0, "resets_at": "2026-05-13T18:00:00Z"},
            "seven_day_sonnet": {"unexpected_field": 1}
        }"#;
        let snapshot = parse_response(body, None, None, None, fixed_ts()).unwrap();
        assert_eq!(snapshot.cadences.len(), 1);
        assert_eq!(snapshot.cadences[0].key, "five_hour");
    }

    #[test]
    fn invalid_json_is_response_shape_error() {
        let body = "not json";
        match parse_response(body, None, None, None, fixed_ts()) {
            Err(OAuthError::ResponseShape(msg)) => assert!(msg.contains("invalid JSON")),
            other => panic!("expected ResponseShape, got {other:?}"),
        }
    }

    #[test]
    fn non_object_root_is_response_shape_error() {
        let body = "[]";
        match parse_response(body, None, None, None, fixed_ts()) {
            Err(OAuthError::ResponseShape(msg)) => assert!(msg.contains("not an object")),
            other => panic!("expected ResponseShape, got {other:?}"),
        }
    }

    #[test]
    fn titlecase_handles_single_word_and_empty() {
        assert_eq!(titlecase_key("foo"), "Foo");
        assert_eq!(titlecase_key("foo_bar_baz"), "Foo Bar Baz");
        assert_eq!(titlecase_key(""), "");
        assert_eq!(titlecase_key("a_b"), "A B");
    }

    #[test]
    fn redact_masks_anthropic_oauth_token() {
        // A reflected Anthropic OAuth bearer is sk-ant-oat01-… shaped and
        // must never survive into the error string.
        let body = r#"{"error":"bad token sk-ant-oat01-AbCdEf0123456789xyz used"}"#;
        let out = redact_for_display(body);
        assert!(!out.contains("AbCdEf0123456789xyz"), "token leaked: {out}");
        assert!(
            out.contains("sk-…REDACTED"),
            "expected redaction marker: {out}"
        );
    }

    #[test]
    fn redact_passes_short_non_key_sk_prefix() {
        // "sk-" not followed by 15+ key chars is ordinary text, not a secret.
        let body = "the sk-1 ticket is unrelated";
        assert_eq!(redact_for_display(body), body);
    }

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-20T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn parse_retry_after_delta_seconds_form() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        h.insert(reqwest::header::RETRY_AFTER, HeaderValue::from_static("45"));
        assert_eq!(
            parse_retry_after_at(&h, fixed_now()),
            Some(std::time::Duration::from_secs(45))
        );
    }

    #[test]
    fn parse_retry_after_http_date_form() {
        // RFC 7231 §7.1.3 allows IMF-fixdate; `+0000` and `GMT` zone tokens are
        // both accepted by chrono's RFC 2822 parser.
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        h.insert(
            reqwest::header::RETRY_AFTER,
            HeaderValue::from_static("Wed, 20 May 2026 12:00:30 GMT"),
        );
        assert_eq!(
            parse_retry_after_at(&h, fixed_now()),
            Some(std::time::Duration::from_secs(30))
        );
    }

    #[test]
    fn parse_retry_after_past_http_date_clamps_to_zero() {
        // A stale server clock could push the HTTP-date into the past. The
        // retry loop must not interpret that as a huge negative wait or an
        // out-of-range u64 — clamp to zero ("retry immediately").
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        h.insert(
            reqwest::header::RETRY_AFTER,
            HeaderValue::from_static("Wed, 20 May 2026 11:00:00 GMT"),
        );
        assert_eq!(
            parse_retry_after_at(&h, fixed_now()),
            Some(std::time::Duration::from_secs(0))
        );
    }

    #[test]
    fn parse_retry_after_missing_header_is_none() {
        use reqwest::header::HeaderMap;
        let h = HeaderMap::new();
        assert!(parse_retry_after_at(&h, fixed_now()).is_none());
    }

    #[test]
    fn parse_retry_after_garbage_value_is_none() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        h.insert(
            reqwest::header::RETRY_AFTER,
            HeaderValue::from_static("not a value"),
        );
        assert!(parse_retry_after_at(&h, fixed_now()).is_none());
    }

    #[test]
    fn redact_truncates_overlong_body() {
        let body = "x".repeat(600);
        let out = redact_for_display(&body);
        assert!(out.contains("[truncated, 600 bytes]"), "got: {out}");
        assert!(out.chars().count() < 600, "should be shortened");
    }
}
