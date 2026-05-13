//! De-duplicate `UsageEvent`s by their `(message_id, request_id)` pair.
//!
//! Claude Code's JSONL writer occasionally emits the same assistant message
//! multiple times in the same file (observed: ~37% of unique keys in a real
//! session were duplicated, all with byte-identical usage payloads). Without
//! dedup, token totals over-count by the duplication factor.

use std::collections::HashSet;

use crate::types::UsageEvent;

/// Collapse duplicate events in place, keeping the first occurrence of each
/// `(message_id, request_id)` pair and discarding subsequent ones.
///
/// Events where either `message_id` or `request_id` is `None` are never
/// deduped — without a complete key we can't safely identify duplicates, so
/// they pass through unchanged.
///
/// Stable: original ordering of retained events is preserved. O(n) time,
/// O(unique-keys) space.
pub fn dedup_events(events: &mut Vec<UsageEvent>) {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    events.retain(|e| match (e.message_id.as_deref(), e.request_id.as_deref()) {
        (Some(m), Some(r)) => seen.insert((m.to_string(), r.to_string())),
        _ => true,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AccountType, DataSource, Provider};
    use chrono::{TimeZone, Utc};

    fn ev(msg: Option<&str>, req: Option<&str>, output: u64) -> UsageEvent {
        UsageEvent {
            ts: Utc.with_ymd_and_hms(2026, 5, 14, 12, 0, 0).unwrap(),
            provider: Provider::Claude,
            account_type: AccountType::Subscription,
            model: "claude-sonnet-4-6".to_string(),
            input_tokens: 0,
            output_tokens: output,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_micro_usd: None,
            source: DataSource::Jsonl,
            message_id: msg.map(String::from),
            request_id: req.map(String::from),
        }
    }

    #[test]
    fn empty_input_is_noop() {
        let mut events: Vec<UsageEvent> = Vec::new();
        dedup_events(&mut events);
        assert!(events.is_empty());
    }

    #[test]
    fn no_duplicates_passes_through_unchanged() {
        let mut events = vec![
            ev(Some("msg_a"), Some("req_1"), 100),
            ev(Some("msg_b"), Some("req_2"), 200),
            ev(Some("msg_c"), Some("req_3"), 300),
        ];
        dedup_events(&mut events);
        assert_eq!(events.len(), 3);
        let outputs: Vec<u64> = events.iter().map(|e| e.output_tokens).collect();
        assert_eq!(outputs, vec![100, 200, 300]);
    }

    #[test]
    fn duplicates_collapsed_keeping_first_occurrence() {
        let mut events = vec![
            ev(Some("msg_a"), Some("req_1"), 100),
            ev(Some("msg_a"), Some("req_1"), 999), // dup of #0
            ev(Some("msg_a"), Some("req_1"), 888), // dup of #0
        ];
        dedup_events(&mut events);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].output_tokens, 100); // the first one wins
    }

    #[test]
    fn dedup_preserves_input_order_for_distinct_events() {
        let mut events = vec![
            ev(Some("msg_a"), Some("req_1"), 1),
            ev(Some("msg_b"), Some("req_2"), 2),
            ev(Some("msg_a"), Some("req_1"), 999), // dup; removed
            ev(Some("msg_c"), Some("req_3"), 3),
            ev(Some("msg_b"), Some("req_2"), 998), // dup; removed
        ];
        dedup_events(&mut events);
        let outputs: Vec<u64> = events.iter().map(|e| e.output_tokens).collect();
        assert_eq!(outputs, vec![1, 2, 3]);
    }

    #[test]
    fn events_with_missing_message_id_are_not_deduped() {
        let mut events = vec![
            ev(None, Some("req_1"), 100),
            ev(None, Some("req_1"), 200), // same req but no msg_id → kept
            ev(None, Some("req_1"), 300),
        ];
        dedup_events(&mut events);
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn events_with_missing_request_id_are_not_deduped() {
        let mut events = vec![
            ev(Some("msg_a"), None, 100),
            ev(Some("msg_a"), None, 200),
        ];
        dedup_events(&mut events);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn same_message_id_different_request_id_are_distinct() {
        // Real case: a single Anthropic message_id can be reused across
        // multiple in-flight requests (rare but legal per the API). Dedup
        // must compare the full pair, not just message_id.
        let mut events = vec![
            ev(Some("msg_a"), Some("req_1"), 100),
            ev(Some("msg_a"), Some("req_2"), 200),
        ];
        dedup_events(&mut events);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn mixed_keyed_and_unkeyed_events_handled_independently() {
        let mut events = vec![
            ev(Some("msg_a"), Some("req_1"), 100),
            ev(None, None, 200),
            ev(Some("msg_a"), Some("req_1"), 300), // dup; removed
            ev(None, None, 400),                   // kept (no key)
        ];
        dedup_events(&mut events);
        let outputs: Vec<u64> = events.iter().map(|e| e.output_tokens).collect();
        assert_eq!(outputs, vec![100, 200, 400]);
    }
}
