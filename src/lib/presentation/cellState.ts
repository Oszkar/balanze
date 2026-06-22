// Pure cell/column state selection for the popover matrix. No Svelte, no IPC -
// just data in, a discriminated state out, so the rules are unit-testable.

export type QuotaState =
  | { kind: 'data' }
  | { kind: 'loading' }
  | { kind: 'notConfigured'; message: string }
  | { kind: 'error'; message: string };

export type ColumnState =
  | { kind: 'data' }
  | { kind: 'connect' }
  | { kind: 'error'; message: string }
  | { kind: 'hidden' };

// `unavailable` is the neutral "not configured" marker
// (Snapshot.claude_oauth_unavailable): Claude Code isn't installed, so the quota
// will never load. Distinct from a cold-start `loading` (which keeps the
// skeleton) and from an `error` (a failed fetch). Precedence: real data > error
// > not-configured > still-loading.
export function anthropicQuotaState(i: {
  hasQuota: boolean;
  error: string | null;
  unavailable: string | null;
}): QuotaState {
  if (i.hasQuota) return { kind: 'data' };
  if (i.error) return { kind: 'error', message: i.error };
  if (i.unavailable) return { kind: 'notConfigured', message: i.unavailable };
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
