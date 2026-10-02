// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Both wasm targets the workspace builds must be targets the Soroban host accepts.
//!
//! CI builds `wasm32v1-none` — the target the README, `stellar contract build`
//! and the upgrade tests all use — and `wasm32-unknown-unknown`, the target a
//! plain `cargo build` picks. They do not produce the same module: on current
//! Rust the second one encodes `call_indirect` with a reference-types table
//! index, and the host refuses to translate it (README, "Build & test"; the
//! same warning is in DEPLOY.md, where uploading such a module would brick the
//! contract's only upgrade path).
//!
//! So the artifacts of *both* targets are loaded into the host here:
//!
//! * the `wasm32v1-none` artifacts must load and answer, because those are the
//!   bytes we ship — a toolchain bump that makes them host-incompatible fails
//!   this test instead of a deployment;
//! * the `wasm32-unknown-unknown` artifacts must be rejected for exactly the
//!   documented reason. The day that stops being true, in either direction,
//!   this test fails and the README, DEPLOY.md and the toolchain pin get
//!   revisited together rather than one of them quietly going stale.
//!
//! `make test` builds both targets before running this file. A plain
//! `cargo test` without a prior `make wasm-both` has only the shipped
//! artifacts, and the canary half is skipped with a note rather than failed.

use soroban_sdk::{testutils::Address as _, Address, Env, Symbol};
use std::path::PathBuf;

/// The target the project ships: `stellar contract build` and `make build`.
const SHIPPED_TARGET: &str = "wasm32v1-none";
/// The target a plain `cargo build --target wasm32-unknown-unknown` produces.
const CANARY_TARGET: &str = "wasm32-unknown-unknown";

/// One deployable module: the file `cargo build --release` leaves behind, the
/// constructor arguments it needs to be instantiated, and a view to call so
/// that "loaded" means "ran" and not only "parsed".
struct Module {
    file: &'static str,
    /// `lumina_registry` stores a bootstrap admin in its constructor; the
    /// other two modules have no constructor at all.
    bootstrap_admin: bool,
    /// A view taking no arguments and returning a `u32`.
    view: Option<&'static str>,
}

const MODULES: &[Module] = &[
    Module {
        file: "lumina_registry.wasm",
        bootstrap_admin: true,
        view: Some("get_version"),
    },
    Module {
        file: "lumina_registry_v2.wasm",
        bootstrap_admin: false,
        view: Some("get_version"),
    },
    Module {
        file: "lumina_registry_consumer_example.wasm",
        bootstrap_admin: false,
        view: None,
    },
];

/// Where `cargo` leaves an artifact, relative to this crate.
fn artifact(target: &str, file: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.push("target");
    path.push(target);
    path.push("release");
    path.push(file);
    path
}

/// Upload, translate, instantiate and return the address of `wasm`, exactly as
/// a deployment would. A module the host cannot translate makes this panic.
fn load(env: &Env, wasm: &[u8], bootstrap_admin: bool) -> Address {
    // The registry's constructor authorizes the admin outside the root
    // invocation, which the plain all-auths mock rejects.
    env.mock_all_auths_allowing_non_root_auth();
    if bootstrap_admin {
        let admin = Address::generate(env);
        env.register(wasm, (&admin,))
    } else {
        env.register(wasm, ())
    }
}

/// Run `f`, turning a host panic into the message it was raised with so an
/// assertion can say *why* a module was refused.
fn caught<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    use std::panic::AssertUnwindSafe;
    use std::sync::{Arc, Mutex};

    let captured = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&captured);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        *sink.lock().unwrap_or_else(|p| p.into_inner()) = info.to_string();
    }));

    let outcome = std::panic::catch_unwind(AssertUnwindSafe(f));
    std::panic::set_hook(previous);

    match outcome {
        Ok(value) => Ok(value),
        Err(_) => Err(captured.lock().unwrap_or_else(|p| p.into_inner()).clone()),
    }
}

/// The bytes we ship have to be bytes the host accepts: uploaded, translated,
/// instantiated, and able to answer a view.
#[test]
fn the_shipped_target_loads_and_runs_in_the_host() {
    for module in MODULES {
        let path = artifact(SHIPPED_TARGET, module.file);
        let wasm = std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "{} is missing ({e}); run `make build` before `cargo test`",
                path.display()
            )
        });

        let env = Env::default();
        let contract = load(&env, &wasm, module.bootstrap_admin);

        if let Some(view) = module.view {
            let version: u32 =
                env.invoke_contract(&contract, &Symbol::new(&env, view), soroban_sdk::vec![&env]);
            assert!(version > 0, "{} answered {view} = {version}", module.file);
        }
    }
}

/// The other target is built only to keep the README's warning honest: it must
/// stay rejected, and rejected for the reason the README gives.
#[test]
fn the_canary_target_is_refused_for_the_documented_reason() {
    for module in MODULES {
        let path = artifact(CANARY_TARGET, module.file);
        let wasm = match std::fs::read(&path) {
            Ok(wasm) => wasm,
            Err(_) => {
                eprintln!(
                    "note: {} is not built, skipping its canary check; run `make wasm-both` to build it",
                    path.display()
                );
                continue;
            }
        };

        let outcome = caught(|| {
            let env = Env::default();
            load(&env, &wasm, module.bootstrap_admin)
        });

        match outcome {
            Ok(_) => panic!(
                "{} built for {CANARY_TARGET} loaded in the host. README and DEPLOY.md both say \
                 that target's output is refused, so at least one of them, the pinned toolchain \
                 or this test is now wrong — read them again before shipping those bytes.",
                module.file
            ),
            Err(message) => {
                eprintln!(
                    "{CANARY_TARGET}/{} refused by the host: {message}",
                    module.file
                );
                assert!(
                    message.contains("reference-types not enabled"),
                    "{CANARY_TARGET}/{} was refused for a reason other than the documented \
                     reference-types one: {message}",
                    module.file
                );
            }
        }
    }
}
