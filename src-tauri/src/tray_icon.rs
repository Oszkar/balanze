//! Runtime-rendered gauge tray icon. Hand-rolled RGBA raster (no extra dep):
//! a colored ring on a transparent background, color chosen by `ColorBucket`.
//! AGENTS.md §3.1: painting is deduped upstream in `tauri_sink`.

use tauri::AppHandle;
use tauri::image::Image;

use crate::tauri_sink::ColorBucket;

const SIZE: u32 = 32;

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
/// the tray has no persistent label). Logs and no-ops on any Tauri error - a
/// failed repaint must never crash the coordinator task.
pub(crate) fn paint(app: &AppHandle, bucket: ColorBucket, title: &str, tooltip: &str) {
    let Some(tray) = app.tray_by_id("main") else {
        tracing::warn!("tray_icon: tray 'main' not found; skipping paint");
        return;
    };
    let rgba = render_gauge(bucket, SIZE);
    let img = Image::new_owned(rgba, SIZE, SIZE);
    if let Err(e) = tray.set_icon(Some(img)) {
        tracing::warn!("tray_icon: set_icon failed: {e}");
    }
    let _ = tray.set_tooltip(Some(if tooltip.is_empty() {
        "Balanze"
    } else {
        tooltip
    }));
    // `title` is the macOS menu-bar text only; Windows/Linux trays have no label.
    #[cfg(target_os = "macos")]
    {
        let _ = tray.set_title(Some(title));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = title;
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
}
