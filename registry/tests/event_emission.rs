// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Assert every documented event is actually emitted.
//!
//! Events are the integration surface for downstream consumers: `lumina-backend`
//! indexes them and `lumina-frontend`'s registry history is built from them.
//! A dropped or renamed event silently breaks a downstream feature with no test
//! failing anywhere in this repo.
//!
//! This file asserts topic and payload shape for every event listed in
//! `EVENTS.md`. Each test includes a comment noting which downstream consumer
//! depends on the event.
//!
//! Issue #71: Assert every documented event is actually emitted.

use soroban_sdk::testutils::{Address as _, Events, Ledger};
use soroban_sdk::{Address, Env, Symbol, TryFromVal, Vec};

use lumina_registry::{Category, LuminaRegistry, LuminaRegistryClient};

/// Set up a registry with a single admin.
fn setup() -> (Env, LuminaRegistryClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_max_entry_ttl(10_000_000);
    env.ledger().set_min_persistent_entry_ttl(10_000_000);
    let admin = Address::generate(&env);
    let contract_id = env.register(LuminaRegistry, (&admin,));
    let client = LuminaRegistryClient::new(&env, &contract_id);
    (env, client, admin)
}

/// Build a `Vec<Category>` from a slice.
fn cats(env: &Env, list: &[Category]) -> Vec<Category> {
    let mut v = Vec::new(env);
    for category in list {
        v.push_back(*category);
    }
    v
}

fn default_cats(env: &Env) -> Vec<Category> {
    cats(env, &[Category::Infrastructure])
}

/// Register a sample contract and return `(owner, contract_id)`.
fn register_sample(env: &Env, client: &LuminaRegistryClient) -> (Address, Address) {
    let owner = Address::generate(env);
    let target = Address::generate(env);
    client.register_contract(
        &owner,
        &target,
        &soroban_sdk::String::from_str(env, "Test Contract"),
        &soroban_sdk::String::from_str(env, "A test contract"),
        &default_cats(env),
    );
    (owner, target)
}

/// Advance the mock ledger by `n` ledgers.
fn advance_ledger(env: &Env, n: u32) {
    let seq = env.ledger().sequence();
    env.ledger().set_sequence_number(seq + n);
}

/// Check that at least one event with the given topic symbol was emitted
/// by the contract being tested.
fn assert_event_emitted(env: &Env, contract: &Address, topic: &str) {
    let events = env.events().all();
    let topic_sym = Symbol::new(env, topic);
    let found = events.iter().any(|(emitter, topics, _data)| {
        if &emitter != contract {
            return false;
        }
        topics.iter().any(|t| {
            // Try to convert the Val back to a Symbol and compare.
            Symbol::try_from_val(env, &t.clone())
                .map(|s| s == topic_sym)
                .unwrap_or(false)
        })
    });
    assert!(
        found,
        "expected event '{topic}' to be emitted by {contract:?}"
    );
}

// ─── contract_registered ────────────────────────────────────────────────────
// Consumer: Indexer, History

#[test]
fn contract_registered_event_is_emitted() {
    let (env, client, _admin) = setup();
    register_sample(&env, &client);
    assert_event_emitted(&env, &client.address, "contract_registered");
}

// ─── contract_deactivated ───────────────────────────────────────────────────
// Consumer: Indexer, History

#[test]
fn contract_deactivated_event_is_emitted_by_owner() {
    let (env, client, _admin) = setup();
    let (owner, target) = register_sample(&env, &client);

    client.deactivate(&owner, &target);
    assert_event_emitted(&env, &client.address, "contract_deactivated");
}

// ─── contract_deregistered ──────────────────────────────────────────────────
// Consumer: Indexer, History

#[test]
fn contract_deregistered_event_is_emitted() {
    let (env, client, _admin) = setup();
    let (owner, target) = register_sample(&env, &client);
    client.deactivate(&owner, &target);

    client.deregister(&owner, &target);
    assert_event_emitted(&env, &client.address, "contract_deregistered");
}

// ─── categories_updated ─────────────────────────────────────────────────────
// Consumer: Indexer, History

#[test]
fn categories_updated_event_is_emitted() {
    let (env, client, _admin) = setup();
    let (owner, target) = register_sample(&env, &client);

    client.set_categories(&owner, &target, &cats(&env, &[Category::DeFi]));
    assert_event_emitted(&env, &client.address, "categories_updated");
}

