<script lang="ts">
  import type { Snapshot } from '$lib/types/snapshot';
  import { anthropicQuota, quotaTone, codexElapsedFraction, codexWindowExpired } from '$lib/presentation/quota';
  import { microUsdToDollars } from '$lib/presentation/format';
  import { PROV } from '$lib/presentation/provenance';
  import { anthropicQuotaState, openaiColumnState } from '$lib/presentation/cellState';
  import { ANTH_QUOTA_COPY, OPENAI_COL_COPY } from '$lib/presentation/quotaCopy';
  import ProviderCard, { type CardWindow, type CardQuotaState } from './ProviderCard.svelte';

  // `openaiEnabled` = the OpenAI billing opt-in (`openai_enabled`); default false.
  // `onDismissOpenai` collapses the OpenAI column (disables both providers);
  // `onSettings` opens Settings (the connect CTA and the "+ Add OpenAI" affordance).
  let { snapshot, openaiEnabled = false, degraded = {}, onDismissOpenai, onSettings }:
    { snapshot: Snapshot; openaiEnabled?: boolean; degraded?: Record<string, string>;
      onDismissOpenai?: () => void; onSettings?: () => void } = $props();

  // Source order matches GridView's anthropicQuota(): the live statusline (5h/7d)
  // is preferred, the OAuth cadences are the fallback - so the two views never
  // disagree on which source they render. `anthStale` mirrors GridView: the
  // statusline went degraded and we are on the OAuth fallback, so Cards shows the
  // same stale cue (per-window "stale" instead of the reset countdown).
  const anthSource = $derived(
    snapshot.claude_statusline?.payload.rate_limits?.windows.some((w) => w.key === 'five_hour')
      ? 'statusline'
      : 'oauth',
  );
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
    if (rl?.windows.some((w) => w.key === 'five_hour')) {
      return rl.windows.map((w) => ({
        label: w.label,
        used: w.used_percent,
        elapsed: paceElapsed(w.key),
        tone: quotaTone(w.used_percent),
        resetsAt: w.resets_at,
        title: PROV.anthropicQuotaStatusline.title,
      }));
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
  // mirroring GridView's anthState branches (same selector, same copy via
  // ANTH_QUOTA_COPY). The overage billed row still renders underneath regardless
  // of quota state. `hasQuota` uses Grid's exact gate (`!!anthropicQuota()`,
  // which requires a five_hour cadence) so the two views agree on data-vs-loading
  // even when only a seven_day / model-specific cadence is present; the
  // all-cadence bar rendering (anthWindows) is unaffected.
  const anthErr = $derived(snapshot.claude_oauth_error ?? snapshot.claude_statusline_error ?? null);
  const anthQuotaState = $derived.by<CardQuotaState>(() => {
    const s = anthropicQuotaState({ hasQuota: !!anthropicQuota(snapshot), error: anthErr, unavailable: snapshot.claude_oauth_unavailable });
    switch (s.kind) {
      case 'error':
        return { kind: 'error', note: ANTH_QUOTA_COPY.error.note, title: ANTH_QUOTA_COPY.error.title(s.message) };
      case 'notConfigured':
        return { kind: 'notConfigured', heading: s.message, hint: ANTH_QUOTA_COPY.notConfigured.hint, title: ANTH_QUOTA_COPY.notConfigured.title };
      case 'loading':
        return { kind: 'loading', heading: ANTH_QUOTA_COPY.loading.heading, sub: ANTH_QUOTA_COPY.loading.sub, title: ANTH_QUOTA_COPY.loading.title };
      default:
        return { kind: 'data' };
    }
  });

  const eu = $derived(snapshot.claude_oauth?.extra_usage ?? null);
  const codex = $derived(snapshot.codex_quota);
  const openai = $derived(snapshot.openai);
  const openaiErr = $derived(snapshot.openai_error ?? null);
  const anthPlan = $derived(snapshot.claude_oauth?.subscription_type ?? 'Claude');

  // OpenAI column state mirrors GridView (same openaiColumnState selector): the
  // card shows whenever the snapshot carries data, the connect CTA when billing
  // is opted in with nothing to show yet, an error block on a failed fetch, or
  // collapses to the single-provider "+ Add OpenAI" affordance. Dismiss disables
  // both OpenAI-side providers so the data clears and the card collapses.
  const colState = $derived(
    openaiColumnState({ billingEnabled: openaiEnabled, hasData: !!codex || !!openai, error: openaiErr }),
  );
  const showOpenAI = $derived(colState.kind !== 'hidden');
  const openaiQuotaState = $derived.by<CardQuotaState>(() => {
    if (colState.kind === 'connect')
      return { kind: 'connect', label: OPENAI_COL_COPY.connect.label, cta: OPENAI_COL_COPY.connect.cta, aria: OPENAI_COL_COPY.connect.aria, hint: OPENAI_COL_COPY.connect.hint };
    if (colState.kind === 'error')
      return { kind: 'error', note: OPENAI_COL_COPY.error.note, title: OPENAI_COL_COPY.error.title(colState.message) };
    return { kind: 'data' };
  });
  // Codex quota bar for the data state (empty when only OpenAI spend is present,
  // where the card shows just the header + billed spend).
  const codexWindows = $derived.by<CardWindow[]>(() =>
    codex
      ? [{ label: `Codex · ${codex.plan_type}`, used: codex.primary.used_percent,
          elapsed: codexElapsedFraction(codex.primary, snapshot.fetched_at) * 100, tone: quotaTone(codex.primary.used_percent),
          resetsAt: codex.primary.resets_at, stale: codexWindowExpired(codex.primary, snapshot.fetched_at) || !!degraded['codex_quota'],
          title: PROV.codexQuota.title }]
      : [],
  );
</script>

<div class="cards">
  <ProviderCard name="Anthropic · Claude" plan={anthPlan}
    windows={anthWindows} quotaState={anthQuotaState}
    billed={eu?.is_enabled
      ? { amount: `${microUsdToDollars(eu.used_credits_micro_usd)}/${microUsdToDollars(eu.monthly_limit_micro_usd)}`, note: 'overage · this cycle', badge: 'real', title: PROV.anthropicBilledOverage.title }
      : { amount: null, placeholder: 'none', note: 'overage · this cycle', title: PROV.anthropicBilledNa.title }} />
  {#if showOpenAI}
    <ProviderCard name="OpenAI" plan="API + Codex"
      windows={codexWindows} quotaState={openaiQuotaState}
      dismiss={{ aria: OPENAI_COL_COPY.dismiss.aria, title: OPENAI_COL_COPY.dismiss.title, onClick: () => onDismissOpenai?.() }}
      onConnect={onSettings}
      billed={colState.kind === 'data'
        ? (openai
            ? { amount: microUsdToDollars(openai.total_micro_usd), note: 'admin api · this cycle', badge: 'real', title: PROV.openaiBilled.title }
            : { amount: null, placeholder: 'unavailable', note: openaiErr ? 'fetch failed' : 'not configured',
                title: openaiErr ? `OpenAI spend unavailable - ${openaiErr}` : 'OpenAI spend unavailable' })
        : undefined} />
  {/if}
</div>

{#if !showOpenAI}
  <div class="add-openai-row">
    <button class="add-openai" type="button" onclick={() => onSettings?.()}>{OPENAI_COL_COPY.add}</button>
  </div>
{/if}

<style>
  .cards { padding: 2px 16px 0; display: flex; flex-direction: column; gap: 10px; }
  /* Re-add affordance shown when the OpenAI column is collapsed, mirroring GridView. */
  .add-openai-row { display: flex; justify-content: center; padding: 6px 16px 0; }
  .add-openai { font-size: var(--text-2xs); font-weight: 600; color: var(--faint); background: none; border: none;
    cursor: pointer; padding: 4px 8px; border-radius: 6px; }
  .add-openai:hover { color: var(--ink); }
  .add-openai:focus-visible { outline: 2px solid var(--ink2); outline-offset: 2px; }
</style>
