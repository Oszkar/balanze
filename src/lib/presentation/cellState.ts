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

export function openaiColumnState(i: { enabled: boolean; hasData: boolean; error: string | null }): ColumnState {
  if (!i.enabled) return { kind: 'hidden' };
  if (i.hasData) return { kind: 'data' };
  if (i.error) return { kind: 'error', message: i.error };
  return { kind: 'connect' };
}
