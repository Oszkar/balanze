//! Build script for `claude_cost`.
//!
//! Parses the single `data/litellm-prices-<commit>-<YYYYMMDD>.json` filename
//! and emits `PRICE_TABLE_COMMIT` and `PRICE_TABLE_DATE` env vars that
//! `lib.rs` exposes as `pub const`s via `env!()`. This keeps the const
//! values in lockstep with the vendored data file: a refresh swaps the
//! file, the build script picks up the new name, and the consts update —
//! no manual editing required.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=data/");

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let data_dir = Path::new(manifest_dir).join("data");

    let entries: Vec<_> = std::fs::read_dir(&data_dir)
        .expect("data/ directory must exist relative to the crate manifest")
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("litellm-prices-") && name.ends_with(".json")
        })
        .collect();

    assert_eq!(
        entries.len(),
        1,
        "expected exactly one litellm-prices-*.json file in data/, found {}",
        entries.len()
    );

    let filename = entries[0].file_name().to_string_lossy().to_string();
    let stem = filename
        .strip_prefix("litellm-prices-")
        .and_then(|s| s.strip_suffix(".json"))
        .unwrap_or_else(|| {
            panic!("filename must match litellm-prices-<commit>-<YYYYMMDD>.json: {filename}")
        });

    let (commit, date) = stem
        .rsplit_once('-')
        .unwrap_or_else(|| panic!("filename stem must contain a date suffix: {stem}"));

    assert_eq!(
        date.len(),
        8,
        "date suffix must be YYYYMMDD (8 chars), got {date:?}"
    );
    assert!(
        date.chars().all(|c| c.is_ascii_digit()),
        "date suffix must be all digits: {date:?}"
    );
    assert!(
        !commit.is_empty(),
        "commit segment of filename must be non-empty"
    );

    let pretty_date = format!("{}-{}-{}", &date[0..4], &date[4..6], &date[6..8]);

    println!("cargo:rustc-env=PRICE_TABLE_COMMIT={commit}");
    println!("cargo:rustc-env=PRICE_TABLE_DATE={pretty_date}");
}
