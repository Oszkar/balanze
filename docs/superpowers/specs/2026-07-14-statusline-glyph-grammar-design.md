# Statusline glyph grammar

Date: 2026-07-14
Status: approved, not yet implemented

## Problem

The default statusline renders like this:

```
[#---------] 11% 💰 ~$312.58 ⌛5h 15% ↓0.4× (3h8m) 📅7d 38% ↓0.7× (3d5h) ◇wk 7% OpenAI $0.00 ⚠
```

Three defects, in order of depth:

1. **The glyphs encode two different axes.** `⌛` and `📅` name the *window length* (and repeat what the "5h" / "7d" text says two characters later). `◇` names the *provider*. `💰` names the *metric*. There is no single rule to be uniform against, so any spacing fix is cosmetic.

2. **Spacing follows two rules.** `💰 ~$312.58` is `glyph + space + content`; `⌛5h` glues the glyph to the label. `[#---------] 11%` and `OpenAI $0.00` have no glyph at all.

3. **Glyph widths are mixed.** `💰 ⌛ 📅` are emoji-presentation and advance two cells. `◇` (U+25C7) and the bare `⚠` (U+26A0 with no variation selector) are text-presentation and advance one. Even with uniform spaces the segments would not line up.

Two smaller items fall out of the same review:

- The `openai_cost` segment is the only one with no period and no percent, which is why it reads as foreign next to everything else.
- The Codex weekly window is labeled `wk` here but `7d` in `crates/balanze_cli/src/render.rs` (asserted at `render.rs:1127`). The statusline is the only surface in the repo that calls it something other than `7d`.

## Design

### The grammar

One rule, no exceptions:

```
<emoji><space><content>
```

The emoji names **whose** number it is for rate windows, and **what kind** of number it is for the two Claude-only metrics. Every glyph is emoji-presentation, two cells wide.

| Segment | Today | After |
|---|---|---|
| `context_bar` | `[#---------] 11%` | `🧠 [#---------] 11%` |
| `cost` | `💰 ~$312.58` | unchanged |
| `usage` 5h | `⌛5h 15% ↓0.4× (3h8m)` | `✳️ 5h 15% ↓0.4× (3h8m)` |
| `usage` 7d | `📅7d 38% ↓0.7× (3d5h)` | `✳️ 7d 38% ↓0.7× (3d5h)` |
| `codex` 5h | `◇5h 12%` | `🌀 5h 12%` |
| `codex` weekly | `◇wk 7%` | `🌀 7d 7%` |
| stale marker | `⚠` | `⚠️` |
| `model` (not in the default line) | `🤖 Opus` | unchanged |
| `openai_cost` (leaving the default line) | `OpenAI $0.00` | `🌀 $0.00` |

