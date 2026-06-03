# Claude Code — Project Instructions

See @AGENTS.md for the operational contract: prime rule, engineering principles, project conventions, non-negotiables, repo map, architectural boundaries, validation matrix, test discipline, change control, communication style, and troubleshooting.

`AGENTS.md` is the single source of truth for code-discipline rules. Anything Claude-specific that doesn't belong there can be added below.

## gstack

This project uses [gstack](https://github.com/garrytan/gstack) for browser-driven QA and dogfooding.

- **Web browsing:** use the `/browse` skill. **Never** use `mcp__claude-in-chrome__*` tools.
- **Install (one-time, per machine):**
  ```bash
  git clone --single-branch --depth 1 https://github.com/garrytan/gstack.git ~/.claude/skills/gstack
  cd ~/.claude/skills/gstack && ./setup    # requires bun
  ```
- **Stay current:** run `/gstack-upgrade` anytime.

Key skills: `/browse`, `/connect-chrome`, `/qa`, `/qa-only`, `/review`, `/ship`, `/land-and-deploy`, `/canary`, `/benchmark`, `/investigate`, `/design-review`, `/design-consultation`, `/design-shotgun`, `/design-html`, `/devex-review`, `/plan-ceo-review`, `/plan-eng-review`, `/plan-design-review`, `/plan-devex-review`, `/document-release`, `/document-generate`, `/retro`, `/office-hours`, `/codex`, `/cso`, `/learn`, `/careful`, `/freeze`, `/guard`, `/unfreeze`, `/setup-browser-cookies`, `/setup-deploy`, `/setup-gbrain`, `/gstack-upgrade`. Run a skill with no args for its help, or see `~/.claude/skills/gstack` for the full set.
