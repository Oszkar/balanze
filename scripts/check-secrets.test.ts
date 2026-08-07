import { afterEach, describe, expect, it } from 'vitest';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const scanner = resolve('scripts/check-secrets.mjs');
const workspaces: string[] = [];
const fakeKey = ['sk', 'x'.repeat(40)].join('-');

function git(cwd: string, ...args: string[]) {
  const result = spawnSync('git', args, { cwd, encoding: 'utf8' });
  expect(result.status, result.stderr).toBe(0);
}

function stagedScan(
  path: string,
  contents: string | Uint8Array = `credential=${fakeKey}\n`,
  baselineContents?: string | Uint8Array,
) {
  const cwd = mkdtempSync(join(tmpdir(), 'balanze-secret-scan-'));
  workspaces.push(cwd);
  git(cwd, 'init', '--quiet');
  const target = join(cwd, path);
  mkdirSync(dirname(target), { recursive: true });
  if (baselineContents !== undefined) {
    writeFileSync(target, baselineContents);
    git(cwd, 'add', '--', path);
    git(
      cwd,
      '-c',
      'user.name=Balanze Tests',
      '-c',
      'user.email=tests@example.invalid',
      'commit',
      '--quiet',
      '-m',
      'baseline',
    );
  }
  writeFileSync(target, contents);
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

  it('allows a benign env example file', () => {
    expect(stagedScan('.env.example', 'BALANZE_LOG=debug\n').status).toBe(0);
  });

  it.each(['.env.example', 'fixtures/config.example.json'])(
    'scans secret-bearing example content in %s',
    (path) => {
      const result = stagedScan(path);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain('Potential secrets detected');
      expect(result.stderr).not.toContain(fakeKey);
    },
  );

  it.each(['.envrc', '.envrc.local'])('blocks staged %s files by name', (path) => {
    const result = stagedScan(path, 'export BALANZE_LOG=debug\n');
    expect(result.status).toBe(1);
    expect(result.stderr).toContain('Attempted to commit .env file(s)');
    expect(result.stderr).toContain(path);
  });

  it('allows a tracked binary update whose bytes pass the secret patterns', () => {
    const baseline = new Uint8Array([0, 1, 2, 3]);
    const update = new Uint8Array([0, 1, 2, 4]);
    expect(stagedScan('fixtures/blob.bin', update, baseline).status).toBe(0);
  });

  it('blocks and redacts a secret-shaped value in a staged binary file', () => {
    const body = new Uint8Array([0, ...Buffer.from(fakeKey)]);
    const result = stagedScan('fixtures/blob.bin', body);
    expect(result.status).toBe(1);
    expect(result.stderr).toContain('Potential secrets detected in staged binary file(s)');
    expect(result.stderr).toContain('fixtures/blob.bin');
    expect(result.stderr).not.toContain(fakeKey);
  });
});