Glyph assignments: `✳️` = Claude (its starburst mark), `🌀` = Codex / OpenAI (closest emoji to OpenAI's knot), `🧠` = context, `💰` = session cost estimate, `🤖` = model.

Resulting default line:

```
🧠 [#---------] 11% 💰 ~$312.58 ✳️ 5h 15% ↓0.4× (3h8m) ✳️ 7d 38% ↓0.7× (3d5h) 🌀 7d 7%
```

The provider glyph is now the only thing distinguishing Claude's `7d` window from Codex's, which is exactly what the provider axis is for. Segments stay separated by a single space (the glyph does the separating, and `fill_line` already collapses to one space, so no change there). The reset countdown keeps its parens, because a window carries three tokens (percent, pace, reset) and the parens mark the last one as a clock rather than another metric.

### Accepted trade-off

`◇` is text-presentation and therefore takes the ANSI severity color. `🌀` is emoji and will render in its own font colors regardless of the escape wrapping it. Severity still reads from the percentage beside it, which keeps its color, and the same is already true of today's `⌛` / `📅`. Uniform width is worth more here than a tintable glyph.

### Default template

`default_lines()` in `crates/settings/src/statusline.rs` becomes:

```
{context_bar} {cost} {usage} {codex}
```

`openai_cost` stays fully implemented and configurable. It only leaves the default line. The Claude subscription cap is measured in tokens and the Codex windows in percent, while OpenAI API spend is an uncapped dollar figure with no window; dropping it makes the default line a single coherent "percent of a rolling window" reading, plus the Claude session estimate.

### Gating the OpenAI fetch

`statusline_render::self_compose` (`self_compose.rs:49`) fetches the OpenAI cost unconditionally: cache read, refresh lease, and every 300s a real HTTP call to the Admin Costs API. Removing the segment from the default line without touching this leaves all of that running every turn to produce a number nothing renders.

The fetch becomes demand-gated:

```rust
let want_openai = config.lines.iter().any(|l| l.contains("{openai_cost}"));
```

- `crates/balanze_cli/src/statusline.rs`: `render_line` already loads settings, so it computes `want_openai` and threads it through `statusline_cross_provider(config)` → `self_compose_cross(now, want_openai)`.
- The freshness short-circuit in `statusline_cross_provider` stops treating `openai_stale` as a reason to self-compose when the value is not wanted: `if (!want_openai || !cross.openai_stale) && !cross.codex_stale { ... }`.
- `crates/statusline_render/src/self_compose.rs`: `self_compose` gains a `want_openai: bool` parameter and skips `openai_value` entirely when false, yielding `openai_cost_micro_usd: None, openai_stale: false`.

This honors AGENTS.md §3.1 in spirit as well as letter: the politest call to a provider is the one you do not make.

### Config-dir override

`settings::default_path()` has no override, unlike `BALANZE_DATA_DIR_OVERRIDE` and `BALANZE_CACHE_DIR_OVERRIDE`. Add `BALANZE_CONFIG_DIR_OVERRIDE`, following that existing convention, so an integration test can inject a `settings.json` with a custom template. Without it the self-compose integration test cannot exercise the `openai_cost` path at all once the segment leaves the default line.

## Testing

**`crates/statusline_render/src/render.rs` (unit):**

- The glyph table above: each segment renders with its new glyph and exactly one following space.
- Spacing invariant: in a rendered default line, every glyph is followed by exactly one space, and the line contains no double spaces and none of the retired glyphs (`⌛`, `📅`, `◇`, bare `⚠`, the literal `OpenAI`).
- Codex weekly renders `🌀 7d`, not `🌀 wk`. This is the cross-surface parity assertion; its sibling lives at `balanze_cli/src/render.rs:1127`.
- Existing severity-band and rounding tests keep their assertions. Those asserting `◇5h 80%` update to `🌀 5h 80%`.

**`crates/settings/src/statusline.rs` (unit):**

- `default_lines()` no longer contains `{openai_cost}`.

**`crates/balanze_cli/src/statusline.rs` (unit):**

- `want_openai` is false for the default template and true for a template containing `{openai_cost}`.
- The freshness short-circuit returns a snapshot whose only staleness is `openai_stale` when `want_openai` is false.
- `cross_renders_codex_and_openai_segments` (currently asserts `◇5h 6%` and `OpenAI $4.20`) moves to an explicit template containing `{openai_cost}` and asserts `🌀 5h 6%` and `🌀 $4.20`.

**`crates/balanze_cli/tests/integration_statusline_self_compose.rs` (integration):**

- Writes a `settings.json` into a temp config dir with a template containing `{openai_cost}`, points the binary at it with `BALANZE_CONFIG_DIR_OVERRIDE`, and keeps both existing assertions intact: the segment renders, and two renders inside the TTL produce exactly one upstream GET (the 300s gate).
- A second case asserts the inverse: with the **default** template, two renders produce **zero** upstream GETs. This is the regression test for the demand gate.

## Migration

No `settings.json` exists on the maintainer's machine (`%APPDATA%\me.oszkar.Balanze\config\`), so the new default reaches him immediately.

Anyone who has saved settings has `lines` serialized into their file and keeps the old template until they edit or delete that key. This is documented in the changelog rather than migrated: a "rewrite the value if it byte-matches the old default" migration is speculative machinery for a user base of one, and AGENTS.md §2 calls for YAGNI.

The glyph and spacing changes are unaffected by this, because the glyphs live in `render.rs`, not in the template. Only the removal of `{openai_cost}` is defeated by a persisted `lines`.

## Files touched

- `crates/statusline_render/src/render.rs` - glyphs, labels, stale marker
- `crates/statusline_render/src/self_compose.rs` - `want_openai` parameter
- `crates/settings/src/statusline.rs` - `default_lines()`, module doc
- `crates/settings/src/lib.rs` - `BALANZE_CONFIG_DIR_OVERRIDE` in `default_path()`
- `crates/balanze_cli/src/statusline.rs` - demand gate, freshness short-circuit, tests
- `crates/balanze_cli/tests/integration_statusline_self_compose.rs` - config-dir injection, gate regression test
- `docs/TROUBLESHOOTING.md:45` - the cross-provider-segments entry names the OpenAI `$` segment as appearing by default
- `docs/GUIDE.md:80` - describes the statusline as carrying "real OpenAI spend"
- `README.md:120` - same claim, and calls the Codex windows "5h and weekly" rather than 5h and 7d
- `CHANGELOG.md` - the note that a persisted `statusline.lines` pins the old template

The three doc lines all advertise OpenAI spend as a default statusline segment. Each is reworded to describe it as available but off by default, and the Codex windows are named `5h` and `7d` to match the new labels and the existing CLI surface.

## Out of scope

- An OpenAI budget setting (would let the segment show a percent and pick up the shared 50/75/90 severity band). This is a `Settings` schema change and needs its own review per AGENTS.md §8.
- Reinstating `openai_cost` with a month period and a countdown to the 1st (`🌀 mo $0.00 (17d)`). Considered and declined; the segment leaves the default line instead.
