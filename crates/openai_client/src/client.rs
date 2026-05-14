use std::collections::HashMap;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use reqwest::{Client, StatusCode};
use serde::{de, Deserialize, Deserializer};
use tracing::debug;

use crate::types::{LineItemCost, OpenAiCosts, OpenAiError};

#[derive(Debug, Deserialize)]
struct RawPage {
    #[serde(default)]
    data: Vec<RawBucket>,
    #[serde(default)]
    has_more: bool,
}

#[derive(Debug, Deserialize)]
struct RawBucket {
    #[serde(default)]
    results: Vec<RawResult>,
}

#[derive(Debug, Deserialize)]
struct RawResult {
    amount: Option<RawAmount>,
    #[serde(default)]
    line_item: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawAmount {
    /// OpenAI serializes amounts as high-precision strings (e.g.
    /// `"0.02100000000000000000000000000000000"`) to avoid float-precision
    /// loss over the wire. Older endpoints and most docs say "number" so
    /// we accept either shape defensively.
    #[serde(deserialize_with = "deserialize_amount_value")]
    value: f64,
}

fn deserialize_amount_value<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let raw = serde_json::Value::deserialize(d)?;
    match raw {
        serde_json::Value::Number(n) => n
            .as_f64()
            .ok_or_else(|| de::Error::custom("amount.value: number was not representable as f64")),
        serde_json::Value::String(s) => s.trim().parse::<f64>().map_err(|e| {
            de::Error::custom(format!(
                "amount.value: string {s:?} could not parse as f64: {e}"
            ))
        }),
        other => Err(de::Error::custom(format!(
            "amount.value: expected number or string, got {other:?}"
        ))),
    }
}

/// Convenience wrapper: query spend from the first of the current calendar
/// month (00:00 UTC) through now, daily buckets, grouped by line item.
///
/// This is the default tile in the CLI; callers wanting different windows
/// or grouping should use `fetch_costs` directly.
pub async fn costs_this_month(
    client: &Client,
    base_url: &str,
    admin_key: &str,
) -> Result<OpenAiCosts, OpenAiError> {
    let now = Utc::now();
    let month_start = first_of_month(now);
    fetch_costs(client, base_url, admin_key, month_start, Some(now)).await
}

/// Fetch costs over a [start_time, end_time) window with daily buckets and
/// `line_item` grouping. `end_time` defaults to "now" when None.
pub async fn fetch_costs(
    client: &Client,
    base_url: &str,
    admin_key: &str,
    start_time: DateTime<Utc>,
    end_time: Option<DateTime<Utc>>,
) -> Result<OpenAiCosts, OpenAiError> {
    let url = format!("{}/v1/organization/costs", base_url.trim_end_matches('/'));
    let actual_end = end_time.unwrap_or_else(Utc::now);

    let mut req = client
        .get(&url)
        .header("Authorization", format!("Bearer {admin_key}"))
        .header("Accept", "application/json")
        .query(&[
            ("start_time", start_time.timestamp().to_string()),
            ("end_time", actual_end.timestamp().to_string()),
            ("bucket_width", "1d".to_string()),
            ("limit", "31".to_string()),
        ]);
    // group_by takes an array param; reqwest's .query() handles the
    // `group_by[]=line_item` form when passed a Vec of (key, value) tuples.
    req = req.query(&[("group_by[]", "line_item")]);

    let resp = req.send().await?;
    let status = resp.status();
    let body = resp.text().await?;

    match status {
        StatusCode::OK => {}
        StatusCode::UNAUTHORIZED => {
            return Err(OpenAiError::AuthInvalid {
                body: redact_for_display(&body),
            });
        }
        StatusCode::FORBIDDEN => {
            return Err(OpenAiError::InsufficientScope {
                body: redact_for_display(&body),
            });
        }
        _ => {
            return Err(OpenAiError::UnexpectedStatus {
                status: status.as_u16(),
                body: redact_for_display(&body),
            });
        }
    }

    parse_response(&body, start_time, actual_end, Utc::now())
}

