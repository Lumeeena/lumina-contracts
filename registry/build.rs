//! Turns a missing upgrade-test fixture into a message that says what to run.
//!
//! The upgrade tests in `src/lib.rs` deploy the registry from its compiled
//! wasm, so `cargo test` needs `cargo build --target wasm32v1-none --release`
//! to have run first. Without this script the only symptom is `contractimport!`
//! reporting `No such file or directory` with no path and no remedy.

use std::path::PathBuf;

const FIXTURES: [&str; 2] = ["lumina_registry.wasm", "lumina_registry_v2.wasm"];

fn main() {
    // During the wasm build itself the fixtures are the thing being produced,
    // and the test module is not compiled at all — nothing to check.
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        return;
    }

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
