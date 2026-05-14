# Security policy

## Supported versions

Balanze is a personal-use side project. The latest commit on `main` is the only
supported version. Tagged releases (`v0.1.0+` once they exist) receive
patch-level fixes only while they're current; older tags are not back-patched.

## Reporting a vulnerability

If you've found a security issue — credential exposure, a way to read or
exfiltrate the user's API keys or Claude OAuth tokens, a bug in the secret
storage path, or anything else that would compromise a Balanze user's local
machine — **please do not file a public GitHub issue**.

Email **mail@oszkar.me** with:

- a description of the issue,
- steps to reproduce (a minimal repro is gold),
- the commit hash you tested against,
- your assessment of impact (what an attacker could do).

You should expect a first response within a week. If you don't hear back in
that window, feel free to ping me publicly (e.g., GitHub issue *without
reproduction details*) and I'll pick it up.

No bug-bounty program. I'll credit you in the fix commit / release notes if
you want public acknowledgement, anonymous if you prefer.

## Scope

In scope:

- Anything that exposes a user's API keys (`sk-admin-…`, OpenAI project keys)
  outside the OS keychain or the user's own `BALANZE_OPENAI_KEY` env var.
- Anything that exposes the contents of `~/.claude/.credentials.json` — the
  OAuth `accessToken`, `refreshToken`, or any sub-field.
- Logging or telemetry that includes secrets (none should exist; AGENTS.md
  §3.4 forbids it).
- Code paths in `crates/keychain`, `crates/anthropic_oauth::credentials`,
  `crates/settings`, or the CLI's `set-openai-key` flow.
- Network-side issues: the OAuth / Admin Costs / future Console clients
  should treat all responses as untrusted input and never echo secret-shaped
  values from server replies.

Out of scope:

- Vulnerabilities in dependencies (`tauri`, `reqwest`, `keyring`,
  `tracing`, etc.) — please report those upstream. If you find one with
  Balanze-specific impact, do flag it here too.
- Issues that require an attacker who already has read access to the user's
  filesystem (they already have everything Balanze touches).
- Denial-of-service against the Balanze CLI itself (it's a single-shot,
  user-invoked tool; restart fixes it).
- Anthropic / OpenAI API account-takeover concerns that aren't Balanze-
  specific — report those to the provider.

## What "responsible" looks like for this project

This is one person's side project. I'll fix critical issues fast; I may take
longer on hardening or defense-in-depth changes. If you're triaging multiple
projects and have to pick where to spend your time, please don't pick this
one over a project that more people depend on. But if you're already here,
thank you — a heads-up email is genuinely appreciated.
