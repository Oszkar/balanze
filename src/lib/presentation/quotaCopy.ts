// Single-sourced Anthropic quota-state copy, shared by GridView and CardsView so
// the two views can never drift on wording (the cold-start hint, the ~/.claude
// path, the error caption, etc.). The dynamic reason (the snapshot's
// unavailable/error message) is injected by the caller; everything else is static.
export const ANTH_QUOTA_COPY = {
  error: {
    note: 'quota fetch failed',
    title: (message: string) => `Anthropic quota unavailable - ${message}`,
  },
  notConfigured: {
    hint: 'Balanze reads your local Claude usage',
    title:
      'Claude Code not detected. Balanze reads your local Claude usage at ~/.claude; install Claude Code (or restart Balanze after installing) to track quota.',
  },
  loading: {
    heading: 'Connecting to Claude...',
    sub: 'first check can take a minute',
    title:
      'Balanze is fetching your Claude usage for the first time. Wire Balanze as your Claude statusLine in Settings for instant live quota.',
  },
} as const;
