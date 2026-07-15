<script lang="ts">
  // Top-level "something is stale/errored" banner. The per-cell stale marks
  // live in GridView; this surfaces the same degraded map (source -> error)
  // as one visible warning so a failure isn't silently blanked (v0.3.1 item 4).
  let { degraded }: { degraded: Record<string, string> } = $props();

  const LABELS: Record<string, string> = {
    claude_oauth: 'Anthropic quota',
    claude_jsonl: 'Claude activity',
    anthropic_api_cost: 'Anthropic cost estimate',
    codex_quota: 'Codex quota',
    openai_costs: 'OpenAI cost',
    claude_statusline: 'Claude statusLine',
    frontend_events: 'Popover event channel',
  };

  const entries = $derived(Object.entries(degraded));
</script>

{#if entries.length}
  <div class="banner" role="status">
    <span class="dot" aria-hidden="true">⚠</span>
    <div class="msgs">
      {#each entries as [src, msg] (src)}
        <div class="row"><b>{LABELS[src] ?? src}:</b> {msg}</div>
      {/each}
    </div>
  </div>
{/if}

<style>
  .banner { display: flex; gap: 8px; align-items: flex-start; margin: 0 16px 10px;
            padding: 8px 10px; border-radius: 8px; background: var(--warn-bg);
            border: 1px solid var(--warn-hair); }
  .dot { color: var(--warn, #c70); font-size: 12px; line-height: 1.3; flex-shrink: 0; }
  .msgs { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .row { font-size: var(--text-xs); color: var(--ink, inherit); line-height: 1.3; word-break: break-word; }
  .row b { font-weight: 600; }
</style>
