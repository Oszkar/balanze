//! De-duplicate `UsageEvent`s by their `(message_id, request_id)` pair.
//!
//! Claude Code emits partial and completed usage for the same assistant
//! message. Output counts are cumulative, so keep the most complete record
//! instead of summing duplicates or retaining an early partial count.

use std::collections::{HashMap, hash_map::Entry};

use crate::types::UsageEvent;

/// Collapse each `(message_id, request_id)` pair to the record with the largest
/// cumulative output count. Equal counts prefer the later timestamp; exact
/// ties keep the first record. Select a whole record, never sum duplicates or
/// combine counters from different records. File traversal order must not let
/// a copied partial record replace completed usage.
///
/// Events where either `message_id` or `request_id` is `None` are never
/// deduped - without a complete key we can't safely identify duplicates, so
/// they pass through unchanged.
///
/// Keys (and unkeyed events) retain their first-appearance order. O(n) time,
/// O(retained-events) space, with no cloned events or identifier strings.
pub fn dedup_events(events: &mut Vec<UsageEvent>) {
    // Decide against borrowed keys before mutating the vector.
    let winners = {
        let mut seen = HashMap::new();
        let mut winners: Vec<usize> = Vec::new();
        for (index, event) in events.iter().enumerate() {
            match (event.message_id.as_deref(), event.request_id.as_deref()) {
                (Some(message), Some(request)) => match seen.entry((message, request)) {
                    Entry::Vacant(entry) => {
                        entry.insert(winners.len());
                        winners.push(index);
                    }
                    Entry::Occupied(entry) => {
                        let winner = &mut winners[*entry.get()];
                        let previous = &events[*winner];
                        if (event.output_tokens, event.ts) > (previous.output_tokens, previous.ts) {
                            *winner = index;
                        }
                    }
                },
                _ => winners.push(index),
            }
        }
        winners
    };
    // Each winner is at or after its first appearance, hence at or after its
    // destination. Earlier swaps cannot touch a later winner's source index:
    // their destinations precede it and their sources belong to distinct keys.
    let retained = winners.len();
    for (destination, source) in winners.into_iter().enumerate() {
        events.swap(destination, source);
    }
    events.truncate(retained);
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
            cache_creation: None,
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
    fn duplicates_keep_greatest_cumulative_output_not_first_or_last() {
        let mut events = vec![
            ev(Some("msg_a"), Some("req_1"), 100),
            ev(Some("msg_a"), Some("req_1"), 999), // dup of #0
            ev(Some("msg_a"), Some("req_1"), 888), // dup of #0
        ];
        dedup_events(&mut events);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].output_tokens, 999);
    }

    #[test]
    fn dedup_preserves_input_order_for_distinct_events() {
        let mut events = vec![
            ev(Some("msg_a"), Some("req_1"), 1),
            ev(Some("msg_b"), Some("req_2"), 2),
            ev(Some("msg_a"), Some("req_1"), 999), // updated usage
            ev(Some("msg_c"), Some("req_3"), 3),
            ev(Some("msg_b"), Some("req_2"), 998), // updated usage
        ];
        dedup_events(&mut events);
        let outputs: Vec<u64> = events.iter().map(|e| e.output_tokens).collect();
        assert_eq!(outputs, vec![999, 998, 3]);
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
        let mut events = vec![ev(Some("msg_a"), None, 100), ev(Some("msg_a"), None, 200)];
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
            ev(Some("msg_a"), Some("req_1"), 300), // updated usage
            ev(None, None, 400),                   // kept (no key)
        ];
        dedup_events(&mut events);
        let outputs: Vec<u64> = events.iter().map(|e| e.output_tokens).collect();
        assert_eq!(outputs, vec![300, 200, 400]);
    }

    #[test]
    fn completed_record_wins_in_either_file_order_without_summing_counters() {
        let mut partial = ev(Some("msg"), Some("req"), 7);
        partial.input_tokens = 2;
        partial.cache_creation_input_tokens = 55_379;
        let mut complete = partial.clone();
        complete.output_tokens = 206;
        complete.ts += chrono::Duration::seconds(1);
        // Even a later copy of the partial record cannot replace completed usage.
        partial.ts += chrono::Duration::seconds(2);
        for mut events in [
            vec![partial.clone(), complete.clone(), partial.clone()],
            vec![complete.clone(), partial.clone(), complete.clone()],
        ] {
            dedup_events(&mut events);
            assert_eq!(events, vec![complete.clone()]);
            dedup_events(&mut events);
            assert_eq!(events, vec![complete.clone()], "dedup is idempotent");
        }
    }

    #[test]
    fn equal_output_uses_latest_record_as_a_whole() {
        let older = ev(Some("msg"), Some("req"), 100);
        let mut newer = older.clone();
        newer.ts += chrono::Duration::seconds(1);
        newer.input_tokens = 2;
        newer.cache_read_input_tokens = 80;
        for mut events in [
            vec![older.clone(), newer.clone()],
            vec![newer.clone(), older],
        ] {
            dedup_events(&mut events);
            assert_eq!(events, vec![newer.clone()]);
        }
    }
}
