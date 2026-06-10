#!/usr/bin/env node
// Pre-commit hook: block commits containing secrets or .env files.
// Scans only staged content (git diff --cached) so it won't flag your working tree.
// Plain dependency-free JS - invoked via `bun scripts/check-secrets.mjs` from
// lefthook.yml (runs under node too; only uses node:child_process).

import { execSync } from 'node:child_process';

const RED = '\x1b[0;31m';
const YELLOW = '\x1b[1;33m';
const NC = '\x1b[0m';

let errors = 0;

function run(cmd) {
  try {
    return execSync(cmd, { encoding: 'utf-8' }).trim();
  } catch {
    return '';
  }
}

// ---------------------------------------------------------------------------
// 1. Block .env files (someone might `git add -f .env`)
// ---------------------------------------------------------------------------
const stagedFiles = run('git diff --cached --name-only --diff-filter=ACMR');
const envFiles = stagedFiles
  .split('\n')
  .filter((f) => f && /(?:^|\/)\.env(?:$|\..*)/.test(f) && !f.endsWith('.example'));

if (envFiles.length > 0) {
  console.error(`${RED}ERROR: Attempted to commit .env file(s):${NC}`);
  envFiles.forEach((f) => console.error(`  - ${f}`));
  console.error(`${YELLOW}Hint: .env is gitignored on purpose (AGENTS.md 3.4) - never commit real .env files.${NC}`);
  errors++;
}

// ---------------------------------------------------------------------------
// 2. Scan staged diffs for common secret patterns
// ---------------------------------------------------------------------------
// Patterns are intentionally broad enough to catch real leaks but narrow
// enough to avoid false positives on example/placeholder values.
const SECRET_PATTERNS = [
  // OpenAI API keys (classic sk-..., plus sk-proj-/sk-svcacct-/sk-admin-
  // variants; also catches sk-ant-... Anthropic keys by shape)
  /sk-[A-Za-z0-9_-]{32,}/,
  // BALANZE_OPENAI_KEY assigned a literal value - the env override belongs in
  // the shell session, never in a committed file
  /BALANZE_OPENAI_KEY\s*=\s*["']?[A-Za-z0-9_-]{16,}/,
  // GitHub tokens
  /gh[pousr]_[A-Za-z0-9_]{36,}/,
  // Generic long high-entropy values assigned to secret-looking vars (>= 40 chars)
  /(?:SECRET|TOKEN|PASSWORD|API.?KEY|PRIVATE.KEY)\s*=\s*["']?[A-Za-z0-9+/=_-]{40,}/,
  // Generic "Bearer <JWT>" in code
  /Bearer\s+eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/,
];

const combined = new RegExp(SECRET_PATTERNS.map((r) => r.source).join('|'));

// Only scan staged diff, skip .example files, this script, and markdown
const diff = run(
  'git diff --cached -U0 --diff-filter=ACMR -- . ":!*.example" ":!*.example.*" ":!*check-secrets*" ":!*.md"',
);

if (diff) {
  const matches = diff
    .split('\n')
    .filter((line) => line.startsWith('+') && !line.startsWith('+++') && combined.test(line))
    .slice(0, 20);

  if (matches.length > 0) {
    console.error(`${RED}ERROR: Potential secrets detected in staged changes:${NC}`);
    matches.forEach((m) => console.error(`  ${m}`));
    console.error(`${YELLOW}Hint: if this is a false positive, narrow SECRET_PATTERNS in scripts/check-secrets.mjs.${NC}`);
    errors++;
  }
}

// ---------------------------------------------------------------------------
// 3. Exit
// ---------------------------------------------------------------------------
if (errors > 0) {
  console.error(`${RED}Commit blocked. Fix the issues above before committing.${NC}`);
  process.exit(1);
}
