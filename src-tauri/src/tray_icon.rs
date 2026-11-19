//! Runtime-rendered gauge tray icon. Hand-rolled RGBA raster (no extra dep):
//! vertical utilization bars with provider identity and explicit missing/stale states.
//! AGENTS.md §3.1: targets are deduped upstream in `tauri_sink`; this worker
//! also retains per-property successes while retrying a partial paint.

use tauri::AppHandle;
use tauri::image::Image;

use crate::tauri_sink::ColorBucket;

const SIZE: u32 = 32;
const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
const WARN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Provider {
    Claude,
    Codex,
}
impl Provider {
    pub(crate) fn short_name(self) -> &'static str {
        match self {
            Self::Claude => "Cla",
            Self::Codex => "Cdx",
        }
    }
    pub(crate) fn full_name(self) -> &'static str {
        match self {
            Self::Claude => "Anthropic / Claude quota",
            Self::Codex => "OpenAI / Codex quota",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IconState {
    Fresh,
    Stale,
    Partial,
    Missing,
    Connecting,
    Error,
}
impl IconState {
    pub(crate) fn degraded(self) -> bool {
        matches!(self, Self::Stale | Self::Partial | Self::Error)
    }
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Fresh => "current",
            Self::Stale => "stale - last known quota",
            Self::Partial => "some quota unavailable",
            Self::Missing => "quota unavailable",
            Self::Connecting => "connecting...",
            Self::Error => "quota error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IconTarget {
    pub(crate) provider: Provider,
    pub(crate) bucket: ColorBucket,
    pub(crate) fill: Option<u8>,
    pub(crate) state: IconState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaintTarget {
    pub(crate) icon: IconTarget,
    pub(crate) title: String,
    pub(crate) tooltip: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaintSet {
    pub(crate) main: PaintTarget,
    pub(crate) codex: Option<PaintTarget>,
}

#[derive(Debug, Clone, Default)]
struct AppliedSet {
    main: AppliedPaint,
    codex: Option<AppliedPaint>,
}
impl AppliedSet {
    fn matches(&self, target: &PaintSet) -> bool {
        self.main.matches(&target.main)
            && match (&self.codex, &target.codex) {
                (None, None) => true,
                (Some(applied), Some(next)) => applied.matches(next),
                _ => false,
            }
    }
}

/// Tray properties that the operating system has accepted. This is the
/// worker's only dedupe cache: a partial success is retained so retries do not
/// churn setters that already succeeded, and a target counts as painted only
/// when every property in here matches it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AppliedPaint {
    icon: Option<IconTarget>,
    tooltip: Option<String>,
    #[cfg(target_os = "macos")]
    title: Option<String>,
}

impl AppliedPaint {
    fn needs_icon(&self, target: &PaintTarget) -> bool {
        self.icon != Some(target.icon)
    }

    fn effective_tooltip(target: &PaintTarget) -> &str {
        if target.tooltip.is_empty() {
            "Balanze"
        } else {
            &target.tooltip
        }
    }

    fn needs_tooltip(&self, target: &PaintTarget) -> bool {
        self.tooltip.as_deref() != Some(Self::effective_tooltip(target))
    }

    #[cfg(target_os = "macos")]
    fn needs_title(&self, target: &PaintTarget) -> bool {
        self.title.as_deref() != Some(target.title.as_str())
    }

    /// True when every operating-system property already holds `target`, so a
    /// repaint would be a no-op. This - not "the last target we finished
    /// painting" - is what the worker must dedupe against: after a partial
    /// paint the two disagree, and trusting the latter leaves the properties
    /// that DID land showing a target the worker believes it abandoned.
    fn matches(&self, target: &PaintTarget) -> bool {
        let matched = !self.needs_icon(target) && !self.needs_tooltip(target);
        #[cfg(target_os = "macos")]
        let matched = matched && !self.needs_title(target);
        matched
    }
}

fn bucket_rgb(bucket: ColorBucket) -> (u8, u8, u8) {
    match bucket {
        // Muted cool grey: visible on both light and dark trays, clearly
        // "inactive" next to the green/yellow/orange/red heat colors.
        ColorBucket::Neutral => (0x8a, 0x8f, 0x99),
        ColorBucket::Green => (0x3f, 0x8f, 0x5f),
        ColorBucket::Yellow => (0xcf, 0x8a, 0x2a),
        ColorBucket::Orange => (0xd9, 0x6a, 0x2a),
        ColorBucket::Red => (0xc0, 0x49, 0x3a),
        // Degraded/stale. Deliberately outside the green-to-red heat ramp: the
        // fill color and marker make a source problem
        // distinguishable from 90% without opening the tooltip.
        ColorBucket::Warn => (0x7c, 0x6a, 0xc9),
    }
}

/// Integer raster at a 16-pixel design grid, doubled for the supplied image.
/// Identity never changes with severity. Windows uses A/O with a contrasting
/// outline; macOS has the full short name in its native title.
fn render_bar(target: IconTarget, windows: bool) -> Vec<u8> {
    let mut pixels = vec![0; (SIZE * SIZE * 4) as usize];
    let mut dot = |x: usize, y: usize, rgb: (u8, u8, u8)| {
        for dy in 0..2 {
            for dx in 0..2 {
                let i = ((y * 2 + dy) * SIZE as usize + x * 2 + dx) * 4;
                pixels[i..i + 4].copy_from_slice(&[rgb.0, rgb.1, rgb.2, 255]);
            }
        }
    };
    let (left, right) = if windows { (9, 14) } else { (5, 10) };
    let top = 2;
    let bottom = 13;
    for y in top..=bottom {
        for x in left..=right {
            if x == left || x == right || y == top || y == bottom {
                dot(x, y, (138, 143, 153));
            }
        }
    }
    let color = bucket_rgb(target.bucket);
    if let Some(fill) = target.fill {
        let height = (usize::from(fill.min(100)) * (bottom - top - 1) + 50) / 100;
        for y in bottom - height..bottom {
            for x in left + 1..right {
                dot(x, y, color);
            }
        }
    } else {
        match target.state {
            IconState::Connecting => {
                for y in [5, 7, 9] {
                    dot(left + 2, y, color);
                }
            }
            IconState::Error => {
                for y in 4..8 {
                    dot(left + 2, y, color);
                }
                dot(left + 2, 10, color);
            }
            _ => {
                for x in left + 1..right {
                    dot(x, 7, color);
                }
            }
        }
    }
    if windows {
        let glyph = match target.provider {
            Provider::Claude => [0b010, 0b101, 0b111, 0b101, 0b101],
            Provider::Codex => [0b111, 0b101, 0b101, 0b101, 0b111],
        };
        // Dark halo plus light foreground remains visible in both taskbar themes.
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..3 {
                if bits & (1 << (2 - col)) != 0 {
                    for dy in 0..3 {
                        for dx in 0..3 {
                            dot(1 + col + dx, 4 + row + dy, (32, 36, 32));
                        }
                    }
                }
            }
        }
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..3 {
                if bits & (1 << (2 - col)) != 0 {
                    dot(2 + col, 5 + row, (245, 246, 239));
                }
            }
        }
    }
    if target.state.degraded() {
        for y in 1..4 {
            dot(15, y, color);
        }
        dot(15, 5, color);
    }
    pixels
}

