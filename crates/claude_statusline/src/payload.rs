use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::StatuslineSnapshot;

/// Current schema version. Any change to the on-disk JSON shape is a §8
/// (Change Control) event that requires bumping this constant AND adding a
/// `from_v<N>` migration path in `file_io::read_snapshot`.
pub const SCHEMA_VERSION: u8 = 1;

/// Envelope written to disk by `balanze-cli statusline` and read by the
/// watcher. `captured_at` is the producer's wall-clock at write
/// time — the authoritative freshness signal for the consumer's render-time
/// dedup (prevents replaying stale snapshots).
///
/// The envelope is independent of the Claude Code `statusLine` wire format
/// (owned by `parse.rs`) and of the `statusLine` stanza in Claude's
/// `settings.json` (owned by `wiring.rs`). It is Balanze's own IPC data
/// file, sitting at `<data_dir>/statusline.snapshot.json` where
/// `<data_dir>` is `directories::ProjectDirs::from("me", "oszkar", "Balanze").data_dir()`
/// (already includes the per-OS Balanze subpath; see `file_io` module doc).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatuslineFilePayload {
    pub schema_version: u8,
    pub captured_at: DateTime<Utc>,
    pub payload: StatuslineSnapshot,
}

impl StatuslineFilePayload {
    /// Construct a versioned envelope stamped with the given wall-clock time.
    pub fn new(payload: StatuslineSnapshot, captured_at: DateTime<Utc>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            captured_at,
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn sample_snapshot() -> StatuslineSnapshot {
        StatuslineSnapshot {
            rate_limits: None,
            session_cost_micro_usd: Some(3_420_000),
            claude_code_version: Some("v2.1.144".to_string()),
            model_display_name: None,
            context_used_percent: None,
        }
    }

    #[test]
    fn roundtrips_through_json() {
        let captured_at = Utc.with_ymd_and_hms(2026, 5, 21, 12, 0, 0).unwrap();
        let original = StatuslineFilePayload::new(sample_snapshot(), captured_at);
        let json = serde_json::to_string(&original).unwrap();
        let back: StatuslineFilePayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn schema_version_is_one() {
        let captured_at = Utc.with_ymd_and_hms(2026, 5, 21, 12, 0, 0).unwrap();
        let p = StatuslineFilePayload::new(sample_snapshot(), captured_at);
        assert_eq!(p.schema_version, SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 1);
    }

    #[test]
    fn new_stamps_schema_version() {
        let captured_at = Utc.with_ymd_and_hms(2026, 5, 21, 0, 0, 0).unwrap();
        let p = StatuslineFilePayload::new(sample_snapshot(), captured_at);
        assert_eq!(p.schema_version, 1);
        assert_eq!(p.captured_at, captured_at);
    }
}
