//! Minimal style-string parser: turns a cship-like style spec
//! ("bold fg:#7aa2f7 bg:#1a1b26 italic underline") into a 24-bit ANSI escape
//! prefix + reset, applied around a segment's text. Unknown tokens and invalid
//! hex are ignored (forward-compatible), never an error.

/// Wrap `text` in the ANSI escapes described by `spec`. A blank spec (or one
/// with no recognized tokens) returns `text` unchanged - no escapes.
pub fn apply_style(spec: &str, text: &str) -> String {
    let codes = ansi_codes(spec);
    if codes.is_empty() {
        return text.to_string();
    }
    format!("\x1b[{}m{}\x1b[0m", codes.join(";"), text)
}

/// Parse a style spec into ANSI SGR parameter fragments (no escape framing).
/// Exposed for unit testing.
pub fn ansi_codes(spec: &str) -> Vec<String> {
    let mut codes = Vec::new();
    for tok in spec.split_whitespace() {
        match tok {
            "bold" => codes.push("1".to_string()),
            "italic" => codes.push("3".to_string()),
            "underline" => codes.push("4".to_string()),
            _ => {
                if let Some(hex) = tok.strip_prefix("fg:#") {
                    if let Some((r, g, b)) = parse_hex(hex) {
                        codes.push(format!("38;2;{r};{g};{b}"));
                    }
                } else if let Some(hex) = tok.strip_prefix("bg:#") {
                    if let Some((r, g, b)) = parse_hex(hex) {
                        codes.push(format!("48;2;{r};{g};{b}"));
                    }
                }
                // Unrecognized token: ignore (forward-compat).
            }
        }
    }
    codes
}

fn parse_hex(hex: &str) -> Option<(u8, u8, u8)> {
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_spec_returns_text_unchanged() {
        assert_eq!(apply_style("", "x"), "x");
        assert_eq!(apply_style("   ", "x"), "x");
    }

    #[test]
    fn bold_and_fg_combine_in_order() {
        assert_eq!(ansi_codes("bold fg:#7aa2f7"), vec!["1", "38;2;122;162;247"]);
        assert_eq!(
            apply_style("bold fg:#7aa2f7", "x"),
            "\x1b[1;38;2;122;162;247mx\x1b[0m"
        );
    }

    #[test]
    fn bg_and_attrs_parse() {
        assert_eq!(
            ansi_codes("italic underline bg:#1a1b26"),
            vec!["3", "4", "48;2;26;27;38"]
        );
    }

    #[test]
    fn invalid_hex_and_unknown_tokens_ignored() {
        assert!(ansi_codes("fg:#zzzzzz").is_empty());
        assert!(ansi_codes("fg:#abc").is_empty()); // wrong length
        assert!(ansi_codes("sparkle wobble").is_empty());
        assert_eq!(ansi_codes("bogus bold"), vec!["1"]);
    }
}
