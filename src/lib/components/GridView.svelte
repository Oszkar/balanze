<script lang="ts">
  import type { Snapshot } from '$lib/types/snapshot';
  import { anthropicQuota, codexElapsedFraction, codexWindowExpired, codexQuota, overageCell } from '$lib/presentation/quota';
  import { microUsdToDollars } from '$lib/presentation/format';
  import { PROV } from '$lib/presentation/provenance';
  import { ANTH_QUOTA_COPY, OPENAI_COL_COPY } from '$lib/presentation/quotaCopy';
  import { anthropicQuotaState, openaiColumnState } from '$lib/presentation/cellState';
  import QuotaCell from './QuotaCell.svelte';
  import BilledCell from './BilledCell.svelte';

  // `openaiEnabled` is the OpenAI *billing* opt-in (the `openai_enabled` setting),
  // not a column-visibility flag - see openaiColumnState. Default false (the
  // settings default); the column still shows whenever the snapshot carries data.
  let { snapshot, degraded, openaiEnabled = false, onDismissOpenai, onSettings }:
    { snapshot: Snapshot; degraded: Record<string, string>; openaiEnabled?: boolean;
      onDismissOpenai?: () => void; onSettings?: () => void } = $props();

  const aq = $derived(anthropicQuota(snapshot));
  const fivePace = $derived(snapshot.pace.find((p) => p.key === 'five_hour') ?? null);
  const codex = $derived(snapshot.codex_quota);
  const cq = $derived(codexQuota(snapshot));
  const eu = $derived(snapshot.claude_oauth?.extra_usage ?? null);
  const overage = $derived(overageCell(eu));
  const openai = $derived(snapshot.openai);
  const anthStale = $derived(!!degraded['claude_statusline'] && aq?.source === 'oauth');
  // When no quota is available, distinguish a real failure (an error slot is
  // set) from a cold-start window where the first OAuth poll is still in flight
  // (no data, no error yet - OAuth backs off on the 429s that happen during
  // active Claude Code use, so this can take a moment when statusline isn't wired).
  const anthErr = $derived(snapshot.claude_oauth_error ?? snapshot.claude_statusline_error ?? null);
  const openaiErr = $derived(snapshot.openai_error ?? null);

  // Pure state selection (unit-tested in cellState.test.ts). The quota state
  // drives the Anthropic cell; the column state drives whether the OpenAI column
  // shows data, a connect CTA, an error, or collapses to single-provider.
  const anthState = $derived(anthropicQuotaState({ hasQuota: !!aq, error: anthErr, unavailable: snapshot.claude_oauth_unavailable }));
  const colState = $derived(
    openaiColumnState({ billingEnabled: openaiEnabled, hasData: !!codex || !!openai, error: openaiErr }),
  );
  const showOpenAI = $derived(colState.kind !== 'hidden');
</script>

