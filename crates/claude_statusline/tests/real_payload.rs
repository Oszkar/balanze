//! Pins the REAL statusLine payload shape from the maintainer's installed
//! Claude Code v2.1.144 (Pro/Max, captured 2026-05; cwd/session/transcript
//! paths redacted to "REDACTED"). Proves the documented `rate_limits` block
//! is actually EMITTED by the installed version (not just documented), that
//! `cost.total_cost_usd` parses on a real large value, and that the parser
//! tolerates the real payload's extra fields (effort, fast_mode, thinking,
//! context_window, …). rate_limits was identical across two independent
//! sessions (different model + cwd) — it is account-global, not per-session.
use claude_statusline::parse;

#[test]
fn real_captured_payload_parses_with_rate_limits() {
    let body = include_str!("fixtures/real-payload.json");
    let s = parse(body).expect("real payload parses");
    let rl = s
        .rate_limits
        .expect("Claude Code 2.1.144 emits rate_limits (Pro/Max, post-first-response)");
    let fh = rl.five_hour.expect("five_hour window present");
    assert!((fh.used_percent - 45.0).abs() < 1e-4);
    assert_eq!(fh.resets_at.timestamp(), 1779209400);
    let sd = rl.seven_day.expect("seven_day window present");
    assert!((sd.used_percent - 54.0).abs() < 1e-4);
    assert_eq!(sd.resets_at.timestamp(), 1779458400);
    // $100.03923590000005 × 1e6 = 100_039_235.9... -> round-half-away -> 100_039_236 micro-USD
    assert_eq!(s.session_cost_micro_usd, Some(100_039_236));
    assert_eq!(s.claude_code_version.as_deref(), Some("2.1.144"));
    assert_eq!(
        s.model_display_name.as_deref(),
        Some("Opus 4.7 (1M context)")
    );
    assert_eq!(s.context_used_percent, Some(83.0));
}
