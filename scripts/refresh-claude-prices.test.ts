import { afterEach, describe, expect, it, vi } from 'vitest';
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync, renameSync, unlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { prepareSnapshot, publishSnapshot, refresh } from './refresh-claude-prices.mjs';

const commit = 'a'.repeat(40);
const date = '2026-10-20';
const entry = {
  input_cost_per_token: 0.000003,
  output_cost_per_token: 0.000015,
  cache_read_input_token_cost: 0.0000003,
  litellm_provider: 'anthropic',
};
const previous = {
  _meta: { commit: 'b'.repeat(40), fetched_at: '2026-09-16' },
  'claude-old': entry,
};
const roots: string[] = [];

function fixture() {
  const root = mkdtempSync(join(tmpdir(), 'balanze-prices-'));
  roots.push(root);
  const data = join(root, 'crates/claude_cost/data');
  const src = join(root, 'crates/claude_cost/src');
  mkdirSync(data, { recursive: true });
  mkdirSync(src, { recursive: true });
  const oldPath = join(data, 'litellm-prices-bbbbbbb-20260916.json');
  const sourcePath = join(src, 'prices.rs');
  writeFileSync(oldPath, JSON.stringify(previous));
  writeFileSync(sourcePath, 'const JSON: &str = include_str!("../data/litellm-prices-bbbbbbb-20260916.json");\r\n');
  const state = () => ({
    files: readdirSync(data),
    json: readFileSync(oldPath, 'utf8'),
    source: readFileSync(sourcePath, 'utf8'),
  });
  return { root, data, oldPath, sourcePath, state };
}

afterEach(() => {
  for (const root of roots.splice(0)) {
    // Only delete the exact temporary workspace created by this test.
    expect(root.startsWith(join(tmpdir(), 'balanze-prices-'))).toBe(true);
    rmSync(root, { recursive: true, force: true });
  }
});

