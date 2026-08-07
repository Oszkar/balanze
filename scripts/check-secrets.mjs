#!/usr/bin/env node
// Pre-commit hook: block commits containing secrets or .env files.
// Scans only staged content (git diff --cached) so it won't flag your working tree.
// Plain dependency-free JS - invoked via `bun scripts/check-secrets.mjs` from
// lefthook.yml (runs under node too; only uses node:child_process).

import { execFileSync, execSync } from 'node:child_process';

const RED = '\x1b[0;31m';
const YELLOW = '\x1b[1;33m';
const NC = '\x1b[0m';
const MAX_STAGED_BINARY_BYTES = 64 * 1024 * 1024;

let errors = 0;

function run(cmd) {
  try {
    return execSync(cmd, { encoding: 'utf-8' }).trim();
  } catch (err) {
    // Fail closed: a secret gate that cannot inspect the staged content must
    // block the commit, not silently wave it through.
    console.error(`${RED}ERROR: secret scan could not inspect staged changes:${NC} ${cmd}`);
    console.error(String(err?.message ?? err));
    process.exit(1);
  }
}

function readStagedBlob(path) {
  try {
    return execFileSync('git', ['show', `:${path}`], {
      maxBuffer: MAX_STAGED_BINARY_BYTES,
    });
  } catch (err) {
    // Fail closed if the exact staged bytes cannot be inspected. In
    // particular, do not fall back to the potentially different working copy.
    console.error(`${RED}ERROR: secret scan could not inspect staged binary:${NC} ${path}`);
    console.error(String(err?.message ?? err));
    process.exit(1);
  }
}

// ---------------------------------------------------------------------------
// 1. Block .env files (someone might `git add -f .env`)
// ---------------------------------------------------------------------------
const stagedFiles = run('git diff --cached --name-only --diff-filter=ACMR');
const envFiles = stagedFiles
  .split('\n')
  .filter((f) => f && /(?:^|\/)\.env(?:rc)?(?:$|\..*)/.test(f) && !f.endsWith('.example'));

if (envFiles.length > 0) {
  console.error(`${RED}ERROR: Attempted to commit .env file(s):${NC}`);
  envFiles.forEach((f) => console.error(`  - ${f}`));
  console.error(`${YELLOW}Hint: .env is gitignored on purpose (AGENTS.md 3.4) - never commit real .env files.${NC}`);
  errors++;
}

// ---------------------------------------------------------------------------
// 2. Define common secret patterns
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

// Test each pattern individually (a single combined RegExp would drop
// per-pattern flags like /i).
const matchesSecret = (line) => SECRET_PATTERNS.some((re) => re.test(line));

// Mask every matched span before printing - the hook must not re-expose the
// very secret it just caught (AGENTS.md 3.4: never log secrets).
function redact(line) {
  let out = line;
  for (const re of SECRET_PATTERNS) {
    const g = new RegExp(re.source, re.flags.includes('g') ? re.flags : `${re.flags}g`);
    out = out.replace(g, (m) => `${m.slice(0, 4)}***REDACTED***`);
  }
  return out;
}

// ---------------------------------------------------------------------------
// 3. Scan the exact staged bytes of binary files
// ---------------------------------------------------------------------------
// Unified diffs expose only "Binary files differ". Read each staged Git blob
// directly and decode it byte-for-byte so ASCII secret signatures still reach
// the same matcher. --no-renames makes the numstat path the staged destination.
const binaryFiles = run('git diff --cached --numstat --no-renames --diff-filter=ACMR')
  .split('\n')
  .filter((line) => line.startsWith('-\t-\t'))
  .map((line) => line.slice(4));
const binaryMatches = binaryFiles.filter((path) =>
  matchesSecret(readStagedBlob(path).toString('latin1')),
);

if (binaryMatches.length > 0) {
  console.error(`${RED}ERROR: Potential secrets detected in staged binary file(s):${NC}`);
  binaryMatches.forEach((f) => console.error(`  - ${f}`));
  console.error(`${YELLOW}Hint: remove or rotate the secret before committing the binary.${NC}`);
  errors++;
}

// ---------------------------------------------------------------------------
// 4. Scan staged text diffs
// ---------------------------------------------------------------------------
// Only scan staged diff, skipping this script itself. Example and Markdown
// content stays in scope because a pasted real key is still a leak.
const diff = run(
  'git diff --cached -U0 --diff-filter=ACMR -- . ":!scripts/check-secrets.mjs"',
);

if (diff) {
  const matches = diff
    .split('\n')
    .filter((line) => line.startsWith('+') && !line.startsWith('+++') && matchesSecret(line))
    .slice(0, 20);

  if (matches.length > 0) {
    console.error(`${RED}ERROR: Potential secrets detected in staged changes:${NC}`);
    matches.forEach((m) => console.error(`  ${redact(m)}`));
    console.error(`${YELLOW}Hint: if this is a false positive, narrow SECRET_PATTERNS in scripts/check-secrets.mjs.${NC}`);
    errors++;
  }
}

// ---------------------------------------------------------------------------
// 5. Exit
// ---------------------------------------------------------------------------
if (errors > 0) {
  console.error(`${RED}Commit blocked. Fix the issues above before committing.${NC}`);
  process.exit(1);
}
