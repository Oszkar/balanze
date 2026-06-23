<script lang="ts">
  import type { Snapshot } from '$lib/types/snapshot';
  import { quotaTone, codexElapsedFraction, codexWindowExpired } from '$lib/presentation/quota';
  import { microUsdToDollars } from '$lib/presentation/format';
  import { PROV } from '$lib/presentation/provenance';
  import ProviderCard, { type CardWindow } from './ProviderCard.svelte';

  // `openaiEnabled` = the OpenAI billing opt-in (`openai_enabled`); default false.
  let { snapshot, openaiEnabled = false, degraded = {} }:
    { snapshot: Snapshot; openaiEnabled?: boolean; degraded?: Record<string, string> } = $props();

  // Anthropic windows: same source order GridView uses via anthropicQuota() -
  // the live statusline (5h/7d) is preferred, the OAuth cadences are the
  // fallback - so the two views never disagree on which source they render.
  // Each window carries its pace tick (looked up by key) and the matching
  // provenance tooltip for its source.
  const paceElapsed = (key: string): number | null => {
    const p = snapshot.pace.find((x) => x.key === key);
    return p ? p.elapsed_fraction * 100 : null;
  };
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
      title: PROV.anthropicQuotaOauth.title,
    }));
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
    windows={anthWindows}
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
