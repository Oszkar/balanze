# Tray indicators and compact macOS text

Approved by the maintainer on 2026-11-19 for [tray indicators #295](https://github.com/Oszkar/balanze/issues/295) and [compact macOS text #296](https://github.com/Oszkar/balanze/issues/296). Both are high priority and share the provider/window selection policy. Native Windows validation will follow on the maintainer's machine.

## Selected design

- Separate native filling bars, one for Claude and one for Codex, are the default.
- macOS uses **Cla/Cdx**, with percentages off by default and an option to show them.
- Windows uses **A/O beside the bars**, the B variant in the [Windows comparison](windows-tray.html). These are ordinary letters, not official logos. Tooltips identify Anthropic / Claude quota and OpenAI / Codex quota, avoiding confusion with API billing.
- An optional combined mode shows the provider with the highest utilization.
- Each item opens the same popover anchored at the click position and offers Open Balanze / Quit.
- The [original comparison board](tray-indicators.html) preserves the visual exploration. Its alternative variants and browser geometry are not the implementation contract.

## Settings and lifecycle

The approved additive settings object is:

```json
{
  "tray": {
    "mode": "separate",
    "show_percentages": false
  }
}
```

Mode accepts `separate` or `combined`. Missing fields receive the same defaults for existing and new installations. Settings version remains 2. Both nested fields can be independently patched through the existing `get_settings` / `set_settings` IPC.

The percentage flag controls native macOS titles only. It is persisted but unused on Windows, where exact percentages appear in tooltips. The checkbox is shown only on macOS.

The existing main tray item remains available in both modes. Separate mode creates the Codex item when Codex scanning is enabled; disabling Codex or choosing combined mode removes it. Claude remains accessible even when its quota is unavailable. App-scoped handlers register once, preventing repeated mode changes from accumulating callbacks.

Committed settings reach the presentation sink before its snapshot repaint and command acknowledgment. Native mutations run on the existing supervised painter worker. No new dependencies, capabilities, Snapshot fields, StateMsg variants, polling, or credentials behavior.

## Presentation semantics

1. Each provider uses its highest-utilization eligible window. The tooltip retains the actual window label, including model-specific or unknown windows.
2. Existing Anthropic freshness and fallback selection, Codex expiry, and rounded 50/75/90 severity thresholds remain in force.
3. Fill clamps to 0-100%; text preserves the rounded reported value, including over 100%. Empty quota is never displayed as zero.
4. Missing quota uses a neutral dash, connecting uses dots, and an error without data uses an exclamation mark. Stale data retains its last fill with violet warning color and an exclamation marker. macOS also appends `!` to degraded titles.
5. Separate mode isolates degradation. A healthy selected fallback stays healthy despite errors in an unused source. Billing and activity failures remain visible in the popover.
6. Combined mode selects raw utilization before rounding, with Codex winning ties. A stale/error participating provider adds a warning even when the winning value is healthy.
7. Repaint equality includes provider, fill, state, color, title, and tooltip. Per-property successful OS writes are retained across retries. Topology changes cannot be deduped merely because the main item is unchanged. There is no new timer.

## Implementation and validation

Implementation spans settings, the coordinator's presentation hook, Tauri tray rendering/lifecycle, and the Settings view. The general presentation reducer and unrelated provider work remain out of scope.

Automated checks cover old settings defaults, independent partial patches and persistence, settings-before-snapshot ordering, healthy fallback selection, provider degradation isolation, combined selection, disabled Codex, fill/identity changes, missing versus zero, topology changes, and retries after partial setter success.

The local workspace build, clippy, formatting, 912 Rust tests (2 intentionally skipped), frontend typecheck, and 96 frontend tests pass. The development app launches, and the maintainer confirmed both native macOS indicators appear. Automated native capture was unavailable; the remaining interaction checks below are not yet certified.

## Native acceptance checklist

On macOS and Windows, run `bun run tauri dev` and check:

- Both items open the popover; right-click menus offer Open Balanze and Quit.
- Switch separate → combined → separate repeatedly. Confirm no duplicate or orphaned items and that popover access continues.
- On macOS, toggle percentages and restart. Confirm mode and text preferences persist.
- Disable/re-enable Codex scanning. Confirm only its separate item disappears/reappears.
- Quit removes both items and exits cleanly.

On Windows additionally inspect:

- A/O recognition at 100%, 125%, 150%, and 200% display scaling, in light and dark taskbars.
- Fill visibility at low, medium, and near-full values; missing quota remains distinguishable from empty usage.
- Tooltips identify the correct product, selected window, utilization, and stale/error state.
- Each item remains identifiable if Windows reorders it or places it in the overflow area.

Use [#263](https://github.com/Oszkar/balanze/issues/263) for cross-surface parity coverage. Browser mockups establish design preference; native checks establish actual tray readability and interaction behavior.