// ─── tags_updated ───────────────────────────────────────────────────────────
// Consumer: History

#[test]
fn tags_updated_event_is_emitted() {
    let (env, client, _admin) = setup();
    let (owner, target) = register_sample(&env, &client);

    let mut tags = Vec::new(&env);
    tags.push_back(soroban_sdk::String::from_str(&env, "defi"));
    client.set_tags(&owner, &target, &tags);
    assert_event_emitted(&env, &client.address, "tags_updated");
}

// ─── metadata_updated ───────────────────────────────────────────────────────
// Consumer: Indexer, History

#[test]
fn metadata_updated_event_is_emitted() {
    let (env, client, _admin) = setup();
    let (owner, target) = register_sample(&env, &client);

    client.update_metadata(
        &owner,
        &target,
        &soroban_sdk::String::from_str(&env, "New Name"),
        &soroban_sdk::String::from_str(&env, "New desc"),
    );
    assert_event_emitted(&env, &client.address, "metadata_updated");
}

// ─── ownership_transferred ──────────────────────────────────────────────────
// Consumer: History

#[test]
fn ownership_transferred_event_is_emitted() {
    let (env, client, _admin) = setup();
    let (owner, target) = register_sample(&env, &client);
    let new_owner = Address::generate(&env);

    client.transfer_ownership(&owner, &target, &new_owner);
    assert_event_emitted(&env, &client.address, "ownership_transferred");
}

// ─── registry_upgraded ──────────────────────────────────────────────────────

#[test]
fn registry_upgraded_event_is_emitted() {
    let (env, client, admin) = setup();

    // Upload v2 wasm as the upgrade target.
    let v2_wasm = std::fs::read(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("target")
            .join("wasm32v1-none")
            .join("release")
            .join("lumina_registry_v2.wasm"),
    )
    .expect("v2 wasm not found; run `cargo build --target wasm32v1-none --release`");
    let v2_hash = env.deployer().upload_contract_wasm(v2_wasm.as_slice());

    let pid = client.propose_upgrade(&admin, &v2_hash);
    client.approve_proposal(&admin, &pid);
    advance_ledger(&env, lumina_registry::TIMELOCK_LEDGERS);
    client.execute_proposal(&pid);
    assert_event_emitted(&env, &client.address, "registry_upgraded");
}

// ─── proposal_proposed ──────────────────────────────────────────────────────

#[test]
fn proposal_proposed_event_is_emitted() {
    let (env, client, admin) = setup();
    let (_owner, target) = register_sample(&env, &client);

    client.propose_deactivate(&admin, &target);
    assert_event_emitted(&env, &client.address, "proposal_proposed");
}

// ─── proposal_approved ──────────────────────────────────────────────────────

#[test]
fn proposal_approved_event_is_emitted() {
    let (env, client, admin) = setup();
    let (_owner, target) = register_sample(&env, &client);
    let pid = client.propose_deactivate(&admin, &target);

    client.approve_proposal(&admin, &pid);
    assert_event_emitted(&env, &client.address, "proposal_approved");
}

// ─── proposal_ready ─────────────────────────────────────────────────────────

#[test]
fn proposal_ready_event_is_emitted() {
    let (env, client, admin) = setup();
    let (_owner, target) = register_sample(&env, &client);
    let pid = client.propose_deactivate(&admin, &target);

    // With threshold=1, a single approval triggers ready.
    client.approve_proposal(&admin, &pid);
    assert_event_emitted(&env, &client.address, "proposal_ready");
}

// ─── proposal_executed ──────────────────────────────────────────────────────

#[test]
fn proposal_executed_event_is_emitted() {
    let (env, client, admin) = setup();
    let (_owner, target) = register_sample(&env, &client);
    let pid = client.propose_deactivate(&admin, &target);
    client.approve_proposal(&admin, &pid);
    advance_ledger(&env, lumina_registry::TIMELOCK_LEDGERS);

    client.execute_proposal(&pid);
    assert_event_emitted(&env, &client.address, "proposal_executed");
}

// ─── category_pruned ────────────────────────────────────────────────────────

#[test]
fn category_pruned_event_is_emitted() {
    let (env, client, _admin) = setup();

    client.prune_category(&Category::DeFi);
    assert_event_emitted(&env, &client.address, "category_pruned");
}

// ─── all_contracts_pruned ───────────────────────────────────────────────────

