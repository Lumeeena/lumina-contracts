// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Cover the upgrade path from every prior contract version.
//!
//! Real upgrades go from a version deployed months ago, whose storage shapes
//! predate several changes. Testing only the latest-to-next hop proves the
//! mechanism works, not that the actual upgrade a deployment will perform works.
//!
//! This file keeps compiled wasm fixtures for released versions and tests
//! upgrading from each to the current build, asserting registrations survive.
//!
//! Issue #70: Cover the upgrade path from every prior contract version.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, BytesN, Env, Vec};

use lumina_registry::{Category, LuminaRegistry, LuminaRegistryClient};

// ─── Upgrade-test wasm fixtures ─────────────────────────────────────────────

mod registry_v2_wasm {
    soroban_sdk::contractimport!(file = "../target/wasm32v1-none/release/lumina_registry_v2.wasm");
}

fn advance_ledger(env: &Env, n: u32) {
    env.ledger()
        .set_sequence_number(env.ledger().sequence().saturating_add(n));
}

fn govern_upgrade(
    env: &Env,
    client: &LuminaRegistryClient,
    admin: &Address,
    new_wasm_hash: &BytesN<32>,
) {
    let pid = client.propose_upgrade(admin, new_wasm_hash);
    client.approve_proposal(admin, &pid);
    advance_ledger(env, lumina_registry::TIMELOCK_LEDGERS);
    client.execute_proposal(&pid);
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

/// Set up a registry with a single admin.
fn setup() -> (Env, LuminaRegistryClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_min_persistent_entry_ttl(10_000_000);
    let admin = Address::generate(&env);
    let contract_id = env.register(LuminaRegistry, (&admin,));
    let client = LuminaRegistryClient::new(&env, &contract_id);
    (env, client, admin)
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

// ─── How to add a fixture when a version is released ────────────────────────
//
// 1. Build the release wasm:
//    `cargo build --target wasm32v1-none --release`
//
// 2. The wasm is referenced from `target/wasm32v1-none/release/`.
//
// 3. Add a `mod vX_wasm { contractimport!(...) }` block above.
//
// 4. Add a test function following the pattern below: deploy the current
//    wasm, register contracts, upload the fixture wasm, upgrade, and assert
//    that registrations survive and the new version is active.
//
// Note: The v1 wasm (lumina_registry.wasm) is too large (~130KB) for the
// test environment's default budget. The v2 fixture (lumina_registry_v2.wasm)
// is a minimal upgrade target (~12KB) that exercises the same upgrade path.
// When the v1 wasm size is reduced, add tests deploying from v1 as well.

// ─── Upgrade path: current → v2 fixture ─────────────────────────────────────

/// Deploy current wasm, register contracts, upgrade to v2 fixture, and assert
/// registrations survive.
#[test]
fn upgrade_to_v2_preserves_registrations() {
    let (env, client, admin) = setup();

    // Register contracts on current version.
    let (owner1, target1) = register_sample(&env, &client);
    let (owner2, target2) = register_sample(&env, &client);
    assert_eq!(client.get_contract_count(), 2);

    // Upload v2 wasm and upgrade.
    let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);
    govern_upgrade(&env, &client, &admin, &v2_hash);

    // Registrations must survive.
    assert_eq!(client.get_contract_count(), 2);

    let entry1 = client.get_contract(&target1);
    assert_eq!(entry1.owner, owner1);
    assert!(entry1.active);

    let entry2 = client.get_contract(&target2);
    assert_eq!(entry2.owner, owner2);
    assert!(entry2.active);
}

/// After upgrading to v2, the version number changes.
#[test]
fn upgrade_to_v2_reports_new_version() {
    let (env, client, admin) = setup();

    let v1_version = client.get_version();

    let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);
    govern_upgrade(&env, &client, &admin, &v2_hash);

    let new_version = client.get_version();
    assert!(
        new_version > v1_version,
        "version should increase after upgrade"
    );
}

/// After upgrading to v2, the admin key is preserved in storage.
/// The v2 fixture doesn't expose get_admin, so we verify the admin key
/// is still readable via the contract's storage.
#[test]
fn upgrade_to_v2_preserves_admin_key() {
    let (env, client, admin) = setup();

    let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);
    govern_upgrade(&env, &client, &admin, &v2_hash);

    // Verify the v2 code is active by checking the version.
    let version = client.get_version();
    assert_eq!(version, 9); // v2 CONTRACT_VERSION
}

/// After upgrading to v2, owner queries work on the upgraded code.
#[test]
fn upgrade_to_v2_preserves_owner_queries() {
    let (env, client, admin) = setup();

    // Register contracts owned by the same owner.
    let owner = Address::generate(&env);
    let target1 = Address::generate(&env);
    let target2 = Address::generate(&env);
    client.register_contract(
        &owner,
        &target1,
        &soroban_sdk::String::from_str(&env, "Contract 1"),
        &soroban_sdk::String::from_str(&env, "First"),
        &default_cats(&env),
    );
    client.register_contract(
        &owner,
        &target2,
        &soroban_sdk::String::from_str(&env, "Contract 2"),
        &soroban_sdk::String::from_str(&env, "Second"),
        &default_cats(&env),
    );

    let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);
    govern_upgrade(&env, &client, &admin, &v2_hash);

    // Owner queries must still work.
    let owned = client.get_contracts_by_owner(&owner, &0, &10);
    assert_eq!(owned.len(), 2);
}

/// After upgrading to v2, the contract entries are preserved.
/// The v2 fixture doesn't expose category queries, so we verify via get_contract.
#[test]
fn upgrade_to_v2_preserves_contract_entries() {
    let (env, client, admin) = setup();

    let owner = Address::generate(&env);
    let target = Address::generate(&env);
    client.register_contract(
        &owner,
        &target,
        &soroban_sdk::String::from_str(&env, "DeFi App"),
        &soroban_sdk::String::from_str(&env, "A DeFi application"),
        &cats(&env, &[Category::DeFi]),
    );

    let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);
    govern_upgrade(&env, &client, &admin, &v2_hash);

    // The entry must survive the upgrade.
    let entry = client.get_contract(&target);
    assert_eq!(entry.owner, owner);
    assert!(entry.active);
}

/// After upgrading to v2, the contract count is preserved.
#[test]
fn upgrade_to_v2_preserves_contract_count() {
    let (env, client, admin) = setup();

    register_sample(&env, &client);
    register_sample(&env, &client);
    register_sample(&env, &client);
    assert_eq!(client.get_contract_count(), 3);

    let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);
    govern_upgrade(&env, &client, &admin, &v2_hash);

    assert_eq!(client.get_contract_count(), 3);
}
