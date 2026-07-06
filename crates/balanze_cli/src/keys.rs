//! `set-openai-key` / `clear-openai-key` subcommands: store or remove the
//! OpenAI admin key in the OS keychain and flip the provider toggle.

use anyhow::{Result, anyhow};
use std::io;

pub(crate) fn cmd_set_openai_key() -> Result<()> {
    // Two input paths, by TTY status - never argv. A positional `sk-...` would
    // land in shell history / `ps`, which is the exact thing this command
    // exists to avoid:
    //   - Interactive TTY → masked input via rpassword (same pattern as
    //     `cmd_setup`'s prompt_for_openai_key).
    //   - Non-TTY (`echo $KEY | balanze-cli set-openai-key`) → read whole
    //     stdin to EOF. The pipe closes; no hang.
    use std::io::{IsTerminal, Read};

    let raw = if io::stdin().is_terminal() {
        rpassword::prompt_password("Paste your OpenAI API key (sk-...) and press Enter (hidden): ")
            .map_err(|e| anyhow!("failed to read key from stdin: {e}"))?
    } else {
        let mut input = String::new();
        let n = io::stdin().read_to_string(&mut input)?;
        if n == 0 {
            return Err(anyhow!(
                "stdin closed without input. Run interactively with a TTY, or pipe the key on stdin: `echo $KEY | balanze-cli set-openai-key`"
            ));
        }
        input
    };

    let key = raw.trim().to_string();
    if key.is_empty() {
        return Err(anyhow!("no key provided"));
    }
    if !key.starts_with("sk-") {
        return Err(anyhow!(
            "key doesn't look like an OpenAI key (expected to start with `sk-`)"
        ));
    }
    // Warn about non-admin keys but don't block - the API will reject them
    // and the user will see the specific error in the next status fetch.
    let is_admin_key = key.starts_with("sk-admin-");
    if !is_admin_key {
        eprintln!("Heads up: this doesn't look like an admin key. The organization/costs");
        eprintln!("endpoint Balanze uses requires an admin key (`sk-admin-...`); project keys");
        eprintln!("(`sk-proj-...`) and service-account keys will return 403 here. Create an");
        eprintln!("admin key at https://platform.openai.com/settings/organization/admin-keys");
        eprintln!("and replace this one if the next `balanze-cli` run shows an error.");
    }

    keychain::set(keychain::keys::OPENAI_API_KEY, &key)?;

    let mut s =
        settings::load_for_update().map_err(|e| anyhow!("{}: {e}", settings::UPDATE_LOAD_HINT))?;
    s.providers.openai_enabled = true;
    settings::save(&s)?;

    eprintln!(
        "Stored OpenAI key in the OS keychain ({} bytes).",
        key.len()
    );
    if is_admin_key {
        eprintln!("Run `balanze-cli` to verify the tile shows spend data.");
    }
    Ok(())
}

pub(crate) fn cmd_clear_openai_key() -> Result<()> {
    keychain::delete(keychain::keys::OPENAI_API_KEY)?;
    let mut s =
        settings::load_for_update().map_err(|e| anyhow!("{}: {e}", settings::UPDATE_LOAD_HINT))?;
    s.providers.openai_enabled = false;
    settings::save(&s)?;
    eprintln!("Removed OpenAI key from the keychain.");
    Ok(())
}
