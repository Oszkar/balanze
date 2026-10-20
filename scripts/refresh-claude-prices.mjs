#!/usr/bin/env node
// Maintainer-only vendoring tool. Runtime pricing remains entirely offline.
import { readFileSync, readdirSync, writeFileSync, renameSync, unlinkSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const baseRates = [
  'input_cost_per_token', 'output_cost_per_token',
  'cache_creation_input_token_cost', 'cache_read_input_token_cost',
];
const tierRates = baseRates.map((field) => `${field}_above_100k_tokens`);
const rates = [...baseRates, ...tierRates];
const fields = [...rates, 'max_input_tokens', 'max_output_tokens', 'litellm_provider'];
const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const usage = `Usage: node scripts/refresh-claude-prices.mjs <full-commit-sha> [--dry-run] [--retain-missing]

Fetch a pinned LiteLLM snapshot and print added / removed / changed rates.
--dry-run         Preview without writing (missing keys still fail by default).
--retain-missing  Carry missing historical entries forward unchanged.

Cross-check rates against Anthropic and update verification metadata manually.
`;

function object(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value;
}

function validateEntry(entry, model) {
  object(entry, model);
  for (const field of rates) {
    const value = entry[field];
    if (value == null && !baseRates.slice(0, 2).includes(field)) continue;
    if (typeof value !== 'number' || !Number.isFinite(value) || value < 0 || value >= 1) {
      throw new Error(`${model}: ${field} must be a finite rate in [0, 1) USD/token`);
    }
  }
  if (tierRates.some((field) => entry[field] != null)
      && tierRates.slice(0, 2).some((field) => entry[field] == null)) {
    throw new Error(`${model}: incomplete above-100k tier`);
  }
}

export function prepareSnapshot(upstream, previous, commit, date) {
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error('Supply a full lowercase 40-character commit SHA');
  object(upstream, 'Upstream JSON');
  object(previous, 'Previous snapshot');
  const models = {};
  for (const key of Object.keys(upstream).sort()) {
    const entry = upstream[key];
    if (!key.startsWith('claude-') || key.includes('/') || key.includes(':')
        || !entry || entry.litellm_provider !== 'anthropic') continue;
    validateEntry(entry, key);
    models[key] = Object.fromEntries(fields.filter((field) => Object.hasOwn(entry, field))
      .map((field) => [field, entry[field]]));
  }
  if (!Object.keys(models).length) throw new Error('Upstream contains no eligible Claude models');
  const oldKeys = Object.keys(previous).filter((key) => key !== '_meta').sort();
  for (const key of oldKeys) validateEntry(previous[key], key);
  const added = Object.keys(models).filter((key) => !Object.hasOwn(previous, key));
  const removed = oldKeys.filter((key) => !Object.hasOwn(models, key));
  const changed = [];
  for (const key of Object.keys(models)) {
    if (!Object.hasOwn(previous, key)) continue;
    for (const field of rates) {
      const before = previous[key][field] ?? null;
      const after = models[key][field] ?? null;
      if (before !== after) changed.push(`${key} ${field}: ${before ?? '(absent)'} -> ${after ?? '(absent)'}`);
    }
  }
  // Retention is prepared here but must be explicitly authorized by the caller.
  const retained = {};
  for (const key of removed) {
    models[key] = previous[key];
    retained[key] = previous._meta?.retained_models?.[key] ?? {
      commit: previous._meta?.commit,
      fetched_at: previous._meta?.fetched_at,
    };
    if (!retained[key].commit || !retained[key].fetched_at) {
      throw new Error(`Missing provenance for retained model ${key}`);
    }
  }
  const snapshot = {
    _meta: {
      source: 'https://github.com/BerriAI/litellm',
      commit,
      fetched_at: date,
      source_file: 'model_prices_and_context_window.json',
      filter: 'bare claude-* keys, anthropic provider only (no slashes or colon variants); non-batch base and above-100k rates',
      license: 'MIT (see LICENSE-LITELLM)',
      verification_note: 'Not yet cross-checked against Anthropic. Maintainer verification required before merge.',
      ...(removed.length ? { retained_models: retained } : {}),
    },
    ...Object.fromEntries(Object.keys(models).sort().map((key) => [key, models[key]])),
  };
  return { snapshot, added, removed, changed };
}

// The temporary files are siblings so each rename stays on the same filesystem.
// Multiple files cannot be committed atomically; restore the old state on ordinary
// I/O errors. A process crash can still require restoring these paths from git.
export function publishSnapshot(plan, io = { writeFileSync, renameSync, unlinkSync }) {
  const suffix = `.refresh-${process.pid}.tmp`;
  const dataTemp = `${plan.newPath}${suffix}`;
  const sourceTemp = `${plan.sourcePath}${suffix}`;
  const samePath = plan.newPath === plan.oldPath;
  let dataPublished = false;
  let sourcePublished = false;
  try {
    io.writeFileSync(dataTemp, plan.json, { flag: 'wx' });
    io.writeFileSync(sourceTemp, plan.source, { flag: 'wx' });
    io.renameSync(dataTemp, plan.newPath);
    dataPublished = true;
    io.renameSync(sourceTemp, plan.sourcePath);
    sourcePublished = true;
    if (!samePath) io.unlinkSync(plan.oldPath);
  } catch (error) {
    const failures = [];
    try {
      if (sourcePublished) io.writeFileSync(plan.sourcePath, plan.oldSource);
    } catch (rollbackError) { failures.push(rollbackError.message); }
    try {
      if (dataPublished) {
        if (samePath) io.writeFileSync(plan.oldPath, plan.oldJson);
        else io.unlinkSync(plan.newPath);
      }
    } catch (rollbackError) { failures.push(rollbackError.message); }
    if (failures.length) {
      throw new Error(`${error.message}; rollback failed: ${failures.join('; ')}. Restore snapshot and prices.rs from git.`);
    }
    throw error;
  } finally {
    for (const path of [dataTemp, sourceTemp]) {
      try { io.unlinkSync(path); } catch (error) {
        if (error.code !== 'ENOENT') console.error(`Could not remove temporary file ${path}: ${error.message}`);
      }
    }
  }
}

export async function refresh({
  root = repoRoot, commit, dryRun = false, retainMissing = false,
  fetcher = fetch, date = new Date().toISOString().slice(0, 10), log = console.log,
}) {
  if (!/^[a-f0-9]{40}$/.test(commit ?? '')) throw new Error('Supply a full lowercase 40-character commit SHA');
  const dataDir = join(root, 'crates/claude_cost/data');
  const files = readdirSync(dataDir).filter((name) => /^litellm-prices-.*\.json$/.test(name));
  if (files.length !== 1) throw new Error(`Expected exactly one snapshot, found ${files.length}`);
  const oldPath = join(dataDir, files[0]);
  const oldJson = readFileSync(oldPath, 'utf8');
  const sourcePath = join(root, 'crates/claude_cost/src/prices.rs');
  const oldSource = readFileSync(sourcePath, 'utf8');
  const include = `include_str!("../data/${files[0]}")`;
  if (oldSource.split(include).length !== 2) throw new Error('Expected exactly one matching include_str! path');
  const response = await fetcher(
    `https://raw.githubusercontent.com/BerriAI/litellm/${commit}/model_prices_and_context_window.json`,
    { signal: AbortSignal.timeout(30_000), redirect: 'error' },
  );
  if (!response.ok) throw new Error(`LiteLLM fetch failed: HTTP ${response.status}`);
  const { snapshot, added, removed, changed } = prepareSnapshot(await response.json(), JSON.parse(oldJson), commit, date);
  log(`Added (${added.length}): ${added.join(', ') || '(none)'}`);
  log(`Removed upstream (${removed.length}): ${removed.join(', ') || '(none)'}`);
  log(`Changed rates (${changed.length}):\n${changed.join('\n') || '(none)'}`);
  if (removed.length && !retainMissing) {
    throw new Error('Model keys disappeared. Review the diff and use --retain-missing to preserve historical pricing. No files changed.');
  }
  if (removed.length) log('Missing models will be retained unchanged with their previous provenance.');
  const filename = `litellm-prices-${commit.slice(0, 7)}-${date.replaceAll('-', '')}.json`;
  const plan = {
    oldPath, newPath: join(dataDir, filename), sourcePath, oldJson, oldSource,
    json: `${JSON.stringify(snapshot, null, 2)}\n`,
    source: oldSource.replace(include, `include_str!("../data/${filename}")`),
  };
  if (!dryRun) publishSnapshot(plan);
  log(`${dryRun ? 'Preview' : 'Wrote'}: ${filename}`);
  log('Before merge: cross-check Anthropic rates, update verification metadata and README provenance, review upstream LICENSE, and run the documented checks.');
  return plan;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  if (args.length === 1 && args[0] === '--help') {
    console.log(usage);
  } else {
    const [commit, ...flags] = args;
    if (flags.some((flag) => !['--dry-run', '--retain-missing'].includes(flag))) {
      console.error(usage);
      process.exitCode = 1;
    } else {
      refresh({ commit, dryRun: flags.includes('--dry-run'), retainMissing: flags.includes('--retain-missing') })
        .catch((error) => { console.error(error.message); process.exitCode = 1; });
    }
  }
}