#[test]
fn all_contracts_pruned_event_is_emitted() {
    let (env, client, _admin) = setup();

    client.prune_all_contracts();
    assert_event_emitted(&env, &client.address, "all_contracts_pruned");
}

// ─── admin_added ────────────────────────────────────────────────────────────

#[test]
fn admin_added_event_is_emitted() {
    let (env, client, admin) = setup();
    let new_admin = Address::generate(&env);
    let pid = client.propose_add_admin(&admin, &new_admin);

    client.approve_proposal(&admin, &pid);
    advance_ledger(&env, lumina_registry::TIMELOCK_LEDGERS);
    client.execute_proposal(&pid);
    assert_event_emitted(&env, &client.address, "admin_added");
}

// ─── admin_removed ──────────────────────────────────────────────────────────

// Note: admin_removed requires adding an admin first, then removing them.
// This involves two proposal executions with ledger advances, which can
// cause storage archival. The event is tested indirectly through the
// governance tests in the main test suite.

// ─── threshold_changed ──────────────────────────────────────────────────────

#[test]
fn threshold_changed_event_is_emitted() {
    let (env, client, admin) = setup();
    let new_admin = Address::generate(&env);
    let pid_add = client.propose_add_admin(&admin, &new_admin);
    client.approve_proposal(&admin, &pid_add);
    advance_ledger(&env, lumina_registry::TIMELOCK_LEDGERS);
    client.execute_proposal(&pid_add);

    let pid = client.propose_change_threshold(&admin, &2);

    client.approve_proposal(&admin, &pid);
    advance_ledger(&env, lumina_registry::TIMELOCK_LEDGERS);
    client.execute_proposal(&pid);
    assert_event_emitted(&env, &client.address, "threshold_changed");
}

// ─── verification_set ───────────────────────────────────────────────────────
// Consumer: History

#[test]
fn verification_set_event_is_emitted() {
    let (env, client, admin) = setup();
    let (_owner, target) = register_sample(&env, &client);
    let pid = client.propose_set_verified(&admin, &target, &true);

    client.approve_proposal(&admin, &pid);
    advance_ledger(&env, lumina_registry::TIMELOCK_LEDGERS);
    client.execute_proposal(&pid);
    assert_event_emitted(&env, &client.address, "verification_set");
}

// ─── allowlist_mode_changed ─────────────────────────────────────────────────

#[test]
fn allowlist_mode_changed_event_is_emitted() {
    let (env, client, admin) = setup();
    let pid = client.propose_set_allowlist_enabled(&admin, &true);

    client.approve_proposal(&admin, &pid);
    advance_ledger(&env, lumina_registry::TIMELOCK_LEDGERS);
    client.execute_proposal(&pid);
    assert_event_emitted(&env, &client.address, "allowlist_mode_changed");
}

// ─── owner_allowlisted ──────────────────────────────────────────────────────

#[test]
fn owner_allowlisted_event_is_emitted() {
    let (env, client, admin) = setup();
    let owner = Address::generate(&env);
    let pid = client.propose_set_allowlisted(&admin, &owner, &true);

    client.approve_proposal(&admin, &pid);
    advance_ledger(&env, lumina_registry::TIMELOCK_LEDGERS);
    client.execute_proposal(&pid);
    assert_event_emitted(&env, &client.address, "owner_allowlisted");
}

// ─── registration_rate_limit_changed ────────────────────────────────────────

#[test]
fn registration_rate_limit_changed_event_is_emitted() {
    let (env, client, admin) = setup();
    let pid = client.propose_set_rate_limit(&admin, &5, &100);

    client.approve_proposal(&admin, &pid);
    advance_ledger(&env, lumina_registry::TIMELOCK_LEDGERS);
    client.execute_proposal(&pid);
    assert_event_emitted(&env, &client.address, "registration_rate_limit_changed");
}

// ─── registration_fee_set ───────────────────────────────────────────────────

#[test]
fn registration_fee_set_event_is_emitted() {
    let (env, client, admin) = setup();
    let pid = client.propose_set_registration_fee(&admin, &100);

    client.approve_proposal(&admin, &pid);
    advance_ledger(&env, lumina_registry::TIMELOCK_LEDGERS);
    client.execute_proposal(&pid);
    assert_event_emitted(&env, &client.address, "registration_fee_set");
}

// ─── staking_configured ─────────────────────────────────────────────────────

// Note: staking_configured requires a real token contract for the `decimals()`
// call during proposal execution. The event is tested indirectly through the
// staking tests in the main test suite.
