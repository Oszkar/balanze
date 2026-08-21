//! Runtime-rendered gauge tray icon. Hand-rolled RGBA raster (no extra dep):
//! a colored ring on a transparent background, color chosen by `ColorBucket`.
//! AGENTS.md §3.1: targets are deduped upstream in `tauri_sink`; this worker
//! also retains per-property successes while retrying a partial paint.

use tauri::AppHandle;
use tauri::image::Image;

use crate::tauri_sink::ColorBucket;

const SIZE: u32 = 32;
const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
const WARN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaintTarget {
    pub(crate) bucket: ColorBucket,
    pub(crate) title: String,
    pub(crate) tooltip: String,
}

/// Tray properties that the operating system has accepted. A target is cached
/// as fully painted only when every property matches, but a partial success is
/// retained so retries do not churn setters that already succeeded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AppliedPaint {
    bucket: Option<ColorBucket>,
    tooltip: Option<String>,
    #[cfg(target_os = "macos")]
    title: Option<String>,
}

impl AppliedPaint {
    fn needs_icon(&self, target: &PaintTarget) -> bool {
        self.bucket != Some(target.bucket)
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
        ColorBucket::Warn => (0xc0, 0x49, 0x3a),
    }
}

/// Render a `size`x`size` RGBA ring in the bucket color on transparent bg.
pub(crate) fn render_gauge(bucket: ColorBucket, size: u32) -> Vec<u8> {
    let (r, g, b) = bucket_rgb(bucket);
    let mut buf = vec![0u8; (size * size * 4) as usize];
    let c = (size as f32 - 1.0) / 2.0;
    let outer = size as f32 / 2.0 - 1.0;
    let inner = outer - size as f32 * 0.22;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - c;
            let dy = y as f32 - c;
            let dist = (dx * dx + dy * dy).sqrt();
            let alpha: u8 = if dist > inner && dist < outer { 255 } else { 0 };
            let i = ((y * size + x) * 4) as usize;
            buf[i] = r;
            buf[i + 1] = g;
            buf[i + 2] = b;
            buf[i + 3] = alpha;
        }
    }
    buf
}

/// Paint the tray icon, macOS menu-bar title, and hover tooltip. `title` is the
/// compact `Claude X% · Codex Y%` line (macOS menu bar only); `tooltip` is the
/// multi-line status panel shown on hover (and the only text on Windows, where
/// the tray has no persistent label). Setter failures are collected and
/// returned to the dedicated painter, which retries without blocking the
/// coordinator actor or repeating properties that already landed.
fn paint(app: &AppHandle, target: &PaintTarget, applied: &mut AppliedPaint) -> Result<(), String> {
    let Some(tray) = app.tray_by_id("main") else {
        return Err("tray 'main' not found".to_string());
    };
    let mut failures = Vec::new();
    if applied.needs_icon(target) {
        let rgba = render_gauge(target.bucket, SIZE);
        let img = Image::new_owned(rgba, SIZE, SIZE);
        match tray.set_icon(Some(img)) {
            Ok(()) => applied.bucket = Some(target.bucket),
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

/// Start the single repaint worker. The watch channel coalesces a burst to the
/// newest target, and each blocking Tauri/main-thread round trip runs on the
/// blocking pool rather than on the coordinator's tokio worker.
pub(crate) fn spawn_painter(
    app: AppHandle,
) -> (
    tokio::sync::watch::Sender<Option<PaintTarget>>,
    tokio::task::JoinHandle<Result<(), String>>,
) {
    let (tx, mut rx) = tokio::sync::watch::channel::<Option<PaintTarget>>(None);
    let join = tokio::spawn(async move {
        let mut last_painted = None;
        let mut applied = AppliedPaint::default();
        let mut last_warn: Option<std::time::Instant> = None;
        loop {
            if rx.changed().await.is_err() {
                return Ok(());
            }
            let Some(mut target) = rx.borrow_and_update().clone() else {
                continue;
            };
            if last_painted.as_ref() == Some(&target) {
                continue;
            }
            loop {
                let paint_app = app.clone();
                let attempt = target.clone();
                let mut attempt_applied = applied.clone();
                let (next_applied, result) = tokio::task::spawn_blocking(move || {
                    let result = paint(&paint_app, &attempt, &mut attempt_applied);
                    (attempt_applied, result)
                })
                .await
                .map_err(|error| format!("tray repaint worker failed: {error}"))?;
                applied = next_applied;
                match result {
                    Ok(()) => {
                        last_painted = Some(target.clone());
                        break;
                    }
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
                            if last_painted.as_ref() == Some(&target) {
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

    #[test]
    fn gauge_has_correct_size_and_some_opaque_pixels() {
        let buf = render_gauge(ColorBucket::Green, SIZE);
        assert_eq!(buf.len(), (SIZE * SIZE * 4) as usize);
        let opaque = buf.chunks_exact(4).filter(|px| px[3] > 0).count();
        assert!(opaque > 0, "ring must have some opaque pixels");
        assert!(
            opaque < (SIZE * SIZE) as usize,
            "ring must not be fully filled"
        );
    }

    #[test]
    fn partial_success_retries_only_unapplied_properties() {
        let target = PaintTarget {
            bucket: ColorBucket::Green,
            title: "Claude 10%".to_string(),
            tooltip: "Balanze".to_string(),
        };
        let mut applied = AppliedPaint::default();

        assert!(applied.needs_icon(&target));
        assert!(applied.needs_tooltip(&target));
        applied.bucket = Some(target.bucket);
        assert!(!applied.needs_icon(&target));
        assert!(applied.needs_tooltip(&target));

        applied.tooltip = Some(target.tooltip.clone());
        assert!(!applied.needs_tooltip(&target));
        #[cfg(target_os = "macos")]
        assert!(applied.needs_title(&target));
    }
}
