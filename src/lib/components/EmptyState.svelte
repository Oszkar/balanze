<script lang="ts">
  // A centered status panel matching the popover aesthetic. Reusable for the
  // "backend not ready" state and other empty states. Inert by default: actions
  // only do something when an onClick is wired by the caller.
  type EmptyAction = { label: string; kind?: 'primary' | 'secondary'; onClick?: () => void };

  let { title, body = '', detail = null, actions = [] }: {
    title: string;
    body?: string;
    detail?: string | null;
    actions?: EmptyAction[];
  } = $props();
</script>

<div class="empty">
  <p class="title">{title}</p>
  {#if body}
    <p class="body">{body}</p>
  {/if}
  {#if detail}
    <p class="detail">{detail}</p>
  {/if}
  {#if actions.length > 0}
    <div class="actions">
      {#each actions as action (action.label)}
        <button
          type="button"
          class:secondary={action.kind === 'secondary'}
          onclick={() => action.onClick?.()}
        >
          {action.label}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .empty { display: flex; flex-direction: column; align-items: center; text-align: center;
    gap: 10px; padding: 22px 20px; min-height: 180px; justify-content: center; }
  .title { margin: 0; font-size: var(--text-base); font-weight: 600; color: var(--ink); }
  .body { margin: 0; font-size: var(--text-sm); color: var(--ink2); max-width: 264px; line-height: 1.45; }
  .detail { margin: 0; font-size: var(--text-2xs); color: var(--faint); font-family: ui-monospace, monospace;
    word-break: break-word; max-width: 280px; }
  .actions { display: flex; flex-direction: row; gap: 8px; margin-top: 4px; }
  button { font-size: var(--text-sm); font-weight: 600; padding: 5px 12px; border-radius: 8px; cursor: pointer;
    background: var(--seg-on); color: var(--seg-on-text); border: 1px solid var(--seg-border); }
  button.secondary { background: transparent; color: var(--ink); border: 1px solid var(--seg-border); }
  button:hover { opacity: 0.88; }
  button:focus-visible { outline: 2px solid var(--ink2); outline-offset: 2px; }
</style>
