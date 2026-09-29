// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
/// Turns a missing upgrade-test fixture into a message that says what to run.
///
/// The upgrade tests in `src/lib.rs` deploy the registry from its compiled
/// wasm, so `cargo test` needs `cargo build --target wasm32v1-none --release`
/// to have run first. Without this script the only symptom is `contractimport!`
/// reporting `No such file or directory` with no path and no remedy.
///
/// It also guards the hand-maintained `registry-v2` upgrade fixture: the
/// duplicated v2 storage types must match the real ones field-for-field,
/// otherwise the fixture silently stops testing anything.

use std::fs;
use std::path::PathBuf;

const FIXTURES: [&str; 2] = ["lumina_registry.wasm", "lumina_registry_v2.wasm"];
const TYPE_PACKAGES: [&str; 1] = ["ContractEntry"];

fn main() {
    // During the wasm build itself the fixtures are the thing being produced,
    // and the test module is not compiled at all — nothing to check.
    if std::env::var("CARGO_CFG_TARGET_ARCH")
        .as_deref()
        .map(|arch| arch == "wasm32")
        .unwrap_or(false)
    {
        return;
    }

    check_fixtures();
    check_v2_types_in_sync();
}

fn check_fixtures() {
    let release: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "target",
        "wasm32v1-none",
        "release",
    ]
    .iter()
    .collect();

    let missing: Vec<&str> = FIXTURES
        .iter()
        .filter(|name| {
            let path = release.join(name);
            println!("cargo::rerun-if-changed={}", path.display());
            !path.exists()
        })
        .copied()
        .collect();

    if !missing.is_empty() {
        println!(
            "cargo::warning=upgrade-test fixtures not built ({}). \
             Run `cargo build --target wasm32v1-none --release` before `cargo test`.",
            missing.join(", "),
        );
    }
}

fn extract_struct_fields(source: &str, name: &str) -> Option<Vec<(String, String)>> {
    let needle = format!("struct {name}");
    let start = source.find(&needle)?;
    let after = &source[start + needle.len()..];
    let open = after.find('{')?;
    if !after[..open].trim().is_empty() {
        return None;
    }

    let body = &after[open + 1..];
    let close = body.find('}')?;
    let body = &body[..close];

    let mut fields = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }

        let line = line.trim_end_matches(',').trim_end();
        let (type_part, name_part) = line.split_once(':')?;
        let field_name = name_part.trim().to_string();
        let field_type = type_part.trim();
        if field_name.is_empty() || field_type.is_empty() {
            return None;
        }
        fields.push((field_name, field_type.to_string()));
    }

    Some(fields)
}

fn check_v2_types_in_sync() {
    let mut failed = false;

    let canonical_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
    let duplicate_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../registry-v2/src/lib.rs");

    let canonical_src = match fs::read_to_string(&canonical_path) {
        Ok(s) => s,
        Err(e) => {
            println!("cargo::warning=could not read {}: {e}", canonical_path.display());
            return;
        }
    };
    let duplicate_src = match fs::read_to_string(&duplicate_path) {
        Ok(s) => s,
        Err(e) => {
            println!("cargo::warning=could not read {}: {e}", duplicate_path.display());
            return;
        }
    };

    for struct_name in TYPE_PACKAGES {
        println!("cargo::rerun-if-changed={}", canonical_path.display());
        println!("cargo::rerun-if-changed={}", duplicate_path.display());

        let canonical_fields = extract_struct_fields(&canonical_src, struct_name);
        let duplicate_fields = extract_struct_fields(&duplicate_src, struct_name);

        match (canonical_fields, duplicate_fields) {
            (Some(c), Some(d)) if c == d => {}
            (Some(c), Some(d)) => {
                println!(
                    "cargo::warning=`registry-v2` fixture drifted: `{struct_name}` in {} no longer matches {}. Expected fields {:?}, found {:?}. Update the duplicated type in {} to match, or if the change is intentional, regenerate the `registry-v2` fixture and commit it with the storage change.",
                    duplicate_path.display(),
                    canonical_path.display(),
                    c,
                    d,
                    duplicate_path.display(),
                );
                failed = true;
            }
            (None, _) => {
                println!(
                    "cargo::warning=could not locate `struct {struct_name}` in {}. The `registry-v2` check needs this type to compare against {}.",
                    canonical_path.display(),
                    duplicate_path.display(),
                );
                failed = true;
            }
            (_, None) => {
                println!(
                    "cargo::warning=could not locate `struct {struct_name}` in {}. The `registry-v2` fixture is supposed to duplicate this type.",
                    duplicate_path.display(),
                );
                failed = true;
            }
        }
    }

    if failed {
        panic!(
            "`registry-v2` fixture is out of sync with the real storage types. Regenerate the fixture and commit it together with the storage change."
        );
    }
}
