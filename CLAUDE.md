# Claude Code - Project Instructions

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

## GBrain Configuration (configured by /setup-gbrain)

- Mode: local-stdio
- Engine: postgres
- Config file: `~/.gbrain/config.json` (mode 0600)
- Setup date: 2026-08-21
- MCP registered: yes (user scope, absolute path `C:/Users/oszka/.bun/bin/gbrain.exe`)
- Artifacts repo: `https://github.com/Oszkar/gstack-artifacts-oszka`
- Artifacts sync: full
- Current repo policy: read-write

## GBrain Search Guidance (configured by /sync-gbrain)
<!-- gstack-gbrain-search-guidance:start -->

GBrain is set up and synced on this machine. The agent should prefer gbrain
over Grep when the question is semantic or when you don't know the exact
identifier yet.

**This worktree is pinned to a worktree-scoped code source** via the
`.gbrain-source` file in the repo root (kubectl-style context).
`gbrain code-def`, `code-refs`, `code-callers`, `code-callees`, `search`, and
`query` from anywhere under this worktree route to that source by default -
no `--source` flag needed. Sibling worktrees of the same repo each have their
own pin and indexed pages, so semantic results match the code on disk here.

Call-graph queries (`code-callers`/`code-callees`) also need the graph to be
built first - run `/sync-gbrain --dream` (or `--full`) if they return
`count: 0`. This only works if this source's GBrain schema pack extracts code
symbols. `code-def`/`code-refs` need the same extraction.

Two indexed corpora are available through the `gbrain` CLI:

- This worktree's code, auto-pinned to `gstack-code-balanze-0a18e314`.
- Curated gstack artifacts in the federated `gstack-artifacts-oszka` source.

Prefer gbrain when:

- "Where is X handled?" or semantic intent without an exact identifier:
  `gbrain search "<terms>"` or `gbrain query "<question>"`
- "Where is symbol Y defined?" or symbol-based code questions:
  `gbrain code-def <symbol>` or `gbrain code-refs <symbol>`
- "What calls Y?" or "What does Y depend on?":
  `gbrain code-callers <symbol>` or `gbrain code-callees <symbol>`
- "What did we decide last time?" or past plans, retros, and learnings:
  `gbrain search "<terms>" --source gstack-artifacts-oszka`

Grep is still right for known exact strings, regex, multiline patterns, and
file globs. Run `/sync-gbrain` after meaningful code changes. Do not run it
while `gbrain autopilot` is active because both operations can race source
maintenance.

<!-- gstack-gbrain-search-guidance:end -->
