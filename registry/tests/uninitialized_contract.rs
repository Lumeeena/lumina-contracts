// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Test behaviour against an uninitialised contract.
//!
//! The contract is deployed via `__constructor` which sets up a bootstrap admin,
//! but some entrypoints handle a missing admin set differently. This test file
//! exercises every entrypoint before `initialize` is called (using direct
//! registration without the constructor) to document and verify the intended
//! behaviour.
//!
//! Issue #72: Test behaviour against an uninitialised contract.

use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Env, Vec};

use lumina_registry::{Category, LuminaRegistry, LuminaRegistryClient, RegistryError};

/// Deploy the contract with the constructor (which sets up a single bootstrap
/// admin) but do NOT call `initialize` (which sets up the full multi-sig admin
/// set). This represents the state after deployment but before governance is
/// fully configured.
fn setup_uninitialized() -> (Env, LuminaRegistryClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
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

// ─── Entrypoints that should work before initialization ─────────────────────

/// `register_contract` works before `initialize` — registration is permissionless.
#[test]
fn register_contract_works_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let owner = Address::generate(&env);
    let target = Address::generate(&env);

    client.register_contract(
        &owner,
        &target,
        &soroban_sdk::String::from_str(&env, "Test"),
        &soroban_sdk::String::from_str(&env, "A test"),
        &default_cats(&env),
    );
    assert!(client.is_registered(&target));
}

/// `get_version` returns the version regardless of initialization state.
#[test]
fn get_version_works_before_initialize() {
    let (_env, client, _admin) = setup_uninitialized();
    let version = client.get_version();
    assert!(version > 0);
}

/// `get_contract` works for a registered contract even without initialization.
#[test]
fn get_contract_works_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let owner = Address::generate(&env);
    let target = Address::generate(&env);

    client.register_contract(
        &owner,
        &target,
        &soroban_sdk::String::from_str(&env, "Test"),
        &soroban_sdk::String::from_str(&env, "A test"),
        &default_cats(&env),
    );

    let entry = client.get_contract(&target);
    assert_eq!(entry.owner, owner);
    assert!(entry.active);
}

/// `get_contract_count` works before initialization.
#[test]
fn get_contract_count_works_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    assert_eq!(client.get_contract_count(), 0);

    let owner = Address::generate(&env);
    let target = Address::generate(&env);
    client.register_contract(
        &owner,
        &target,
        &soroban_sdk::String::from_str(&env, "Test"),
        &soroban_sdk::String::from_str(&env, "A test"),
        &default_cats(&env),
    );
    assert_eq!(client.get_contract_count(), 1);
}

/// `is_registered` works before initialization.
#[test]
fn is_registered_works_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let target = Address::generate(&env);
    assert!(!client.is_registered(&target));
}

/// `get_active_contracts` works before initialization (returns empty list).
#[test]
fn get_active_contracts_works_before_initialize() {
    let (_env, client, _admin) = setup_uninitialized();
    let active = client.get_active_contracts(&0, &10);
    assert_eq!(active.len(), 0);
}

/// `get_contracts_by_owner` works before initialization.
#[test]
fn get_contracts_by_owner_works_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let owner = Address::generate(&env);
    let owned = client.get_contracts_by_owner(&owner, &0, &10);
    assert_eq!(owned.len(), 0);
}

/// `get_categories` works before initialization.
#[test]
fn get_categories_works_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let target = Address::generate(&env);
    let categories = client.get_categories(&target);
    assert_eq!(categories.len(), 0);
}

/// `get_tags` works before initialization.
#[test]
fn get_tags_works_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let target = Address::generate(&env);
    let tags = client.get_tags(&target);
    assert_eq!(tags.len(), 0);
}

/// `get_stake` works before initialization (returns 0).
#[test]
fn get_stake_works_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let target = Address::generate(&env);
    assert_eq!(client.get_stake(&target), 0);
}

/// `is_verified` works before initialization (returns false).
#[test]
fn is_verified_works_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let target = Address::generate(&env);
    assert!(!client.is_verified(&target));
}

/// `get_reputation` works before initialization (returns zeroed values).
#[test]
fn get_reputation_works_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let target = Address::generate(&env);
    let rep = client.get_reputation(&target);
    assert_eq!(rep.stake, 0);
    assert!(!rep.verified);
    assert_eq!(rep.slashed_total, 0);
}

/// `get_attestations` works before initialization (returns empty list).
#[test]
fn get_attestations_works_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let target = Address::generate(&env);
    let attestations = client.get_attestations(&target);
    assert_eq!(attestations.len(), 0);
}

/// `get_slashes` works before initialization (returns empty list).
#[test]
fn get_slashes_works_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let target = Address::generate(&env);
    let slashes = client.get_slashes(&target);
    assert_eq!(slashes.len(), 0);
}

/// `get_registration_fee` works before initialization (returns 0).
#[test]
fn get_registration_fee_works_before_initialize() {
    let (_env, client, _admin) = setup_uninitialized();
    assert_eq!(client.get_registration_fee(), 0);
}

/// `get_minimum_stake` works before initialization (returns 0).
#[test]
fn get_minimum_stake_works_before_initialize() {
    let (_env, client, _admin) = setup_uninitialized();
    assert_eq!(client.get_minimum_stake(), 0);
}

/// `prune_category` works before initialization (returns 0).
#[test]
fn prune_category_works_before_initialize() {
    let (_env, client, _admin) = setup_uninitialized();
    let removed = client.prune_category(&Category::DeFi);
    assert_eq!(removed, 0);
}

/// `prune_all_contracts` works before initialization (returns 0).
#[test]
fn prune_all_contracts_works_before_initialize() {
    let (_env, client, _admin) = setup_uninitialized();
    let removed = client.prune_all_contracts();
    assert_eq!(removed, 0);
}

