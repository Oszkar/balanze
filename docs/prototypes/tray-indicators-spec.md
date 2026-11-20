# Tray indicator native validation

The implemented indicator behavior is described in the [user guide](../src/GUIDE.md#tray-indicators) and [architecture](../ARCHITECTURE.md#tray-presentation-settings). This checklist tracks the remaining native validation for the unreleased tray changes.

The maintainer confirmed that the macOS behavior works well. On Windows, the maintainer tested an installed build from current source and found the indicators small but legible at the current display scale. Each separate icon needed to be pinned individually. Switching to highest-utilization mode removed one icon as expected, and hiding a provider removed its corresponding icon.

For remaining Windows validation, check the installed build or run `bun run tauri dev`:

- Both items open the popover; right-click menus offer Open Balanze and Quit.
- Switch separate to combined and back repeatedly. Confirm no duplicate or orphaned items and that popover access continues.
- Disable and re-enable Codex scanning. Confirm only its separate item disappears and reappears.
- Quit removes both items and exits cleanly.
- Check A/O recognition at 100%, 125%, 150%, and 200% display scaling, in light and dark taskbars.
- Check fill visibility at low, medium, and near-full values; missing quota remains distinguishable from empty usage.
- Check tooltips identify the correct product, selected window, utilization, and stale/error state.
- Check each item remains identifiable if Windows reorders it or places it in the overflow area.

Cross-surface parity coverage is tracked in [#263](https://github.com/Oszkar/balanze/issues/263).