/// Paint the tray icon, macOS menu-bar title, and hover tooltip. `title` is the
/// compact `Cla X%` or `Cdx Y%` label (macOS menu bar only); `tooltip` is the
/// multi-line status panel shown on hover (and the only text on Windows, where
/// the tray has no persistent label). Setter failures are collected and
/// returned to the dedicated painter, which retries without blocking the
/// coordinator actor or repeating properties that already landed.
fn paint(
    app: &AppHandle,
    id: &str,
    target: &PaintTarget,
    applied: &mut AppliedPaint,
) -> Result<(), String> {
    let Some(tray) = app.tray_by_id(id) else {
        return Err(format!("tray {id} not found"));
    };
    let mut failures = Vec::new();
    if applied.needs_icon(target) {
        let rgba = render_bar(target.icon, cfg!(target_os = "windows"));
        let img = Image::new_owned(rgba, SIZE, SIZE);
        match tray.set_icon(Some(img)) {
            Ok(()) => applied.icon = Some(target.icon),
            Err(error) => failures.push(format!("set_icon failed: {error}")),
        }
    }
    let tooltip = AppliedPaint::effective_tooltip(target);
    if applied.needs_tooltip(target) {
        match tray.set_tooltip(Some(tooltip)) {
            Ok(()) => applied.tooltip = Some(tooltip.to_string()),
            Err(error) => failures.push(format!("set_tooltip failed: {error}")),
        }
    }
    // `title` is the macOS menu-bar text only; Windows/Linux trays have no label.
    #[cfg(target_os = "macos")]
    {
        if applied.needs_title(target) {
            match tray.set_title(Some(&target.title)) {
                Ok(()) => applied.title = Some(target.title.clone()),
                Err(error) => failures.push(format!("set_title failed: {error}")),
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = &target.title;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Create a replacement before removing the extra item so popover access is
/// retained on failures. Only successful per-item setters are cached.
fn paint_set(app: &AppHandle, target: &PaintSet, applied: &mut AppliedSet) -> Result<(), String> {
    paint(app, "main", &target.main, &mut applied.main)?;
    if let Some(codex) = &target.codex {
        if app.tray_by_id("codex").is_none() {
            crate::create_codex_tray(app).map_err(|e| e.to_string())?;
            applied.codex = Some(AppliedPaint::default());
        }
        paint(
            app,
            "codex",
            codex,
            applied.codex.get_or_insert_with(AppliedPaint::default),
        )?;
    } else {
        if app.tray_by_id("codex").is_some() {
            drop(app.remove_tray_by_id("codex"));
        }
        applied.codex = None;
    }
    Ok(())
}

/// Start the single repaint worker. The watch channel coalesces a burst to the
/// newest target, and each blocking Tauri/main-thread round trip runs on the
/// blocking pool rather than on the coordinator's tokio worker.
pub(crate) fn spawn_painter(
    app: AppHandle,
) -> (
    tokio::sync::watch::Sender<Option<PaintSet>>,
    tokio::task::JoinHandle<Result<(), String>>,
) {
    let (tx, mut rx) = tokio::sync::watch::channel::<Option<PaintSet>>(None);
    let join = tokio::spawn(async move {
        let mut applied = AppliedSet::default();
        let mut last_warn: Option<std::time::Instant> = None;
        loop {
            if rx.changed().await.is_err() {
                return Ok(());
            }
            let Some(mut target) = rx.borrow_and_update().clone() else {
                continue;
            };
            if applied.matches(&target) {
                continue;
            }
            loop {
                let paint_app = app.clone();
                let attempt = target.clone();
                let mut attempt_applied = applied.clone();
                let (next_applied, result) = tokio::task::spawn_blocking(move || {
                    let result = paint_set(&paint_app, &attempt, &mut attempt_applied);
                    (attempt_applied, result)
                })
                .await
                .map_err(|error| format!("tray repaint worker failed: {error}"))?;
                applied = next_applied;
                match result {
                    Ok(()) => break,
                    Err(error)
                        if last_warn
                            .is_none_or(|last_warn| last_warn.elapsed() >= WARN_INTERVAL) =>
                    {
                        tracing::warn!("tray_icon: repaint failed: {error}; retrying");
                        last_warn = Some(std::time::Instant::now());
                    }
                    Err(error) => tracing::debug!("tray_icon: repaint retry failed: {error}"),
                }

                tokio::select! {
                    changed = rx.changed() => {
                        if changed.is_err() {
                            return Ok(());
                        }
                        if let Some(next) = rx.borrow_and_update().clone() {
                            target = next;
                            // Only stop retrying when the operating system is
                            // ALREADY showing the new target. Reverting to the
                            // last fully painted target after a partial paint
                            // still needs a repaint of whatever landed since.
                            if applied.matches(&target) {
                                break;
                            }
                        }
                    }
                    _ = tokio::time::sleep(RETRY_DELAY) => {}
                }
            }
        }
    });
    (tx, join)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icon(fill: u8) -> IconTarget {
        IconTarget {
            provider: Provider::Claude,
            bucket: ColorBucket::from_util(f32::from(fill)),
            fill: Some(fill),
            state: IconState::Fresh,
        }
    }

    #[test]
    fn fill_and_identity_changes_repaint_within_the_same_bucket() {
        let a = PaintTarget {
            icon: icon(10),
            title: "Cla".into(),
            tooltip: "quota".into(),
        };
        let mut b = a.clone();
        b.icon.fill = Some(40);
        let applied = fully_applied(&a);
        assert!(applied.needs_icon(&b));
        assert_ne!(render_bar(a.icon, true), render_bar(b.icon, true));
        b.icon = a.icon;
        b.icon.provider = Provider::Codex;
        assert!(applied.needs_icon(&b));
        assert_ne!(render_bar(a.icon, true), render_bar(b.icon, true));
    }

    #[test]
    fn topology_changes_are_not_deduped_even_when_main_is_unchanged() {
        let main = PaintTarget {
            icon: icon(32),
            title: "Cla".into(),
            tooltip: "Claude quota".into(),
        };
        let mut codex = main.clone();
        codex.icon.provider = Provider::Codex;
        codex.title = "Cdx".into();
        let separate = PaintSet {
            main: main.clone(),
            codex: Some(codex.clone()),
        };
        let combined = PaintSet {
            main: main.clone(),
            codex: None,
        };
        let mut applied = AppliedSet {
            main: fully_applied(&main),
            codex: None,
        };
        assert!(applied.matches(&combined));
        assert!(!applied.matches(&separate));
        applied.codex = Some(fully_applied(&codex));
        assert!(applied.matches(&separate));
        assert!(!applied.matches(&combined));
    }

    #[test]
    fn missing_stale_and_zero_have_distinct_pixels() {
        let zero = icon(0);
        let mut missing = zero;
        missing.fill = None;
        missing.state = IconState::Missing;
        let mut stale = zero;
        stale.state = IconState::Stale;
        stale.bucket = ColorBucket::Warn;
        for windows in [false, true] {
            assert_ne!(render_bar(zero, windows), render_bar(missing, windows));
            assert_ne!(render_bar(zero, windows), render_bar(stale, windows));
        }
    }

    #[test]
    fn gauge_has_correct_size_and_some_opaque_pixels() {
        let buf = render_bar(icon(10), true);
        assert_eq!(buf.len(), (SIZE * SIZE * 4) as usize);
        let opaque = buf.chunks_exact(4).filter(|px| px[3] > 0).count();
        assert!(opaque > 0, "bar must have some opaque pixels");
        assert!(
            opaque < (SIZE * SIZE) as usize,
            "bar must not be fully filled"
        );
    }

    #[test]
    fn every_bucket_has_a_distinct_fill_color() {
        let buckets = [
            ColorBucket::Neutral,
            ColorBucket::Green,
            ColorBucket::Yellow,
            ColorBucket::Orange,
            ColorBucket::Red,
            ColorBucket::Warn,
        ];
        for (i, a) in buckets.iter().enumerate() {
            for b in &buckets[i + 1..] {
                assert_ne!(
                    bucket_rgb(*a),
                    bucket_rgb(*b),
                    "{a:?} and {b:?} must not share a fill color"
                );
            }
        }
    }

    /// Set every property the current platform paints, i.e. a fully
    /// successful paint of `target`.
    fn fully_applied(target: &PaintTarget) -> AppliedPaint {
        AppliedPaint {
            icon: Some(target.icon),
            tooltip: Some(AppliedPaint::effective_tooltip(target).to_string()),
            #[cfg(target_os = "macos")]
            title: Some(target.title.clone()),
        }
    }

    #[test]
    fn reverting_to_the_last_target_after_a_partial_paint_still_repaints() {
        let a = PaintTarget {
            icon: icon(10),
            title: "Claude 10%".to_string(),
            tooltip: "Claude 10%".to_string(),
        };
        let b = PaintTarget {
            icon: icon(95),
            title: "Claude 95%".to_string(),
            tooltip: "Claude 95%".to_string(),
        };

        // A painted fully.
        let mut applied = fully_applied(&a);
        assert!(applied.matches(&a));

        // B painted partially: the icon landed, the tooltip setter failed.
        applied.icon = Some(b.icon);
        assert!(!applied.matches(&b), "B is not fully applied");

        // Back to A: the operating system is still showing B's icon, so this
        // must NOT be deduped away - the bug was caching A as "last painted"
        // and skipping the repaint that reverts the icon.
        assert!(!applied.matches(&a));
        assert!(applied.needs_icon(&a));
        assert!(!applied.needs_tooltip(&a), "A's tooltip never changed");

        // Once the icon is repainted, A is genuinely applied again.
        applied.icon = Some(a.icon);
        assert!(applied.matches(&a));
    }

    #[test]
    fn partial_success_retries_only_unapplied_properties() {
        let target = PaintTarget {
            icon: icon(10),
            title: "Claude 10%".to_string(),
            tooltip: "Balanze".to_string(),
        };
        let mut applied = AppliedPaint::default();

        assert!(applied.needs_icon(&target));
        assert!(applied.needs_tooltip(&target));
        applied.icon = Some(target.icon);
        assert!(!applied.needs_icon(&target));
        assert!(applied.needs_tooltip(&target));

        applied.tooltip = Some(target.tooltip.clone());
        assert!(!applied.needs_tooltip(&target));
        #[cfg(target_os = "macos")]
        assert!(applied.needs_title(&target));
    }
}
