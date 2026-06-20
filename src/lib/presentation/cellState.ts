// Pure cell/column state selection for the popover matrix. No Svelte, no IPC -
// just data in, a discriminated state out, so the rules are unit-testable.

export type QuotaState =
  | { kind: 'data' }
  | { kind: 'loading' }
  | { kind: 'error'; message: string };

export type ColumnState =
  | { kind: 'data' }
  | { kind: 'connect' }
  | { kind: 'error'; message: string }
  | { kind: 'hidden' };

export function anthropicQuotaState(i: { hasQuota: boolean; error: string | null }): QuotaState {
  if (i.hasQuota) return { kind: 'data' };
  if (i.error) return { kind: 'error', message: i.error };
  return { kind: 'loading' };
}

// `billingEnabled` is the OpenAI *billing* opt-in (the `openai_enabled` provider
// toggle), NOT a column-visibility flag. The column shows whenever there is
// actual data (Codex quota or OpenAI spend) regardless of the toggle; the
// connect "paste admin key" CTA only appears when billing is explicitly enabled
// with nothing to show yet. So Codex scanning being on by default never forces a
// spurious key CTA for an Anthropic-only user.
export function openaiColumnState(i: { billingEnabled: boolean; hasData: boolean; error: string | null }): ColumnState {
  if (i.hasData) return { kind: 'data' };
  if (!i.billingEnabled) return { kind: 'hidden' };
  if (i.error) return { kind: 'error', message: i.error };
  return { kind: 'connect' };
}