describe('Claude price refresh', () => {
  it('filters provider variants and unrelated fields, retaining all supported tier rates', () => {
    const tier = {
      ...entry,
      input_cost_per_token_above_100k_tokens: 0.000006,
      output_cost_per_token_above_100k_tokens: 0.00003,
      cache_creation_input_token_cost_above_100k_tokens: 0.0000075,
      cache_read_input_token_cost_above_100k_tokens: 0.0000006,
      input_cost_per_token_batches: 1,
    };
    const { snapshot, added } = prepareSnapshot({
      'claude-new': tier,
      'vendor/claude-new': entry,
      'claude-new-v1:0': entry,
      'claude-other': { ...entry, litellm_provider: 'bedrock' },
      'gpt-test': entry,
    }, previous, commit, date);
    expect(added).toEqual(['claude-new']);
    expect(Object.keys(snapshot)).toEqual(['_meta', 'claude-new', 'claude-old']);
    expect(snapshot['claude-new']).not.toHaveProperty('input_cost_per_token_batches');
    expect(snapshot['claude-new'].cache_creation_input_token_cost_above_100k_tokens).toBe(0.0000075);
    expect(snapshot['claude-new'].cache_read_input_token_cost_above_100k_tokens).toBe(0.0000006);
  });

  it.each([
    { input_cost_per_token: -1 },
    { output_cost_per_token: '0.01' },
    { cache_read_input_token_cost: 1 },
    { input_cost_per_token: Infinity },
    { input_cost_per_token: null },
    { input_cost_per_token_above_100k_tokens: 0.000006 },
    { cache_read_input_token_cost_above_100k_tokens: 0.000001 },
  ])('rejects malformed rates or incomplete tiers: %j', (invalid) => {
    expect(() => prepareSnapshot({ 'claude-new': { ...entry, ...invalid } }, previous, commit, date)).toThrow();
  });

  it('reports removed optional rates and changed tier rates', () => {
    const old = { ...entry, input_cost_per_token_above_100k_tokens: 0.000006, output_cost_per_token_above_100k_tokens: 0.00003 };
    const next = { ...old, cache_read_input_token_cost: null, output_cost_per_token_above_100k_tokens: 0.00004 };
    const result = prepareSnapshot({ 'claude-old': next }, { ...previous, 'claude-old': old }, commit, date);
    expect(result.changed).toEqual([
      'claude-old cache_read_input_token_cost: 3e-7 -> (absent)',
      'claude-old output_cost_per_token_above_100k_tokens: 0.00003 -> 0.00004',
    ]);
  });

  it('preserves historical entries and original retention provenance through repeated refreshes', () => {
    const first = prepareSnapshot({ 'claude-new': entry }, previous, commit, date).snapshot;
    const second = prepareSnapshot({ 'claude-new': entry }, first, 'c'.repeat(40), date).snapshot;
    expect(second['claude-old']).toEqual(entry);
    expect(second._meta.retained_models['claude-old']).toEqual(previous._meta);
    expect(second._meta).not.toHaveProperty('verified_against');
    expect(second._meta.verification_note).toContain('Not yet cross-checked');
  });

  it('rejects empty filtered input and mutable commit references', () => {
    expect(() => prepareSnapshot({}, previous, commit, date)).toThrow('no eligible');
    expect(() => prepareSnapshot({ 'claude-new': entry }, previous, 'main', date)).toThrow('commit SHA');
  });

  it('prints a diff but leaves files unchanged when a model disappears', async () => {
    const f = fixture();
    const before = f.state();
    const log = vi.fn();
    await expect(refresh({
      root: f.root, commit, date, log,
      fetcher: async () => new Response(JSON.stringify({ 'claude-new': entry })),
    })).rejects.toThrow('Model keys disappeared');
    expect(log).toHaveBeenCalledWith('Removed upstream (1): claude-old');
    expect(f.state()).toEqual(before);
  });

  it('previews, then publishes consistent metadata, filename and include path', async () => {
    const f = fixture();
    const before = f.state();
    const options = {
      root: f.root, commit, date, retainMissing: true, log: vi.fn(),
      fetcher: vi.fn(async () => new Response(JSON.stringify({ 'claude-new': entry }))),
    };
    await refresh({ ...options, dryRun: true });
    expect(f.state()).toEqual(before);
    const plan = await refresh(options);
    expect(options.fetcher.mock.calls[0][0]).toContain(`/${commit}/model_prices_and_context_window.json`);
    expect(readdirSync(f.data)).toEqual(['litellm-prices-aaaaaaa-20261020.json']);
    const json = JSON.parse(readFileSync(plan.newPath, 'utf8'));
    expect(json._meta.commit).toBe(commit);
    expect(json._meta.fetched_at).toBe(date);
    expect(json['claude-old']).toEqual(entry);
    expect(readFileSync(f.sourcePath, 'utf8')).toBe('const JSON: &str = include_str!("../data/litellm-prices-aaaaaaa-20261020.json");\r\n');
    await refresh(options); // Same commit/date can be refreshed without deleting the new snapshot.
    expect(readdirSync(f.data)).toEqual(['litellm-prices-aaaaaaa-20261020.json']);
  });

  it.each(['http', 'json', 'include', 'multiple'])('leaves files unchanged on %s failure', async (failure) => {
    const f = fixture();
    if (failure === 'include') writeFileSync(f.sourcePath, 'unexpected source');
    if (failure === 'multiple') writeFileSync(join(f.data, 'litellm-prices-extra.json'), '{}');
    const before = f.state();
    await expect(refresh({
      root: f.root, commit, date, log: vi.fn(),
      fetcher: async () => failure === 'http'
        ? new Response('', { status: 404 }) : new Response('{invalid'),
    })).rejects.toThrow();
    expect(f.state()).toEqual(before);
  });

  it.each(['source', 'delete'])('rolls back publication if %s update fails', async (failure) => {
    const f = fixture();
    const before = f.state();
    const plan = await refresh({
      root: f.root, commit, date, dryRun: true, retainMissing: true, log: vi.fn(),
      fetcher: async () => new Response(JSON.stringify({ 'claude-new': entry })),
    });
    expect(() => publishSnapshot(plan, {
      writeFileSync,
      renameSync: (from: string, to: string) => {
        if (failure === 'source' && to === f.sourcePath) throw new Error('injected source failure');
        renameSync(from, to);
      },
      unlinkSync: (path: string) => {
        if (failure === 'delete' && path === f.oldPath) throw new Error('injected delete failure');
        unlinkSync(path);
      },
    })).toThrow('injected');
    expect(f.state()).toEqual(before);
  });
});