<div class="grid" class:single={!showOpenAI}>
  <div class="colhead"><span class="p">Anthropic</span></div>
  {#if showOpenAI}
    <div class="colhead">
      <span class="p">OpenAI</span>
      {#if colState.kind === 'data' || colState.kind === 'connect' || colState.kind === 'error'}
        <button class="dismiss" type="button" aria-label={OPENAI_COL_COPY.dismiss.aria} title={OPENAI_COL_COPY.dismiss.title} onclick={() => onDismissOpenai?.()}>×</button>
      {/if}
    </div>
  {/if}

  {#if anthState.kind === 'data' && aq}
    <QuotaCell pct={aq.headline.pct} used={(fivePace?.used_fraction ?? aq.headline.pct / 100) * 100}
      elapsed={fivePace ? fivePace.elapsed_fraction * 100 : null} tone={aq.tone}
      resetsAt={aq.headline.resetsAt} secondary={aq.secondary ? `7d ${aq.secondary.pct.toFixed(0)}%` : ''}
      stale={anthStale}
      title={aq.source === 'statusline' ? PROV.anthropicQuotaStatusline.title : PROV.anthropicQuotaOauth.title} />
  {:else if anthState.kind === 'error'}
    <BilledCell hatch placeholder="unavailable" note={ANTH_QUOTA_COPY.error.note}
      title={ANTH_QUOTA_COPY.error.title(anthState.message)} />
  {:else if anthState.kind === 'notConfigured'}
    <div class="cell notconf" title={ANTH_QUOTA_COPY.notConfigured.title}>
      <span class="notconf-title">{anthState.message}</span>
      <span class="notconf-hint">{ANTH_QUOTA_COPY.notConfigured.hint}</span>
    </div>
  {:else}
    <div class="cell skel" title={ANTH_QUOTA_COPY.loading.title}>
      <div class="skelbar"></div>
      <div class="skeltext">
        <span class="skelcap">{ANTH_QUOTA_COPY.loading.heading}</span>
        <span class="skelsub">{ANTH_QUOTA_COPY.loading.sub}</span>
      </div>
    </div>
  {/if}

  {#if showOpenAI}
    {#if colState.kind === 'connect'}
      <div class="cell connect">
        <span class="connect-label">{OPENAI_COL_COPY.connect.label}</span>
        <button class="connect-btn" type="button" aria-label={OPENAI_COL_COPY.connect.aria} onclick={() => onSettings?.()}>{OPENAI_COL_COPY.connect.cta}</button>
        <span class="connect-hint">{OPENAI_COL_COPY.connect.hint}</span>
      </div>
    {:else if colState.kind === 'error'}
      <!-- Span both OpenAI metric rows (col 2) so it stays symmetric with the
           Anthropic quota + billed cells (mirrors the connect-state placement). -->
      <div class="span2">
        <BilledCell hatch placeholder="unavailable" note={OPENAI_COL_COPY.error.note}
          title={OPENAI_COL_COPY.error.title(colState.message)} />
      </div>
    {:else if cq}
      <QuotaCell pct={cq.headline.pct} used={cq.headline.pct}
        elapsed={codexElapsedFraction(cq.headline.window, snapshot.fetched_at) * 100} tone={cq.tone}
        resetsAt={cq.headline.resetsAt}
        secondary={cq.secondary ? `${cq.secondary.label} ${cq.secondary.pct.toFixed(0)}% · ${cq.plan}` : cq.plan}
        stale={!!degraded['codex_quota'] || codexWindowExpired(cq.headline.window, snapshot.fetched_at)} staleLabel="stale" title={PROV.codexQuota.title} />
    {:else}
      <BilledCell note="not connected" title="OpenAI Codex not configured" />
    {/if}
  {/if}

  <BilledCell amount={overage.amount} placeholder={overage.placeholder ?? 'none'} hatch={overage.amount === null}
    note={overage.note} badge={overage.badge ?? null} title={overage.title} />
  {#if showOpenAI && colState.kind !== 'connect' && colState.kind !== 'error'}
    {#if openai}
      <BilledCell amount={microUsdToDollars(openai.total_micro_usd)}
        note="admin api · this cycle" badge={PROV.openaiBilled.badge} title={PROV.openaiBilled.title} />
    {:else}
      <BilledCell hatch note={openaiErr ? 'fetch failed' : 'not configured'}
        title={openaiErr ? `OpenAI spend unavailable - ${openaiErr}` : 'OpenAI spend unavailable'} />
    {/if}
  {/if}
</div>

{#if !showOpenAI}
  <div class="add-openai-row">
    <button class="add-openai" type="button" onclick={() => onSettings?.()}>{OPENAI_COL_COPY.add}</button>
  </div>
{/if}

<style>
  .grid { padding: 2px 16px 4px; display: grid; grid-template-columns: 1fr 1fr; gap: 10px; align-items: stretch; }
  .grid.single { grid-template-columns: 1fr; }
  .colhead { position: relative; display: flex; flex-direction: column; align-items: center; padding-bottom: 3px; }
  .colhead .p { font-size: var(--text-base); font-weight: 600; }
  /* Offsets compensate for the larger padding so the × stays in the corner while
     the hit target grows to ~22px (glyph weight unchanged). */
  .dismiss { position: absolute; top: -3px; right: -3px; min-width: var(--control-target-min); min-height: var(--control-target-min); background: none; border: none; color: var(--faint);
    cursor: pointer; font-size: var(--text-base); line-height: 1; padding: 5px 6px; border-radius: 4px; }
  .dismiss:hover { color: var(--ink); }
  .dismiss:focus-visible { outline: 2px solid var(--ink2); outline-offset: 1px; border-radius: 4px; }
  .cell { border-radius: 12px; background: var(--tile-face); box-shadow: var(--tile-elev); }
  /* Cold-start skeleton: a muted pulsing bar so the first-fetch window reads as
     "loading", not a bare string. */
  .skel { padding: 11px 12px; display: flex; flex-direction: column; gap: 8px; justify-content: center; min-height: 84px; cursor: help; }
  .skelbar { height: 14px; border-radius: 7px; background: var(--track); box-shadow: var(--channel); animation: pulse 1.4s ease-in-out infinite; }
  .skeltext { display: flex; flex-direction: column; gap: 2px; align-items: center; }
  .skelcap { font-size: var(--text-sm); color: var(--faint); }
  .skelsub { font-size: var(--text-2xs); color: var(--faint); opacity: .8; }
  @keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: .4; } }
  @media (prefers-reduced-motion: reduce) { .skelbar { animation: none; } }
  /* Connect CTA: spans the OpenAI column's two metric rows (column 2). */
  /* Connect CTA: a dashed empty slot (clearly fillable), distinct from the
     filled machined tiles. Spans the OpenAI column's two metric rows (column 2). */
  .connect { grid-column: 2; grid-row: span 2; padding: var(--sp-3) var(--sp-2); display: flex; flex-direction: column;
    align-items: center; justify-content: center; gap: 6px; background: transparent; box-shadow: none;
    border: 1.4px dashed var(--seg-border); }
  .connect-label { font-size: var(--text-sm); color: var(--faint); }
  .connect-btn { font-size: var(--text-sm); font-weight: 600; padding: 5px 12px; border-radius: 8px;
    border: 1px solid var(--seg-border); background: var(--seg-on); color: var(--seg-on-text); cursor: pointer; }
  .connect-btn:hover { opacity: .88; }
  .connect-btn:focus-visible { outline: 2px solid var(--ink2); outline-offset: 2px; }
  .connect-hint { font-size: var(--text-2xs); color: var(--faint); }
  /* Error cell: span the OpenAI column's two metric rows, same as connect.
     The wrapper carries the grid placement; :global stretches the BilledCell
     it wraps to fill the full two-row height. */
  .span2 { grid-column: 2; grid-row: span 2; display: flex; }
  .span2 :global(.cell) { flex: 1; }
  .notconf { padding: 11px 12px; display: flex; flex-direction: column; gap: 4px; justify-content: center; min-height: 84px; cursor: help; }
  .notconf-title { font-size: var(--text-sm); font-weight: 600; color: var(--ink); }
  .notconf-hint { font-size: var(--text-2xs); color: var(--faint); }
  .add-openai-row { display: flex; justify-content: center; padding: 2px 16px 4px; }
  .add-openai { font-size: var(--text-2xs); font-weight: 600; color: var(--faint); background: none; border: none;
    cursor: pointer; padding: 4px 8px; border-radius: 6px; }
  .add-openai:hover { color: var(--ink); }
  .add-openai:focus-visible { outline: 2px solid var(--ink2); outline-offset: 2px; }
</style>
