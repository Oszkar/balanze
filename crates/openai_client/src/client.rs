use chrono::{DateTime, TimeZone, Utc};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

use crate::types::{CreditGrants, Grant, OpenAiError};

#[derive(Debug, Deserialize)]
struct RawGrants {
    #[serde(default)]
    total_granted: f64,
    #[serde(default)]
    total_used: f64,
    #[serde(default)]
    total_available: f64,
    #[serde(default)]
    grants: RawGrantsList,
}

#[derive(Debug, Deserialize, Default)]
struct RawGrantsList {
    #[serde(default)]
    data: Vec<RawGrant>,
}

#[derive(Debug, Deserialize)]
struct RawGrant {
    #[serde(default)]
    grant_amount: f64,
    #[serde(default)]
    used_amount: f64,
    /// Unix seconds since epoch. Some old OpenAI fixtures return a float; we
    /// tolerate either with serde_json::Value parsing in `parse_response`.
    expires_at: Option<Value>,
}

/// Call `GET {base_url}/v1/dashboard/billing/credit_grants`.
///
/// `base_url` exists so wiremock tests can point at a local mock; production
/// callers pass `openai_client::DEFAULT_API_BASE`.
pub async fn fetch_credit_grants(
    client: &Client,
    base_url: &str,
    api_key: &str,
) -> Result<CreditGrants, OpenAiError> {
    let url = format!(
        "{}/v1/dashboard/billing/credit_grants",
        base_url.trim_end_matches('/')
    );
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Accept", "application/json")
        .send()
        .await?;

    let status = resp.status();
    let body = resp.text().await?;

    match status {
        StatusCode::OK => {}
        StatusCode::UNAUTHORIZED => return Err(OpenAiError::AuthExpired { body }),
        StatusCode::FORBIDDEN => return Err(OpenAiError::ForbiddenProjectKey { body }),
        _ => {
            return Err(OpenAiError::UnexpectedStatus {
                status: status.as_u16(),
                body,
            });
        }
    }

    parse_response(&body, Utc::now())
}

fn parse_response(body: &str, fetched_at: DateTime<Utc>) -> Result<CreditGrants, OpenAiError> {
    let raw: RawGrants = serde_json::from_str(body)
        .map_err(|e| OpenAiError::ResponseShape(format!("invalid JSON: {e}")))?;

    let mut grants: Vec<Grant> = Vec::with_capacity(raw.grants.data.len());
    for (idx, raw_grant) in raw.grants.data.iter().enumerate() {
        let expires_at = parse_epoch_seconds(raw_grant.expires_at.as_ref()).ok_or_else(|| {
            OpenAiError::ResponseShape(format!(
                "grant[{idx}] missing or invalid expires_at: {:?}",
                raw_grant.expires_at
            ))
        })?;
        grants.push(Grant {
            grant_amount_usd: raw_grant.grant_amount,
            used_amount_usd: raw_grant.used_amount,
            expires_at,
        });
    }

    let next_grant_expiry = grants
        .iter()
        .map(|g| g.expires_at)
        .filter(|ts| *ts > fetched_at)
        .min();

    debug!(
        granted = raw.total_granted,
        used = raw.total_used,
        available = raw.total_available,
        grants = grants.len(),
        "credit_grants: parsed"
    );

    Ok(CreditGrants {
        total_granted_usd: raw.total_granted,
        total_used_usd: raw.total_used,
        total_available_usd: raw.total_available,
        next_grant_expiry,
        grants,
        fetched_at,
    })
}

