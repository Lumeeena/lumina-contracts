// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Pins the set of exported entrypoint names against a hand-maintained
//! allowlist.
//!
//! The `#[contractimpl]` block in `registry/src/lib.rs` exports every `pub fn`
//! it contains as an on-chain entrypoint. Adding a function to the wrong impl
//! block silently grows the public interface. `registry/tests/interface.rs`
//! catches the full signature drift against `registry/interface.snap`;
//! this test is a complementary, minimal name-only check that is cheaper to
//! reason about during review — a reviewer looking at this file can tell
//! whether a new export is intentional without reading rendered types.
//!
//! The allowlist here is **the source of truth for reviewers**. A failing
//! test means one of three things happened:
//!
//! 1. A new entrypoint was added without updating this list — probably
//!    intentional, so add its name below in the same PR.
//! 2. An entrypoint was renamed or removed — update the list too.
//! 3. A helper intended to be private was accidentally added to the
//!    `#[contractimpl]` block — move it to the plain `impl LuminaRegistry`
//!    block below `#[contractimpl]`, which does not export.
//!
//! See the "Contract surface" section in `ARCHITECTURE.md` for the
//! two-impl-block convention this test guards.
//!
//! Issue #255: Add a test asserting no entrypoint is unintentionally public.

use soroban_sdk::xdr::ScSpecEntry;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Every entrypoint the registry wasm is allowed to export. Alphabetical,
/// so a new entry sorts into one place and a diff reads cleanly. Keep
/// `__constructor` last so the leading-underscore sort order does not
/// move with locale.
const EXPECTED_EXPORTS: &[&str] = &[
    "__constructor",
    "approve_proposal",
    "attest",
    "can_slash",
    "deactivate",
    "deregister",
    "execute_proposal",
    "get_active_category_count",
    "get_active_contract_count",
    "get_active_contract_ids",
    "get_active_contracts",
    "get_active_contracts_after",
    "get_active_contracts_by_category",
    "get_active_contracts_by_stake",
    "get_active_contracts_page",
    "get_active_profiles",
    "get_active_profiles_by_stake",
    "get_active_profiles_page",
    "get_admin",
    "get_admin_weight",
    "get_admins",
    "get_attestations",
    "get_categories",
    "get_contract",
    "get_contract_config",
    "get_contract_count",
    "get_contract_profile",
    "get_contracts_by_categories",
    "get_contracts_by_category_after",
    "get_contracts_by_owner",
    "get_contracts_by_owner_after",
    "get_minimum_stake",
    "get_proposal",
    "get_registration_fee",
    "get_registry_stats",
    "get_reputation",
    "get_slashes",
    "get_stake",
    "get_staking_config",
    "get_tags",
    "get_threshold",
    "get_total_admin_weight",
    "get_total_registered",
    "get_version",
    "initialize",
    "is_registered",
    "is_verified",
    "propose_add_admin",
    "propose_batch",
    "propose_change_threshold",
    "propose_configure_minimum_stake",
    "propose_configure_staking",
    "propose_deactivate",
    "propose_remove_admin",
    "propose_set_admin_weight",
    "propose_set_allowlist_enabled",
    "propose_set_allowlisted",
    "propose_set_paused",
    "propose_set_rate_limit",
    "propose_set_registration_fee",
    "propose_set_verified",
    "propose_slash",
    "propose_upgrade",
    "propose_withdraw_from_treasury",
    "prune_all_contracts",
    "prune_category",
    "register_contract",
    "register_contracts",
    "renew",
    "respond_to_slash",
    "revoke_attestation",
    "set_categories",
    "set_superseded_by",
    "set_tags",
    "stake",
    "transfer_ownership",
    "update_metadata",
    "update_metadata_uri",
    "withdraw_stake",
];

fn wasm_path() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.extend([
        "..",
        "target",
        "wasm32v1-none",
        "release",
        "lumina_registry.wasm",
    ]);
    path
}

fn exported_function_names() -> BTreeSet<String> {
    let path = wasm_path();
    let wasm = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {} ({e}). Run `cargo build --target wasm32v1-none --release` first.",
            path.display()
        )
    });
    let entries = soroban_spec::read::from_wasm(&wasm).expect("wasm has no readable contract spec");
    entries
        .iter()
        .filter_map(|e| match e {
            ScSpecEntry::FunctionV0(f) => Some(f.name.0.to_utf8_string_lossy()),
            _ => None,
        })
        .collect()
}

#[test]
fn exported_functions_match_allowlist() {
    let exported = exported_function_names();
    let expected: BTreeSet<String> = EXPECTED_EXPORTS.iter().map(|s| (*s).to_string()).collect();

    let unexpected: Vec<&String> = exported.difference(&expected).collect();
    let missing: Vec<&String> = expected.difference(&exported).collect();

    if unexpected.is_empty() && missing.is_empty() {
        return;
    }

    let mut report = String::new();
    if !unexpected.is_empty() {
        report.push_str(
            "\nUnexpected exported entrypoints (in the wasm, not in the allowlist):\n",
        );
        for name in &unexpected {
            report.push_str(&format!("  + {name}\n"));
        }
        report.push_str(
            "\nIf the export is intentional, add the name to EXPECTED_EXPORTS in \
             registry/tests/exports_allowlist.rs. If it is not, move the helper out \
             of the #[contractimpl] block and into the plain `impl LuminaRegistry` \
             block that follows it — see ARCHITECTURE.md § Contract surface.\n",
        );
    }
    if !missing.is_empty() {
        report.push_str(
            "\nExpected entrypoints missing from the wasm (in the allowlist, not in the wasm):\n",
        );
        for name in &missing {
            report.push_str(&format!("  - {name}\n"));
        }
        report.push_str(
            "\nIf the removal or rename is intentional, drop the name from \
             EXPECTED_EXPORTS and update registry/interface.snap in the same change.\n",
        );
    }
    panic!("{report}");
}
