//! "Save Diagnostics..." bundle: a plain folder of redacted rotated logs plus
//! the current host snapshot and a README describing both. Allowlist only -
//! never settings, credentials, the OpenAI cost store, or provider data
//! (AGENTS.md §3.4). Nothing is uploaded.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub struct BundleSources {
    pub log_dir: Option<PathBuf>,
    pub snapshot_path: Option<PathBuf>,
}

/// Resolve the real sources and destination, then [`build_bundle`].
/// Destination is the user's Downloads folder, falling back to home.
pub fn save_bundle(snapshot_path: Option<PathBuf>) -> io::Result<PathBuf> {
    let user_dirs = directories::UserDirs::new()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no user directories"))?;
    let dest = user_dirs
        .download_dir()
        .unwrap_or_else(|| user_dirs.home_dir())
        .to_path_buf();
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let sources = BundleSources {
        log_dir: settings::log_dir(),
        snapshot_path,
    };
    build_bundle(&sources, &dest, &stamp)
}

/// Create `<dest_parent>/Balanze-diagnostics-<stamp>/` and fill it. Fails with
/// `AlreadyExists` rather than merging into an existing folder; removes the
/// partial folder on any later failure.
pub fn build_bundle(
    sources: &BundleSources,
    dest_parent: &Path,
    stamp: &str,
) -> io::Result<PathBuf> {
    let dir = dest_parent.join(format!("Balanze-diagnostics-{stamp}"));
    fs::create_dir(&dir)?;
    match fill(sources, &dir) {
        Ok(()) => Ok(dir),
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

fn fill(sources: &BundleSources, dir: &Path) -> io::Result<()> {
    let mut included = Vec::new();
    if let Some(log_dir) = &sources.log_dir {
        for name in log_file_names(log_dir)? {
            copy_redacted(&log_dir.join(&name), &dir.join(&name))?;
            included.push(name);
        }
    }
    if let Some(snapshot) = &sources.snapshot_path {
        match copy_redacted(snapshot, &dir.join("snapshot.json")) {
            Ok(()) => included.push("snapshot.json".to_string()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    fs::write(dir.join("README.txt"), readme(&included))
}

/// Sorted `balanze-*.log` regular files; a missing log dir is empty, not an error.
fn log_file_names(log_dir: &Path) -> io::Result<Vec<String>> {
    let entries = match fs::read_dir(log_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("balanze-") && n.ends_with(".log"))
        .collect();
    names.sort();
    Ok(names)
}

/// Copy with redaction applied per line (secrets never span lines, and it
/// bounds the scanner's per-match cost). Invalid UTF-8 is replaced, not fatal.
fn copy_redacted(src: &Path, dst: &Path) -> io::Result<()> {
    let bytes = fs::read(src)?;
    let text = String::from_utf8_lossy(&bytes);
    let redacted: String = text
        .split_inclusive('\n')
        .map(redact::redact_secrets)
        .collect();
    fs::write(dst, redacted)
}

fn readme(included: &[String]) -> String {
    let mut s = String::from(
        "Balanze diagnostics\n\
         ===================\n\n\
         This folder was created by Balanze's \"Save Diagnostics...\" menu item.\n\
         Nothing has been uploaded. Share it only with someone you choose.\n\n\
         Included:\n",
    );
    if included.is_empty() {
        s.push_str("  (no logs or snapshot were found)\n");
    }
    for name in included {
        s.push_str("  - ");
        s.push_str(name);
        s.push('\n');
    }
    s.push_str(
        "\nWhat these files contain:\n\
         - Logs: up to the last 3 days of Balanze app and CLI log lines.\n\
         - snapshot.json: the latest usage figures (quota percentages, spend,\n\
           token counts) and any source error messages.\n\
         - File paths in either may include your OS user name.\n\
         - Secret-shaped strings (API keys, OAuth tokens, Bearer tokens) are\n\
           replaced with ...REDACTED before writing.\n\n\
         Not included: Balanze settings, API keys, Claude or Codex credentials,\n\
         Claude or Codex session files, or the OpenAI cost cache.\n",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn planted_key() -> String {
        format!("sk-ant-oat01-{}", "Q".repeat(40))
    }

    fn setup() -> (tempfile::TempDir, BundleSources, tempfile::TempDir) {
        let data = tempfile::tempdir().unwrap();
        let logs = data.path().join("logs");
        fs::create_dir(&logs).unwrap();
        let key = planted_key();
        fs::write(
            logs.join("balanze-gui.2026-11-20.log"),
            format!("INFO start\nWARN body {key}\n"),
        )
        .unwrap();
        fs::write(logs.join("balanze-cli.2026-11-19.log"), "INFO cli\n").unwrap();
        fs::write(logs.join("notes.txt"), "not a log").unwrap();
        let snapshot = data.path().join("snapshot.json");
        fs::write(
            &snapshot,
            format!(r#"{{"err":"Bearer {}"}}"#, "Z".repeat(30)),
        )
        .unwrap();
        fs::write(data.path().join("settings.json"), "{}").unwrap();
        let sources = BundleSources {
            log_dir: Some(logs),
            snapshot_path: Some(snapshot),
        };
        let dest = tempfile::tempdir().unwrap();
        (data, sources, dest)
    }

    fn read_all(dir: &Path) -> String {
        let mut all = String::new();
        for e in fs::read_dir(dir).unwrap() {
            all.push_str(&fs::read_to_string(e.unwrap().path()).unwrap());
        }
        all
    }

    #[test]
    fn planted_key_does_not_survive_into_bundle() {
        let (_data, sources, dest) = setup();
        let out = build_bundle(&sources, dest.path(), "20261120-183000").unwrap();
        let all = read_all(&out);
        assert!(!all.contains(&planted_key()), "key leaked into bundle");
        assert!(!all.contains(&"Z".repeat(30)), "bearer leaked into bundle");
        assert!(all.contains("sk-...REDACTED"));
        assert!(all.contains("Bearer ...REDACTED"));
    }

    #[test]
    fn bundle_contains_only_allowlisted_files() {
        let (_data, sources, dest) = setup();
        let out = build_bundle(&sources, dest.path(), "20261120-183000").unwrap();
        assert_eq!(out, dest.path().join("Balanze-diagnostics-20261120-183000"));
        let mut names: Vec<String> = fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "README.txt",
                "balanze-cli.2026-11-19.log",
                "balanze-gui.2026-11-20.log",
                "snapshot.json"
            ]
        );
    }

    #[test]
    fn readme_lists_contents_and_exclusions() {
        let (_data, sources, dest) = setup();
        let out = build_bundle(&sources, dest.path(), "s").unwrap();
        let readme = fs::read_to_string(out.join("README.txt")).unwrap();
        assert!(readme.contains("balanze-gui.2026-11-20.log"));
        assert!(readme.contains("snapshot.json"));
        assert!(readme.contains("Nothing has been uploaded"));
        assert!(readme.contains("OS user name"));
    }

    #[test]
    fn missing_sources_still_produce_a_readme() {
        let dest = tempfile::tempdir().unwrap();
        let sources = BundleSources {
            log_dir: Some(dest.path().join("absent-logs")),
            snapshot_path: Some(dest.path().join("absent.json")),
        };
        let out = build_bundle(&sources, dest.path(), "s").unwrap();
        let names: Vec<_> = fs::read_dir(&out).unwrap().collect();
        assert_eq!(names.len(), 1);
        assert!(out.join("README.txt").exists());
    }

    #[test]
    fn existing_folder_is_never_merged_into() {
        let (_data, sources, dest) = setup();
        fs::create_dir(dest.path().join("Balanze-diagnostics-s")).unwrap();
        let err = build_bundle(&sources, dest.path(), "s").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }
}
