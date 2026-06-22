<script lang="ts">
  import type { Snapshot } from '$lib/types/snapshot';
  import { anthropicQuota, quotaTone, codexElapsedFraction, codexWindowExpired } from '$lib/presentation/quota';
  import { microUsdToDollars } from '$lib/presentation/format';
  import { PROV } from '$lib/presentation/provenance';
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
  const eu = $derived(snapshot.claude_oauth?.extra_usage ?? null);
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
  const anthState = $derived(anthropicQuotaState({ hasQuota: !!aq, error: anthErr }));
  const colState = $derived(
    openaiColumnState({ billingEnabled: openaiEnabled, hasData: !!codex || !!openai, error: openaiErr }),
  );
  const showOpenAI = $derived(colState.kind !== 'hidden');
</script>

<div class="grid" class:single={!showOpenAI}>
  <div class="colhead"><span class="p">Anthropic</span><span class="plan">Claude · {snapshot.claude_oauth?.subscription_type ?? '-'}</span></div>
  {#if showOpenAI}
    <div class="colhead">
      <span class="p">OpenAI</span><span class="plan">API + Codex</span>
      {#if colState.kind === 'data' || colState.kind === 'connect' || colState.kind === 'error'}
        <button class="dismiss" type="button" aria-label="Hide OpenAI column" title="Hide OpenAI - re-add in Settings" onclick={() => onDismissOpenai?.()}>×</button>
      {/if}
    </div>
  {/if}

  {#if anthState.kind === 'data' && aq}
    <QuotaCell pct={aq.headline.pct} used={(fivePace?.used_fraction ?? aq.headline.pct / 100) * 100}
      elapsed={fivePace ? fivePace.elapsed_fraction * 100 : null} tone={aq.tone}
      resetsAt={aq.headline.resetsAt} secondary={aq.secondary ? `7-day ${aq.secondary.pct.toFixed(0)}%` : ''}
      stale={anthStale} badge="quota"
      title={aq.source === 'statusline' ? PROV.anthropicQuotaStatusline.title : PROV.anthropicQuotaOauth.title} />
  {:else if anthState.kind === 'error'}
    <BilledCell hatch placeholder="unavailable" note="quota fetch failed"
      title={`Anthropic quota unavailable - ${anthState.message}`} />
  {:else}
    <div class="cell skel"
      title="Waiting for the first quota fetch - the OAuth usage endpoint backs off on the 429s it returns during active Claude Code use. Wire Balanze as your Claude statusLine for instant live quota.">
      <div class="skelbar"></div>
      <span class="skelcap">fetching quota...</span>
    </div>
  {/if}

  {#if showOpenAI}
    {#if colState.kind === 'connect'}
      <div class="cell connect">
        <span class="connect-label">not connected</span>
        <button class="connect-btn" type="button" onclick={() => onSettings?.()}>Connect -&gt;</button>
        <span class="connect-hint">paste admin key</span>
      </div>
    {:else if colState.kind === 'error'}
      <!-- Span both OpenAI metric rows (col 2) so it stays symmetric with the
           Anthropic quota + billed cells (mirrors the connect-state placement). -->
      <div class="span2">
        <BilledCell hatch placeholder="unavailable" note="fetch failed"
          title={`OpenAI unavailable - ${colState.message}`} />
      </div>
    {:else if codex}
      <QuotaCell pct={codex.primary.used_percent} used={codex.primary.used_percent}
        elapsed={codexElapsedFraction(codex.primary, snapshot.fetched_at) * 100} tone={quotaTone(codex.primary.used_percent)}
        resetsAt={codex.primary.resets_at} secondary={`codex ${codex.plan_type}`}
        stale={!!degraded['codex_quota'] || codexWindowExpired(codex.primary, snapshot.fetched_at)} staleLabel="stale" badge="quota" title={PROV.codexQuota.title} />
    {:else}
      <BilledCell note="not connected" title="OpenAI Codex not configured" />
    {/if}
  {/if}

  {#if eu?.is_enabled}
    <BilledCell amount={`${microUsdToDollars(eu.used_credits_micro_usd)}/${microUsdToDollars(eu.monthly_limit_micro_usd)}`}
      note="overage · this cycle" badge={PROV.anthropicBilledOverage.badge} title={PROV.anthropicBilledOverage.title} />
  {:else}
    <BilledCell hatch note="no per-user API spend" badge={PROV.anthropicBilledNa.badge} title={PROV.anthropicBilledNa.title} />
  {/if}
  {#if showOpenAI && colState.kind !== 'connect' && colState.kind !== 'error'}
    {#if openai}
      <BilledCell amount={microUsdToDollars(openai.total_micro_usd)}
        note="admin api · this cycle" badge={PROV.openaiBilled.badge} title={PROV.openaiBilled.title} />
    {:else}
      <BilledCell hatch note={openaiErr ? 'fetch failed' : 'not configured'} badge="na"
        title={openaiErr ? `OpenAI spend unavailable - ${openaiErr}` : 'OpenAI spend unavailable'} />
    {/if}
  {/if}
</div>

<style>
  .grid { padding: 2px 16px 4px; display: grid; grid-template-columns: 1fr 1fr; gap: 10px; align-items: stretch; }
  .grid.single { grid-template-columns: 1fr; }
  .colhead { position: relative; display: flex; flex-direction: column; align-items: center; padding-bottom: 3px; }
  .colhead .p { font-size: var(--text-base); font-weight: 600; }
  .colhead .plan { font-size: var(--text-2xs); color: var(--faint); }
  .dismiss { position: absolute; top: 0; right: 1px; background: none; border: none; color: var(--faint);
    cursor: pointer; font-size: var(--text-base); line-height: 1; padding: 0 2px; }
  .dismiss:hover { color: var(--ink); }
  .dismiss:focus-visible { outline: 2px solid var(--ink2); outline-offset: 1px; border-radius: 4px; }
  .cell { border-radius: 12px; background: var(--tile-face); box-shadow: var(--tile-elev); }
  /* Cold-start skeleton: a muted pulsing bar so the first-fetch window reads as
     "loading", not a bare string. */
  .skel { padding: 11px 12px; display: flex; flex-direction: column; gap: 8px; justify-content: center; min-height: 84px; cursor: help; }
  .skelbar { height: 14px; border-radius: 7px; background: var(--track); box-shadow: var(--channel); animation: pulse 1.4s ease-in-out infinite; }
  .skelcap { font-size: var(--text-2xs); color: var(--faint); }
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
</style>