// ─── Entrypoints that should fail before initialization ─────────────────────

/// `get_admin` returns the bootstrap admin set by the constructor.
/// Even without calling `initialize`, the constructor sets up a single admin.
#[test]
fn get_admin_returns_bootstrap_admin_before_initialize() {
    let (_env, client, admin) = setup_uninitialized();
    let stored = client.get_admin();
    assert_eq!(stored, admin);
}

/// `get_admins` returns the single bootstrap admin.
#[test]
fn get_admins_returns_bootstrap_admin_before_initialize() {
    let (_env, client, admin) = setup_uninitialized();
    let admins = client.get_admins();
    assert_eq!(admins.len(), 1);
    assert!(admins.contains(&admin));
}

/// `get_threshold` returns 1 (set by the constructor).
#[test]
fn get_threshold_returns_one_before_initialize() {
    let (_env, client, _admin) = setup_uninitialized();
    assert_eq!(client.get_threshold(), 1);
}

/// Non-admin cannot create governance proposals before `initialize`.
#[test]
fn non_admin_cannot_propose_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let stranger = Address::generate(&env);
    let target = Address::generate(&env);
    assert_eq!(
        client.try_propose_deactivate(&stranger, &target),
        Err(Ok(RegistryError::NotAdmin))
    );
}

/// The bootstrap admin CAN create governance proposals before `initialize`.
/// The constructor sets up a single admin with threshold=1.
#[test]
fn bootstrap_admin_can_propose_before_initialize() {
    let (env, client, admin) = setup_uninitialized();
    let owner = Address::generate(&env);
    let target = Address::generate(&env);

    client.register_contract(
        &owner,
        &target,
        &soroban_sdk::String::from_str(&env, "Test"),
        &soroban_sdk::String::from_str(&env, "A test"),
        &default_cats(&env),
    );

    let pid = client.propose_deactivate(&admin, &target);
    let proposal = client.get_proposal(&pid);
    assert!(!proposal.executed);
}

/// `get_staking_config` fails with `StakingNotConfigured` before staking is set up.
#[test]
fn get_staking_config_fails_before_staking_configured() {
    let (_env, client, _admin) = setup_uninitialized();
    assert_eq!(
        client.try_get_staking_config(),
        Err(Ok(RegistryError::StakingNotConfigured))
    );
}

/// `deactivate` fails with `ContractNotFound` for an unregistered contract.
#[test]
fn deactivate_unregistered_fails_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let owner = Address::generate(&env);
    let target = Address::generate(&env);
    assert_eq!(
        client.try_deactivate(&owner, &target),
        Err(Ok(RegistryError::ContractNotFound))
    );
}

/// `deregister` fails with `ContractNotFound` for an unregistered contract.
#[test]
fn deregister_unregistered_fails_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let owner = Address::generate(&env);
    let target = Address::generate(&env);
    assert_eq!(
        client.try_deregister(&owner, &target),
        Err(Ok(RegistryError::ContractNotFound))
    );
}

/// `transfer_ownership` fails with `ContractNotFound` for an unregistered contract.
#[test]
fn transfer_ownership_fails_for_unregistered_contract() {
    let (env, client, _admin) = setup_uninitialized();
    let caller = Address::generate(&env);
    let target = Address::generate(&env);
    let new_owner = Address::generate(&env);
    assert_eq!(
        client.try_transfer_ownership(&caller, &target, &new_owner),
        Err(Ok(RegistryError::ContractNotFound))
    );
}

/// `update_metadata` fails with `ContractNotFound` for an unregistered contract.
#[test]
fn update_metadata_unregistered_fails_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let owner = Address::generate(&env);
    let target = Address::generate(&env);
    assert_eq!(
        client.try_update_metadata(
            &owner,
            &target,
            &soroban_sdk::String::from_str(&env, "New"),
            &soroban_sdk::String::from_str(&env, "New desc"),
        ),
        Err(Ok(RegistryError::ContractNotFound))
    );
}

/// `set_categories` fails with `ContractNotFound` for an unregistered contract.
#[test]
fn set_categories_unregistered_fails_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let owner = Address::generate(&env);
    let target = Address::generate(&env);
    assert_eq!(
        client.try_set_categories(&owner, &target, &cats(&env, &[Category::DeFi])),
        Err(Ok(RegistryError::ContractNotFound))
    );
}

/// `set_tags` fails with `ContractNotFound` for an unregistered contract.
#[test]
fn set_tags_unregistered_fails_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let owner = Address::generate(&env);
    let target = Address::generate(&env);
    let tags: Vec<soroban_sdk::String> = Vec::new(&env);
    assert_eq!(
        client.try_set_tags(&owner, &target, &tags),
        Err(Ok(RegistryError::ContractNotFound))
    );
}

/// `attest` fails with `ContractNotFound` for an unregistered contract.
#[test]
fn attest_unregistered_fails_before_initialize() {
    let (env, client, _admin) = setup_uninitialized();
    let attester = Address::generate(&env);
    let target = Address::generate(&env);
    assert_eq!(
        client.try_attest(
            &attester,
            &target,
            &soroban_sdk::String::from_str(&env, "audited"),
        ),
        Err(Ok(RegistryError::ContractNotFound))
    );
}

/// `stake` fails with `ContractNotFound` for an unregistered contract
/// (contract existence is checked before staking config).
#[test]
fn stake_fails_for_unregistered_contract() {
    let (env, client, _admin) = setup_uninitialized();
    let owner = Address::generate(&env);
    let target = Address::generate(&env);
    assert_eq!(
        client.try_stake(&owner, &target, &100),
        Err(Ok(RegistryError::ContractNotFound))
    );
}
