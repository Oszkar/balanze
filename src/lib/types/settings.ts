// Mirrors `settings::Settings` (crates/settings/src/lib.rs) over IPC.
// Secrets never appear here - API keys live in the OS keychain, not settings.

export interface ProviderSettings {
  openai_enabled: boolean;
  anthropic_enabled: boolean;
}

export interface Settings {
  version: number;
  providers: ProviderSettings;
  oauth_poll_interval_secs: number;
}
