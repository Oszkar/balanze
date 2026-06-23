<script lang="ts">
  import type { Snapshot } from '$lib/types/snapshot';
  import { quotaTone, codexElapsedFraction, codexWindowExpired } from '$lib/presentation/quota';
  import { microUsdToDollars } from '$lib/presentation/format';
  import { PROV } from '$lib/presentation/provenance';
  import { anthropicQuotaState } from '$lib/presentation/cellState';
  import ProviderCard, { type CardWindow, type CardQuotaState } from './ProviderCard.svelte';

  // `openaiEnabled` = the OpenAI billing opt-in (`openai_enabled`); default false.
  let { snapshot, openaiEnabled = false, degraded = {} }:
    { snapshot: Snapshot; openaiEnabled?: boolean; degraded?: Record<string, string> } = $props();

  // Source order matches GridView's anthropicQuota(): the live statusline (5h/7d)
  // is preferred, the OAuth cadences are the fallback - so the two views never
  // disagree on which source they render. `anthStale` mirrors GridView: the
  // statusline went degraded and we are on the OAuth fallback, so Cards shows the
  // same stale cue (per-window "stale" instead of the reset countdown).
  const anthSource = $derived(snapshot.claude_statusline?.payload.rate_limits?.five_hour ? 'statusline' : 'oauth');
  const anthStale = $derived(!!degraded['claude_statusline'] && anthSource === 'oauth');

  const paceElapsed = (key: string): number | null => {
    const p = snapshot.pace.find((x) => x.key === key);
    return p ? p.elapsed_fraction * 100 : null;
  };
  // Each window carries its pace tick (looked up by key) and the matching
  // provenance tooltip for its source. Cards intentionally renders every OAuth
  // cadence as its own bar (richer than Grid's 5h-headline + 7d-string) - a
  // deliberate density difference, not a parity bug.
  const anthWindows = $derived.by<CardWindow[]>(() => {
    const rl = snapshot.claude_statusline?.payload.rate_limits;
    if (rl?.five_hour) {
      const out: CardWindow[] = [];
      out.push({ label: '5-hour', used: rl.five_hour.used_percent, elapsed: paceElapsed('five_hour'),
        tone: quotaTone(rl.five_hour.used_percent), resetsAt: rl.five_hour.resets_at, title: PROV.anthropicQuotaStatusline.title });
      if (rl.seven_day)
        out.push({ label: '7-day', used: rl.seven_day.used_percent, elapsed: paceElapsed('seven_day'),
          tone: quotaTone(rl.seven_day.used_percent), resetsAt: rl.seven_day.resets_at, title: PROV.anthropicQuotaStatusline.title });
      return out;
    }
    const cad = snapshot.claude_oauth?.cadences ?? [];
    return cad.map((c) => ({
      label: c.display_label,
      used: c.utilization_percent,
      elapsed: paceElapsed(c.key),
      tone: quotaTone(c.utilization_percent),
      resetsAt: c.resets_at,
      stale: anthStale,
      title: PROV.anthropicQuotaOauth.title,
    }));
  });

  // Cold-start / error / not-configured states for the Anthropic quota area,
  // mirroring GridView's anthState branches (same selector, same copy). The
  // overage billed row still renders underneath regardless of quota state.
  // `hasQuota` is `anthWindows.length > 0` rather than Grid's `!!anthropicQuota()`
  // so any OAuth cadence (not just five_hour) counts as data - consistent with
  // Cards showing every cadence.
  const anthErr = $derived(snapshot.claude_oauth_error ?? snapshot.claude_statusline_error ?? null);
  const anthQuotaState = $derived.by<CardQuotaState>(() => {
    const s = anthropicQuotaState({ hasQuota: anthWindows.length > 0, error: anthErr, unavailable: snapshot.claude_oauth_unavailable });
    switch (s.kind) {
      case 'error':
        return { kind: 'error', note: 'quota fetch failed', title: `Anthropic quota unavailable - ${s.message}` };
      case 'notConfigured':
        return { kind: 'notConfigured', heading: s.message, hint: 'Balanze reads your local Claude usage',
          title: 'Claude Code not detected. Balanze reads your local Claude usage at ~/.claude; install Claude Code (or restart Balanze after installing) to track quota.' };
      case 'loading':
        return { kind: 'loading', heading: 'Connecting to Claude...', sub: 'first check can take a minute',
          title: 'Balanze is fetching your Claude usage for the first time. Wire Balanze as your Claude statusLine in Settings for instant live quota.' };
      default:
        return { kind: 'data' };
    }
  });

  const eu = $derived(snapshot.claude_oauth?.extra_usage ?? null);
  const codex = $derived(snapshot.codex_quota);
  const openai = $derived(snapshot.openai);
  const anthPlan = $derived(snapshot.claude_oauth?.subscription_type ?? 'Claude');
  // Match GridView's column-visibility rule: show the OpenAI card when there is
  // actual data (Codex quota or OpenAI spend) OR billing is explicitly opted in
  // (`openaiEnabled` = the `openai_enabled` setting). Codex on by default does not
  // force the card for an Anthropic-only user; dismiss disables both providers so
  // the data clears and the card collapses.
  const hasOpenAI = $derived(openaiEnabled || !!codex || !!openai);
</script>

<div class="cards">
  <ProviderCard name="Anthropic · Claude" plan={anthPlan}
    windows={anthWindows} quotaState={anthQuotaState}
    billed={eu?.is_enabled
      ? { amount: `${microUsdToDollars(eu.used_credits_micro_usd)}/${microUsdToDollars(eu.monthly_limit_micro_usd)}`, note: 'overage · this cycle', badge: 'real', title: PROV.anthropicBilledOverage.title }
      : { amount: null, placeholder: 'none', note: 'overage · this cycle', title: PROV.anthropicBilledNa.title }} />
  {#if hasOpenAI}
    <ProviderCard name="OpenAI" plan="API + Codex"
      windows={codex
        ? [{ label: `Codex · ${codex.plan_type}`, used: codex.primary.used_percent,
            elapsed: codexElapsedFraction(codex.primary, snapshot.fetched_at) * 100, tone: quotaTone(codex.primary.used_percent),
            resetsAt: codex.primary.resets_at, stale: codexWindowExpired(codex.primary, snapshot.fetched_at) || !!degraded['codex_quota'],
            title: PROV.codexQuota.title }]
        : []}
      billed={openai
        ? { amount: microUsdToDollars(openai.total_micro_usd), note: 'admin api · this cycle', badge: 'real', title: PROV.openaiBilled.title }
        : { amount: null, placeholder: 'unavailable', note: snapshot.openai_error ? 'fetch failed' : 'not configured',
            title: snapshot.openai_error ? `OpenAI spend unavailable - ${snapshot.openai_error}` : 'OpenAI spend unavailable' }} />
  {/if}
</div>

<style>
  .cards { padding: 2px 16px 0; display: flex; flex-direction: column; gap: 10px; }
</style>
