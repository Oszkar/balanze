import { afterEach, describe, expect, it } from 'vitest';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const scanner = resolve('scripts/check-secrets.mjs');
const workspaces: string[] = [];

function git(cwd: string, ...args: string[]) {
  const result = spawnSync('git', args, { cwd, encoding: 'utf8' });
  expect(result.status, result.stderr).toBe(0);
}

function stagedScan(path: string) {
  const cwd = mkdtempSync(join(tmpdir(), 'balanze-secret-scan-'));
  workspaces.push(cwd);
  git(cwd, 'init', '--quiet');
  const target = join(cwd, path);
  mkdirSync(dirname(target), { recursive: true });
  const fakeKey = ['sk', 'x'.repeat(40)].join('-');
  writeFileSync(target, `credential=${fakeKey}\n`);
  git(cwd, 'add', '--', path);
  return spawnSync(process.execPath, [scanner], { cwd, encoding: 'utf8' });
}

afterEach(() => {
  for (const workspace of workspaces.splice(0)) rmSync(workspace, { recursive: true, force: true });
});

describe('check-secrets staged path exclusions', () => {
  it('excludes only the scanner source itself', () => {
    expect(stagedScan('scripts/check-secrets.mjs').status).toBe(0);
  });

  it('still scans similarly named files', () => {
    const result = stagedScan('scripts/check-secrets-notes.txt');
    expect(result.status).toBe(1);
    expect(result.stderr).toContain('Potential secrets detected');
    expect(result.stderr).not.toContain('sk-' + 'x'.repeat(40));
  });
});