/// Sanitize an HTTP error body before it ends up in an `OpenAiError`'s
/// `Display` impl, which the CLI prints to stdout on the unhappy path.
///
/// Two protections, both defensive (OpenAI's current 4xx/5xx bodies don't
/// echo headers, but the contract isn't a guarantee):
///   1. Anything matching `sk-` followed by 15+ key-shaped characters is
///      replaced with `sk-…REDACTED`.
///   2. Bodies longer than 500 chars are truncated with a length-suffix.
fn redact_for_display(body: &str) -> String {
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
            // Not key-shaped; emit verbatim and continue.
            out.push_str("sk-");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn parse_response(
    body: &str,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    fetched_at: DateTime<Utc>,
) -> Result<OpenAiCosts, OpenAiError> {
    let page: RawPage = serde_json::from_str(body)
        .map_err(|e| OpenAiError::ResponseShape(format!("invalid JSON: {e}")))?;

    let mut total_usd = 0.0f64;
    let mut by_line: HashMap<String, f64> = HashMap::new();

    for bucket in &page.data {
        for result in &bucket.results {
            let Some(amount) = &result.amount else {
                continue;
            };
            let label = result
                .line_item
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown")
                .to_string();
            total_usd += amount.value;
            *by_line.entry(label).or_insert(0.0) += amount.value;
        }
    }

    let mut by_line_item: Vec<LineItemCost> = by_line
        .into_iter()
        .map(|(line_item, amount_usd)| LineItemCost {
            line_item,
            amount_usd,
        })
        .collect();
    by_line_item.sort_by(|a, b| {
        b.amount_usd
            .partial_cmp(&a.amount_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.line_item.cmp(&b.line_item))
    });

    debug!(
        buckets = page.data.len(),
        total_usd, has_more = page.has_more, "costs: parsed"
    );

    Ok(OpenAiCosts {
        start_time,
        end_time,
        total_usd,
        by_line_item,
        truncated: page.has_more,
        fetched_at,
    })
}

/// 00:00 UTC on the first of the current month for the given `now`.
fn first_of_month(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .expect("constructing first-of-month always succeeds for a Utc now")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_window() -> (DateTime<Utc>, DateTime<Utc>) {
        let start = DateTime::parse_from_rfc3339("2026-05-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end = DateTime::parse_from_rfc3339("2026-05-13T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        (start, end)
    }

    #[test]
    fn parses_typical_response_with_buckets() {
        let body = r#"{
            "object": "page",
            "data": [
                {
                    "object": "bucket",
                    "start_time": 1746057600,
                    "end_time": 1746144000,
                    "results": [
                        {
                            "object": "organization.costs.result",
                            "amount": {"value": 0.42, "currency": "usd"},
                            "line_item": "gpt-5"
                        },
                        {
                            "object": "organization.costs.result",
                            "amount": {"value": 0.08, "currency": "usd"},
                            "line_item": "o1-mini"
                        }
                    ]
                },
                {
                    "object": "bucket",
                    "start_time": 1746144000,
                    "end_time": 1746230400,
                    "results": [
                        {
                            "object": "organization.costs.result",
                            "amount": {"value": 1.23, "currency": "usd"},
                            "line_item": "gpt-5"
                        }
                    ]
                }
            ],
            "has_more": false
        }"#;
        let (start, end) = fixed_window();
        let parsed = parse_response(body, start, end, Utc::now()).expect("parse");
        assert!((parsed.total_usd - 1.73).abs() < 1e-9);
        assert!(!parsed.truncated);
        // Two distinct line items, sorted by amount desc: gpt-5 (1.65), o1-mini (0.08)
        assert_eq!(parsed.by_line_item.len(), 2);
        assert_eq!(parsed.by_line_item[0].line_item, "gpt-5");
        assert!((parsed.by_line_item[0].amount_usd - 1.65).abs() < 1e-9);
        assert_eq!(parsed.by_line_item[1].line_item, "o1-mini");
    }

    #[test]
    fn empty_data_array_yields_zero_total() {
        let body = r#"{"object":"page","data":[],"has_more":false}"#;
        let (start, end) = fixed_window();
        let parsed = parse_response(body, start, end, Utc::now()).expect("parse");
        assert_eq!(parsed.total_usd, 0.0);
        assert!(parsed.by_line_item.is_empty());
        assert!(!parsed.truncated);
    }

    #[test]
    fn bucket_with_empty_results_is_fine() {
        let body = r#"{
            "object": "page",
            "data": [{"object":"bucket","start_time":1,"end_time":2,"results":[]}],
            "has_more": false
        }"#;
        let (start, end) = fixed_window();
        let parsed = parse_response(body, start, end, Utc::now()).expect("parse");
        assert_eq!(parsed.total_usd, 0.0);
    }

    #[test]
    fn null_line_item_is_labeled_unknown() {
        let body = r#"{
            "object": "page",
            "data": [{"object":"bucket","start_time":1,"end_time":2,"results":[
                {"object":"organization.costs.result","amount":{"value":0.5,"currency":"usd"},"line_item":null}
            ]}],
            "has_more": false
        }"#;
        let (start, end) = fixed_window();
        let parsed = parse_response(body, start, end, Utc::now()).expect("parse");
        assert_eq!(parsed.by_line_item.len(), 1);
        assert_eq!(parsed.by_line_item[0].line_item, "unknown");
        assert!((parsed.by_line_item[0].amount_usd - 0.5).abs() < 1e-9);
    }

    #[test]
    fn missing_amount_is_skipped_not_errored() {
        // OpenAI sometimes returns results with metadata but no amount when
        // grouping interacts oddly with empty windows. Tolerate it.
        let body = r#"{
            "object": "page",
            "data": [{"object":"bucket","start_time":1,"end_time":2,"results":[
                {"object":"organization.costs.result","line_item":"gpt-5"},
                {"object":"organization.costs.result","amount":{"value":0.1,"currency":"usd"},"line_item":"gpt-5"}
            ]}],
            "has_more": false
        }"#;
        let (start, end) = fixed_window();
        let parsed = parse_response(body, start, end, Utc::now()).expect("parse");
        assert_eq!(parsed.by_line_item.len(), 1);
        assert!((parsed.by_line_item[0].amount_usd - 0.1).abs() < 1e-9);
    }

    #[test]
    fn line_items_aggregate_across_buckets() {
        // Same line item appearing in multiple buckets must sum.
        let body = r#"{
            "object": "page",
            "data": [
                {"object":"bucket","start_time":1,"end_time":2,"results":[
                    {"object":"organization.costs.result","amount":{"value":0.10,"currency":"usd"},"line_item":"gpt-5"}
                ]},
                {"object":"bucket","start_time":2,"end_time":3,"results":[
                    {"object":"organization.costs.result","amount":{"value":0.20,"currency":"usd"},"line_item":"gpt-5"}
                ]},
                {"object":"bucket","start_time":3,"end_time":4,"results":[
                    {"object":"organization.costs.result","amount":{"value":0.05,"currency":"usd"},"line_item":"gpt-5"}
                ]}
            ],
            "has_more": false
        }"#;
        let (start, end) = fixed_window();
        let parsed = parse_response(body, start, end, Utc::now()).expect("parse");
        assert_eq!(parsed.by_line_item.len(), 1);
        assert_eq!(parsed.by_line_item[0].line_item, "gpt-5");
        assert!((parsed.by_line_item[0].amount_usd - 0.35).abs() < 1e-9);
        assert!((parsed.total_usd - 0.35).abs() < 1e-9);
    }

    #[test]
    fn has_more_true_sets_truncated_flag() {
        let body = r#"{
            "object": "page",
            "data": [{"object":"bucket","start_time":1,"end_time":2,"results":[
                {"object":"organization.costs.result","amount":{"value":1.0,"currency":"usd"},"line_item":"gpt-5"}
            ]}],
            "has_more": true,
            "next_page": "page_abc"
        }"#;
        let (start, end) = fixed_window();
        let parsed = parse_response(body, start, end, Utc::now()).expect("parse");
        assert!(parsed.truncated);
        // Total is still computed from what we did get — it's just flagged partial.
        assert!((parsed.total_usd - 1.0).abs() < 1e-9);
    }

    #[test]
    fn amount_value_as_high_precision_string_is_accepted() {
        // The real OpenAI Costs API serializes amount.value as a high-precision
        // string ("0.02100000000000000000000000000000000") rather than a JSON
        // number, presumably to avoid float-roundtrip loss. We have to tolerate
        // both. Regression: prior versions of this crate assumed f64.
        let body = r#"{
            "object": "page",
            "data": [{"object":"bucket","start_time":1,"end_time":2,"results":[
                {"object":"organization.costs.result","amount":{"value":"0.02100000000000000000000000000000000","currency":"usd"},"line_item":"gpt-5"},
                {"object":"organization.costs.result","amount":{"value":"1.50000000000000000000000000000000000","currency":"usd"},"line_item":"o1-mini"}
            ]}],
            "has_more": false
        }"#;
        let (start, end) = fixed_window();
        let parsed = parse_response(body, start, end, Utc::now()).expect("parse");
        // 0.021 + 1.5 = 1.521; check within float epsilon.
        assert!((parsed.total_usd - 1.521).abs() < 1e-9, "got {}", parsed.total_usd);
        assert_eq!(parsed.by_line_item.len(), 2);
        // o1-mini has the higher amount (1.50), comes first.
        assert_eq!(parsed.by_line_item[0].line_item, "o1-mini");
        assert!((parsed.by_line_item[0].amount_usd - 1.5).abs() < 1e-9);
        assert_eq!(parsed.by_line_item[1].line_item, "gpt-5");
        assert!((parsed.by_line_item[1].amount_usd - 0.021).abs() < 1e-9);
    }

    #[test]
    fn amount_value_as_invalid_string_is_shape_error() {
        let body = r#"{
            "object": "page",
            "data": [{"object":"bucket","start_time":1,"end_time":2,"results":[
                {"object":"organization.costs.result","amount":{"value":"not a number","currency":"usd"},"line_item":"gpt-5"}
            ]}],
            "has_more": false
        }"#;
        let (start, end) = fixed_window();
        match parse_response(body, start, end, Utc::now()) {
            Err(OpenAiError::ResponseShape(msg)) => {
                assert!(msg.contains("could not parse"), "msg: {msg}")
            }
            other => panic!("expected ResponseShape, got {other:?}"),
        }
    }

    #[test]
    fn invalid_json_is_shape_error() {
        let (start, end) = fixed_window();
        match parse_response("not json", start, end, Utc::now()) {
            Err(OpenAiError::ResponseShape(msg)) => assert!(msg.contains("invalid JSON")),
            other => panic!("expected ResponseShape, got {other:?}"),
        }
    }

    #[test]
    fn first_of_month_handles_january() {
        let mid_jan = DateTime::parse_from_rfc3339("2026-01-15T12:34:56Z")
            .unwrap()
            .with_timezone(&Utc);
        let fom = first_of_month(mid_jan);
        assert_eq!(fom.to_rfc3339(), "2026-01-01T00:00:00+00:00");
    }

    #[test]
    fn first_of_month_handles_december_to_january_boundary() {
        // We're computing first-of-current-month, not first-of-previous. A
        // December date returns December 1st, not January 1st of next year.
        let mid_dec = DateTime::parse_from_rfc3339("2026-12-31T23:59:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let fom = first_of_month(mid_dec);
        assert_eq!(fom.to_rfc3339(), "2026-12-01T00:00:00+00:00");
    }

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
        let body = "auth failed for sk-admin-AbCdEfGhIjKlMnOpQrStUvWxYz1234567890";
        let out = redact_for_display(body);
        assert!(
            out.contains("sk-…REDACTED"),
            "expected redaction marker; got: {out}"
        );
        assert!(
            !out.contains("AbCdEfGhIjKlMn"),
            "key body must not survive: {out}"
        );
    }

    #[test]
    fn redact_replaces_multiple_keys_in_one_body() {
        let body = "key1 sk-admin-AAAAAAAAAAAAAAAA mid sk-proj-BBBBBBBBBBBBBBBB tail";
        let out = redact_for_display(body);
        assert_eq!(out.matches("sk-…REDACTED").count(), 2);
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
    fn redact_truncates_long_bodies_with_length_suffix() {
        let body: String = "x".repeat(800);
        let out = redact_for_display(&body);
        assert!(out.starts_with(&"x".repeat(500)));
        assert!(out.contains("truncated"));
        assert!(out.contains("800"));
        assert!(out.len() < body.len());
    }
}
