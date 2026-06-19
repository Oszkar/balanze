<script lang="ts">
  import type { Snapshot } from '$lib/types/snapshot';
  import { anthropicQuota, quotaTone, codexElapsedFraction, codexWindowExpired } from '$lib/presentation/quota';
  import { microUsdToDollars } from '$lib/presentation/format';
  import { PROV } from '$lib/presentation/provenance';
  import QuotaCell from './QuotaCell.svelte';
  import BilledCell from './BilledCell.svelte';

  let { snapshot, degraded }: { snapshot: Snapshot; degraded: Record<string, string> } = $props();

  const aq = $derived(anthropicQuota(snapshot));
  const fivePace = $derived(snapshot.pace.find((p) => p.key === 'five_hour') ?? null);
  const codex = $derived(snapshot.codex_quota);
  const eu = $derived(snapshot.claude_oauth?.extra_usage ?? null);
  const openai = $derived(snapshot.openai);
  const hasOpenAI = $derived(!!codex || !!openai || !!snapshot.openai_error);
  const anthStale = $derived(!!degraded['claude_statusline'] && aq?.source === 'oauth');
  // When no quota is available, distinguish a real failure (an error slot is
  // set) from a cold-start window where the first OAuth poll is still in flight
  // (no data, no error yet — OAuth backs off on the 429s that happen during
  // active Claude Code use, so this can take a moment when statusline isn't wired).
  const anthErr = $derived(snapshot.claude_oauth_error ?? snapshot.claude_statusline_error ?? null);
</script>

<div class="grid" class:single={!hasOpenAI}>
  <div class="colhead"><span class="p">Anthropic</span><span class="plan">Claude · {snapshot.claude_oauth?.subscription_type ?? '—'}</span></div>
  {#if hasOpenAI}<div class="colhead"><span class="p">OpenAI</span><span class="plan">API + Codex</span></div>{/if}

  {#if aq}
    <QuotaCell pct={aq.headline.pct} used={(fivePace?.used_fraction ?? aq.headline.pct / 100) * 100}
      elapsed={fivePace ? fivePace.elapsed_fraction * 100 : null} tone={aq.tone}
      resetsAt={aq.headline.resetsAt} secondary={aq.secondary ? `7-day ${aq.secondary.pct.toFixed(0)}%` : ''}
      stale={anthStale}
      title={aq.source === 'statusline' ? PROV.anthropicQuotaStatusline.title : PROV.anthropicQuotaOauth.title} />
  {:else if anthErr}
    <BilledCell hatch placeholder="unavailable" note="quota fetch failed"
      title={`Anthropic quota unavailable - ${anthErr}`} />
  {:else}
    <BilledCell placeholder="connecting..." note="fetching quota..."
      title="Waiting for the first quota fetch - the OAuth usage endpoint backs off on the 429s it returns during active Claude Code use. Wire Balanze as your Claude statusLine for instant live quota." />
  {/if}
  {#if hasOpenAI}
    {#if codex}
      <QuotaCell pct={codex.primary.used_percent} used={codex.primary.used_percent}
        elapsed={codexElapsedFraction(codex.primary) * 100} tone={quotaTone(codex.primary.used_percent)}
        resetsAt={codex.primary.resets_at} secondary={`codex ${codex.plan_type}`}
        stale={!!degraded['codex_quota'] || codexWindowExpired(codex.primary)} title={PROV.codexQuota.title} />
    {:else}
      <BilledCell note="not connected" title="OpenAI Codex not configured" />
    {/if}
  {/if}

  {#if eu?.is_enabled}
    <BilledCell amount={`${microUsdToDollars(eu.used_credits_micro_usd)}/${microUsdToDollars(eu.monthly_limit_micro_usd)}`}
      note="overage · this cycle" title={PROV.anthropicBilledOverage.title} />
  {:else}
    <BilledCell hatch note="no per-user API spend" title={PROV.anthropicBilledNa.title} />
  {/if}
  {#if hasOpenAI}
    {#if openai}
      <BilledCell amount={microUsdToDollars(Math.round(openai.total_usd * 1_000_000))}
        note="admin api · this cycle" title={PROV.openaiBilled.title} />
    {:else}
      <BilledCell hatch note={snapshot.openai_error ? '✗ fetch failed' : 'not configured'} title="OpenAI spend unavailable" />
    {/if}
  {/if}
</div>

<style>
  .grid { padding: 2px 16px 0; display: grid; grid-template-columns: 1fr 1fr; gap: 8px; align-items: stretch; }
  .grid.single { grid-template-columns: 1fr; }
  .colhead { display: flex; flex-direction: column; align-items: center; padding-bottom: 3px; }
  .colhead .p { font-size: 13.5px; font-weight: 600; }
  .colhead .plan { font-size: 9.5px; color: var(--faint); }
</style>
