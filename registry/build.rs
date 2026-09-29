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
use std::path::PathBuf;

const FIXTURES: [&str; 2] = ["lumina_registry.wasm", "lumina_registry_v2.wasm"];

/// Storage types that the `registry-v2` fixture duplicates. Each entry lists
/// the canonical source, the hand-maintained copy, and the type names that
/// must stay byte-compatible between the two.
const TYPE_PACKAGES: [(&str, &str, &[&str]); 1] = [(
    "src/lib.rs",
    "../registry-v2/src/lib.rs",
    &["ContractEntry", "DataKey"],
)];

fn main() {
    // During the wasm build itself the fixtures are the thing being produced,
    // and the test module is not compiled at all — nothing to check.
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
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

/// Strips the SDK path prefix so `soroban_sdk::String` and `String` compare
/// as the same storage type. The fixture writes the long form because it does
/// not import `String`.
fn normalize_type(ty: &str) -> String {
    ty.replace("soroban_sdk::", "")
}

/// Locates the body of a `pub struct`/`pub enum` definition, rejecting
/// generics or attributes between the name and the brace.
fn type_body<'a>(source: &'a str, needle: &str) -> Option<&'a str> {
    let start = source.find(needle)?;
    let after = &source[start + needle.len()..];
    let open = after.find('{')?;
    if !after[..open].trim().is_empty() {
        return None;
    }

    let body = &after[open + 1..];
    let close = body.find('}')?;
    Some(&body[..close])
}

/// Extracts the field names and types of a `struct` definition from source.
/// Returns `None` when the struct is not found or is not a plain field list.
fn extract_struct_fields(source: &str, name: &str) -> Option<Vec<(String, String)>> {
    let body = type_body(source, &format!("pub struct {name}"))?;

    let mut fields = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }

        let line = line.trim_end_matches(',').trim_end();
        let (name_part, type_part) = line.split_once(':')?;
        let name = name_part.trim();
        // Drop the visibility: the canonical type writes `pub field`, the
        // fixture does too, but only the name and type are being compared.
        let name = name.trim_start_matches("pub ").trim();
        let type_part = type_part.trim();
        if name.is_empty() || type_part.is_empty() {
            return None;
        }
        fields.push((name.to_string(), normalize_type(type_part)));
    }

    Some(fields)
}

/// Extracts the variant names and payloads of an `enum` definition from
/// source. A unit variant has an empty payload.
fn extract_enum_variants(source: &str, name: &str) -> Option<Vec<(String, String)>> {
    let body = type_body(source, &format!("pub enum {name}"))?;

    let mut variants = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let line = line.trim_end_matches(',').trim_end();
        // Drop a trailing discriminant (`= 2`) if the enum has one.
        let line = line.split('=').next().unwrap_or(line).trim();
        if line.is_empty() {
            continue;
        }
        let (variant, payload) = match line.split_once('(') {
            Some((variant, rest)) => (
                variant.trim().to_string(),
                normalize_type(rest.trim_end_matches(')').trim()),
            ),
            None => (line.to_string(), String::new()),
        };
        variants.push((variant, payload));
    }

    Some(variants)
}

/// Reports a struct or enum in the fixture that no longer matches the real one.
fn drift(kind: &str, name: &str, duplicate: &str, canonical: &str) {
    println!(
        "cargo::warning=`registry-v2` fixture drifted: `{name}` in {duplicate} \
         no longer matches the {kind} in {canonical}. Update the duplicated \
         definition in {duplicate} to match, or if the change is intentional, \
         regenerate the `registry-v2` fixture and commit it with the storage \
         change. See the \"Upgrade fixture\" section in the README.",
    );
}

fn check_v2_types_in_sync() {
    let mut failed = false;

    for (canonical, duplicate, type_names) in TYPE_PACKAGES {
        println!("cargo::rerun-if-changed={}", canonical);
        println!("cargo::rerun-if-changed={}", duplicate);

        let canonical_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(canonical);
        let duplicate_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(duplicate);
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
        for type_name in type_names {
            let canonical_fields = extract_struct_fields(&canonical_src, type_name);
            let duplicate_fields = extract_struct_fields(&duplicate_src, type_name);

            match (canonical_fields, duplicate_fields) {
                (Some(c), Some(d)) => {
                    if c != d {
                        drift("struct", type_name, duplicate, canonical);
                        println!("cargo::warning=expected fields {c:?}, found {d:?}");
                        failed = true;
                    }
                    continue;
                }
                (None, None) => {}
                (Some(_), None) => {
                    println!(
                        "cargo::warning=could not locate `struct {type_name}` in {}. \
                         The `registry-v2` fixture is supposed to duplicate this type. \
                         Update it (see the \"Upgrade fixture\" section in the README) \
                         or update TYPE_PACKAGES in build.rs if it was renamed.",
                        duplicate,
                    );
                    failed = true;
                    continue;
                }
                (None, _) => {}
            }

            // Not a struct: the other duplicated storage type is an enum.
            let canonical_variants = extract_enum_variants(&canonical_src, type_name);
            let duplicate_variants = extract_enum_variants(&duplicate_src, type_name);
            match (canonical_variants, duplicate_variants) {
                (Some(c), Some(d)) => {
                    // The fixture only declares the variants it reads, so it
                    // may be a subset — but every variant it does declare must
                    // encode exactly as the real one does.
                    let mut missing = false;
                    for variant in &d {
                        if !c.contains(variant) {
                            missing = true;
                            println!(
                                "cargo::warning=`registry-v2` fixture drifted: {type_name}::{} \
                                 is declared as {variant:?} in {duplicate} but not in {canonical}.",
                                variant.0,
                            );
                        }
                    }
                    if missing {
                        drift("enum", type_name, duplicate, canonical);
                        failed = true;
                    }
                }
                (None, None) => {
                    println!(
                        "cargo::warning=could not locate `{type_name}` in {}. \
                         The `registry-v2` check needs this type to compare against {}. \
                         Update the check in build.rs if the type was renamed or moved.",
                        canonical, duplicate,
                    );
                    failed = true;
                }
                (Some(_), None) => {
                    println!(
                        "cargo::warning=could not locate `pub enum {type_name}` in {}. \
                         The `registry-v2` fixture is supposed to duplicate this type. \
                         Update it (see the \"Upgrade fixture\" section in the README) \
                         or update TYPE_PACKAGES in build.rs if it was renamed.",
                        duplicate,
                    );
                    failed = true;
                }
                (None, Some(_)) => {
                    println!(
                        "cargo::warning=could not locate `{type_name}` in {}. \
                         The `registry-v2` check needs this type to compare against {}. \
                         Update TYPE_PACKAGES in build.rs if it was renamed or moved.",
                        canonical, duplicate,
                    );
                    failed = true;
                }
            }
        }
    }

    if failed {
        panic!(
            "`registry-v2` fixture is out of sync with the real storage types. \
             Update the fixture and commit it together with the storage change."
        );
    }
}