fn parse_epoch_seconds(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let secs = match value? {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.parse::<f64>().ok()?,
        _ => return None,
    };
    Utc.timestamp_opt(secs as i64, 0).single()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-13T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn parses_typical_response() {
        let body = r#"{
            "object": "list",
            "total_granted": 25.0,
            "total_used": 18.42,
            "total_available": 6.58,
            "grants": {
                "object": "credit_grant",
                "data": [
                    {"grant_amount": 25.0, "used_amount": 18.42, "expires_at": 1781308800}
                ]
            }
        }"#;
        let parsed = parse_response(body, fixed_now()).expect("parse");
        assert!((parsed.total_granted_usd - 25.0).abs() < 1e-9);
        assert!((parsed.total_used_usd - 18.42).abs() < 1e-9);
        assert!((parsed.total_available_usd - 6.58).abs() < 1e-9);
        assert_eq!(parsed.grants.len(), 1);
        // Future grant → next_grant_expiry is Some.
        assert!(parsed.next_grant_expiry.is_some());
    }

    #[test]
    fn empty_grants_array_is_ok() {
        let body = r#"{
            "total_granted": 0,
            "total_used": 0,
            "total_available": 0,
            "grants": {"data": []}
        }"#;
        let parsed = parse_response(body, fixed_now()).expect("parse");
        assert!(parsed.grants.is_empty());
        assert!(parsed.next_grant_expiry.is_none());
        assert_eq!(parsed.total_granted_usd, 0.0);
    }

    #[test]
    fn missing_grants_field_returns_empty() {
        // serde_default on RawGrantsList covers the case where the field is
        // omitted entirely.
        let body = r#"{"total_granted": 10, "total_used": 5, "total_available": 5}"#;
        let parsed = parse_response(body, fixed_now()).expect("parse");
        assert!(parsed.grants.is_empty());
    }

    #[test]
    fn next_grant_expiry_picks_earliest_future_grant() {
        // 3 grants: one in the past, two in the future. Expect the earlier
        // future one.
        let body = r#"{
            "total_granted": 75.0, "total_used": 50.0, "total_available": 25.0,
            "grants": {"data": [
                {"grant_amount": 25, "used_amount": 25, "expires_at": 1500000000},
                {"grant_amount": 25, "used_amount": 10, "expires_at": 1900000000},
                {"grant_amount": 25, "used_amount": 15, "expires_at": 2000000000}
            ]}
        }"#;
        let parsed = parse_response(body, fixed_now()).expect("parse");
        let earliest_future = parsed.next_grant_expiry.expect("should have future grant");
        assert_eq!(earliest_future.timestamp(), 1900000000);
    }

    #[test]
    fn all_grants_expired_yields_none() {
        let body = r#"{
            "total_granted": 25, "total_used": 25, "total_available": 0,
            "grants": {"data": [
                {"grant_amount": 25, "used_amount": 25, "expires_at": 1500000000}
            ]}
        }"#;
        let parsed = parse_response(body, fixed_now()).expect("parse");
        assert!(parsed.next_grant_expiry.is_none());
    }

    #[test]
    fn expires_at_as_string_is_tolerated() {
        // Some old OpenAI client libraries serialize epochs as strings.
        // We tolerate that.
        let body = r#"{
            "total_granted": 5, "total_used": 0, "total_available": 5,
            "grants": {"data": [{"grant_amount": 5, "used_amount": 0, "expires_at": "1900000000"}]}
        }"#;
        let parsed = parse_response(body, fixed_now()).expect("parse");
        assert_eq!(parsed.grants[0].expires_at.timestamp(), 1900000000);
    }

    #[test]
    fn missing_expires_at_is_shape_error() {
        let body = r#"{
            "total_granted": 5, "total_used": 0, "total_available": 5,
            "grants": {"data": [{"grant_amount": 5, "used_amount": 0}]}
        }"#;
        match parse_response(body, fixed_now()) {
            Err(OpenAiError::ResponseShape(msg)) => assert!(msg.contains("expires_at")),
            other => panic!("expected ResponseShape, got {other:?}"),
        }
    }

    #[test]
    fn invalid_json_is_shape_error() {
        match parse_response("not json", fixed_now()) {
            Err(OpenAiError::ResponseShape(msg)) => assert!(msg.contains("invalid JSON")),
            other => panic!("expected ResponseShape, got {other:?}"),
        }
    }
}
