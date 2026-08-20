//! Self-compose path for cross-provider statusline cells.
//!
//! This renderer owns presentation only. The real OpenAI source delegates to
//! `openai_client`, which owns the durable machine-wide Costs gate. This path
//! never calls Anthropic OAuth.

use chrono::{DateTime, Utc};

use crate::render::CrossProvider;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CodexWindows {
    pub five_hour: Option<f32>,
    pub weekly: Option<f32>,
    pub stale: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpenAiCell {
    pub total_micro_usd: Option<i64>,
    pub partial: bool,
    pub stale: bool,
}

// This crate-local static-dispatch trait has one production implementation and
// test fakes. A boxed future would add allocation and type noise without making
// dynamic dispatch available or serving another caller.
#[allow(async_fn_in_trait)]
pub trait CrossSources {
    async fn openai_cell(&self) -> OpenAiCell;
    fn codex_windows(&self, now: DateTime<Utc>) -> CodexWindows;
}

/// Compose cross-provider cells without the watcher.
///
/// `want_openai = false` must leave the entire OpenAI path untouched. The
/// caller uses the same template-token rule as the renderer.
pub async fn self_compose<S: CrossSources>(
    sources: &S,
    now: DateTime<Utc>,
    want_openai: bool,
) -> CrossProvider {
    let codex = sources.codex_windows(now);
    let openai = if want_openai {
        sources.openai_cell().await
    } else {
        OpenAiCell::default()
    };

    CrossProvider {
        codex_five_hour: codex.five_hour,
        codex_weekly: codex.weekly,
        openai_cost_micro_usd: openai.total_micro_usd,
        openai_partial: openai.partial,
        codex_stale: codex.stale,
        openai_stale: openai.stale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;
    use std::cell::Cell;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 30, 12, 0, 0).unwrap()
    }

    struct Fake {
        openai: OpenAiCell,
        codex: CodexWindows,
        calls: Cell<u32>,
    }

    impl CrossSources for Fake {
        async fn openai_cell(&self) -> OpenAiCell {
            self.calls.set(self.calls.get() + 1);
            self.openai
        }

        fn codex_windows(&self, _now: DateTime<Utc>) -> CodexWindows {
            self.codex
        }
    }

    #[tokio::test]
    async fn current_source_cell_renders_current() {
        let source = Fake {
            openai: OpenAiCell {
                total_micro_usd: Some(4_200_000),
                partial: true,
                stale: false,
            },
            codex: CodexWindows::default(),
            calls: Cell::new(0),
        };
        let composed = self_compose(&source, t0(), true).await;
        assert_eq!(composed.openai_cost_micro_usd, Some(4_200_000));
        assert!(composed.openai_partial);
        assert!(!composed.openai_stale);
        assert_eq!(source.calls.get(), 1);
    }

    #[tokio::test]
    async fn stale_source_cell_renders_stale() {
        let source = Fake {
            openai: OpenAiCell {
                total_micro_usd: Some(999),
                partial: false,
                stale: true,
            },
            codex: CodexWindows::default(),
            calls: Cell::new(0),
        };
        let composed = self_compose(&source, t0(), true).await;
        assert_eq!(composed.openai_cost_micro_usd, Some(999));
        assert!(composed.openai_stale);
    }

    #[tokio::test]
    async fn absent_stale_value_leaves_the_cell_absent() {
        let source = Fake {
            openai: OpenAiCell {
                total_micro_usd: None,
                partial: false,
                stale: true,
            },
            codex: CodexWindows::default(),
            calls: Cell::new(0),
        };
        let composed = self_compose(&source, t0(), true).await;
        assert_eq!(composed.openai_cost_micro_usd, None);
        assert!(composed.openai_stale);
    }

    #[tokio::test]
    async fn want_openai_false_skips_the_source() {
        let source = Fake {
            openai: OpenAiCell {
                total_micro_usd: Some(4_200_000),
                partial: false,
                stale: false,
            },
            codex: CodexWindows {
                five_hour: Some(12.0),
                weekly: Some(25.0),
                stale: true,
            },
            calls: Cell::new(0),
        };
        let composed = self_compose(&source, t0(), false).await;
        assert_eq!(source.calls.get(), 0);
        assert_eq!(composed.openai_cost_micro_usd, None);
        assert_eq!(composed.codex_five_hour, Some(12.0));
        assert_eq!(composed.codex_weekly, Some(25.0));
        assert!(composed.codex_stale);
    }
}
