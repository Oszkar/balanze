// Mirrors `settings::Settings` (crates/settings/src/lib.rs) over IPC.
// Secrets never appear here - API keys live in the OS keychain, not settings.

export interface ProviderSettings {
  openai_enabled: boolean;
  anthropic_enabled: boolean;
  codex_enabled: boolean;
}

export interface Settings {
  version: number;
  providers: ProviderSettings;
  oauth_poll_interval_secs: number;
  // Backend-owned first-run flag (the host sets it; set_settings preserves it
  // across frontend writes). Present on the IPC payload; the frontend never
  // needs to change it, but the mirror must carry it to stay accurate.
  seen_welcome: boolean;
}

// Mirrors `commands::StatuslineWire` - whether Claude Code's statusLine is
// wired to Balanze, free, or taken by another command.
export interface StatuslineWire {
  status: 'wired' | 'unwired' | 'occupied';
  command: string | null;
}
