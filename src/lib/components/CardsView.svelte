<script lang="ts">
  import type { Snapshot } from '$lib/types/snapshot';
  import { anthropicQuota, anthropicSourceView, quotaTone, codexElapsedFraction, codexQuota, codexWindowLabel, codexWindowsByKind, matchingAnthropicPace, openAiCostCell, overageCell } from '$lib/presentation/quota';
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

  // Single source of truth for "which source is active": anthropicQuota()
  // already computes this (headline/tone/source) for GridView, so Cards
  // derives its own source-selection from the same call instead of
  // re-implementing the "has a five_hour window" gate independently. Three
  // independent copies of that gate (this file had two, quota.ts had a
  // third) is exactly what let a prior change desync CardsView from
  // GridView - reusing one computed value removes that risk class rather
  // than relying on the copies happening to agree. `anthStale` mirrors
  // GridView: the statusline went degraded and we are on the OAuth
  // fallback, so Cards shows the same stale cue (per-window "stale"
  // instead of the reset countdown).
  const anthQuota = $derived(anthropicQuota(snapshot));
  const anthView = $derived(anthropicSourceView(snapshot));
  const anthStale = $derived(anthView?.stale ?? false);
  const anthPace = $derived(matchingAnthropicPace(snapshot));

  const paceElapsed = (key: string): number | null => {
    const p = anthPace.find((x) => x.key === key);
    return p ? p.elapsed_fraction * 100 : null;
  };
  // Each window carries its pace tick (looked up by key) and the matching
  // provenance tooltip for its source. Cards intentionally renders every OAuth
  // cadence as its own bar (richer than Grid's 5h-headline + 7d-string) - a
  // deliberate density difference, not a parity bug.
  const anthWindows = $derived.by<CardWindow[]>(() => {
    if (!anthView) return [];
    return anthView.windows.map((w) => ({
      label: w.label,
      used: w.pct,
      elapsed: paceElapsed(w.key),
      tone: quotaTone(w.pct),
      resetsAt: w.resetsAt,
      stale: anthStale,
      title: anthView.source === 'statusline' ? PROV.anthropicQuotaStatusline.title : PROV.anthropicQuotaOauth.title,
    }));
  });

  // Cold-start / error / not-configured states for the Anthropic quota area,
  // mirroring GridView's anthState branches (same selector, same copy via
  // ANTH_QUOTA_COPY). The overage billed row still renders underneath regardless
  // of quota state. `hasQuota` reuses the same `anthQuota` computed above, so
  // both views agree on data-vs-loading when the selected source has any quota
  // window; the all-cadence bar rendering (anthWindows) is unaffected.
  const anthErr = $derived(snapshot.claude_oauth_error ?? snapshot.claude_statusline_error ?? null);
  const anthQuotaState = $derived.by<CardQuotaState>(() => {
    const s = anthropicQuotaState({ hasQuota: !!anthQuota, error: anthErr, unavailable: snapshot.claude_oauth_unavailable });
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
  const anthBilled = $derived(overageCell(eu));
  const codex = $derived(snapshot.codex_quota);
  const codexView = $derived(codexQuota(snapshot));
  const openai = $derived(snapshot.openai);
  const openaiCost = $derived(openAiCostCell(snapshot));
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
  const codexWindows = $derived.by<CardWindow[]>(() => {
    if (!codex) return [];
    const { five, weekly } = codexWindowsByKind(codex);
    const out: CardWindow[] = [];
    for (const win of [five, weekly]) {
      if (!win) continue;
      out.push({
        label: `Codex ${codexWindowLabel(win)} · ${codex.plan_type}`,
        used: win.used_percent,
        elapsed: codexElapsedFraction(win, snapshot.fetched_at) * 100,
        tone: quotaTone(win.used_percent),
        resetsAt: win.resets_at,
        // Staleness belongs to the rollout, not an individual bar. Once any
        // window has reset, every figure in that old rollout is an undercount.
        stale: !!codexView?.expired || !!degraded['codex_quota'],
        title: PROV.codexQuota.title,
      });
    }
    return out;
  });
</script>

<div class="cards">
  <ProviderCard name="Anthropic · Claude" plan={anthPlan}
    windows={anthWindows} quotaState={anthQuotaState}
    billed={anthBilled} />
  {#if showOpenAI}
    <ProviderCard name="OpenAI" plan="API + Codex"
      windows={codexWindows} quotaState={openaiQuotaState}
      dismiss={{ aria: OPENAI_COL_COPY.dismiss.aria, title: OPENAI_COL_COPY.dismiss.title, onClick: () => onDismissOpenai?.() }}
      onConnect={onSettings}
      billed={colState.kind === 'data'
        ? (openaiCost
            ? { ...openaiCost, badge: 'real' }
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
