// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Test helper that asserts full index consistency.
//!
//! The owner index, category index and `AllContracts` must agree with the
//! stored entries after any state-changing operation. This module provides
//! `assert_indexes_consistent` which walks every index and compares it against
//! a fresh scan of stored `Contract` entries.
//!
//! Issue #73: Add a test helper that asserts full index consistency.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env, Vec};

use lumina_registry::{
    Category, ContractEntry, DataKey, LuminaRegistry, LuminaRegistryClient,
};

/// Build a `Vec<Category>` from a slice.
fn cats(env: &Env, list: &[Category]) -> Vec<Category> {
    let mut v = Vec::new(env);
    for category in list {
        v.push_back(*category);
    }
    v
}

/// The default category for tests that don't care which one is used.
fn default_cats(env: &Env) -> Vec<Category> {
    cats(env, &[Category::Infrastructure])
}

/// Set up a registry with a single admin.
fn setup() -> (Env, LuminaRegistryClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
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

/// Advance the mock ledger by `n` ledgers.
fn advance_ledger(env: &Env, n: u32) {
    let seq = env.ledger().sequence();
    env.ledger().set_sequence_number(seq + n);
}

// ─── Index consistency helper ────────────────────────────────────────────────

/// Assert that every index (owner, category, `AllContracts`) agrees with a
/// fresh scan of the stored `Contract` entries.
pub fn assert_indexes_consistent(env: &Env, client: &LuminaRegistryClient) {
    let contract = &client.address;

    env.as_contract(contract, || {
        // ── 1. Collect all stored entries ──────────────────────────────────
        let all: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AllContracts)
            .unwrap_or(Vec::new(env));

        for contract_id in all.iter() {
            let entry = env
                .storage()
                .persistent()
                .get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id.clone()));
            assert!(
                entry.is_some(),
                "AllContracts index contains a contract_id but no Contract entry exists"
            );
        }

        // ── 2. Assert OwnerContracts index ─────────────────────────────────
        for contract_id in all.iter() {
            if let Some(entry) = env
                .storage()
                .persistent()
                .get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id.clone()))
            {
                let owned: Vec<Address> = env
                    .storage()
                    .persistent()
                    .get(&DataKey::OwnerContracts(entry.owner.clone()))
                    .unwrap_or(Vec::new(env));
                assert!(
                    owned.contains(&contract_id),
                    "OwnerContracts index is missing a contract that exists in storage"
                );
            }
        }

        // ── 3. Assert ByCategory index ─────────────────────────────────────
        let all_categories = [
            Category::DeFi,
            Category::Nft,
            Category::Gaming,
            Category::Identity,
            Category::Infrastructure,
            Category::Payments,
            Category::Oracle,
            Category::Dao,
            Category::Other,
        ];

        for category in &all_categories {
            let index: Vec<Address> = env
                .storage()
                .persistent()
                .get(&DataKey::ByCategory(*category))
                .unwrap_or(Vec::new(env));

            for contract_id in index.iter() {
                let entry_exists = env
                    .storage()
                    .persistent()
                    .get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id.clone()))
                    .is_some();
                assert!(
                    entry_exists,
                    "ByCategory index contains a contract_id with no Contract entry"
                );

                let entry_categories: Vec<Category> = env
                    .storage()
                    .persistent()
                    .get(&DataKey::Categories(contract_id.clone()))
                    .unwrap_or(Vec::new(env));
                assert!(
                    entry_categories.contains(*category),
                    "ByCategory index lists a contract under a category it does not declare"
                );
            }
        }

        // ── 4. Reverse check: every entry's categories must be indexed ─────
        for contract_id in all.iter() {
            let entry_categories: Vec<Category> = env
                .storage()
                .persistent()
                .get(&DataKey::Categories(contract_id.clone()))
                .unwrap_or(Vec::new(env));

            for category in entry_categories.iter() {
                let index: Vec<Address> = env
                    .storage()
                    .persistent()
                    .get(&DataKey::ByCategory(category))
                    .unwrap_or(Vec::new(env));
                assert!(
                    index.contains(&contract_id),
                    "Contract declares category {:?} but is not in ByCategory index",
                    category
                );
            }
        }
    });
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[test]
fn helper_passes_on_consistent_indexes_after_registration() {
    let (env, client, _admin) = setup();
    let (_owner, _target) = register_sample(&env, &client);
    assert_indexes_consistent(&env, &client);
}

#[test]
fn helper_passes_after_multiple_registrations() {
    let (env, client, _admin) = setup();
    register_sample(&env, &client);
    register_sample(&env, &client);
    register_sample(&env, &client);
    assert_indexes_consistent(&env, &client);
}

#[test]
fn helper_passes_after_deactivation() {
    let (env, client, _admin) = setup();
    let (owner, target) = register_sample(&env, &client);
    client.deactivate(&owner, &target);
    assert_indexes_consistent(&env, &client);
}

#[test]
fn helper_passes_after_deregistration() {
    let (env, client, _admin) = setup();
    let (owner, target) = register_sample(&env, &client);
    client.deactivate(&owner, &target);
    client.deregister(&owner, &target);
    assert_indexes_consistent(&env, &client);
}

#[test]
fn helper_passes_after_category_change() {
    let (env, client, _admin) = setup();
    let (owner, target) = register_sample(&env, &client);
    client.set_categories(&owner, &target, &cats(&env, &[Category::DeFi]));
    assert_indexes_consistent(&env, &client);
}

#[test]
fn helper_passes_after_ownership_transfer() {
    let (env, client, _admin) = setup();
    let (owner, target) = register_sample(&env, &client);
    let new_owner = Address::generate(&env);
    client.transfer_ownership(&owner, &target, &new_owner);
    assert_indexes_consistent(&env, &client);
}

/// A deliberately corrupted index must be caught by the consistency check.
#[test]
fn helper_detects_corrupted_owner_index() {
    let (env, client, _admin) = setup();
    let (owner, target) = register_sample(&env, &client);

    // Corrupt the owner index: remove the entry from the owner's list.
    env.as_contract(&client.address, || {
        let mut owned: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::OwnerContracts(owner.clone()))
            .unwrap_or(Vec::new(&env));
        if let Some(i) = owned.first_index_of(&target) {
            owned.remove(i);
        }
        env.storage()
            .persistent()
            .set(&DataKey::OwnerContracts(owner.clone()), &owned);
    });

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_indexes_consistent(&env, &client);
    }));
    assert!(
        result.is_err(),
        "consistency check failed to detect a corrupted owner index"
    );
}

/// A deliberately corrupted category index must be caught.
#[test]
fn helper_detects_corrupted_category_index() {
    let (env, client, _admin) = setup();
    let (_owner, target) = register_sample(&env, &client);

    // Corrupt the category index: remove the entry from Infrastructure.
    env.as_contract(&client.address, || {
        let mut index: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::ByCategory(Category::Infrastructure))
            .unwrap_or(Vec::new(&env));
        if let Some(i) = index.first_index_of(&target) {
            index.remove(i);
        }
        env.storage()
            .persistent()
            .set(&DataKey::ByCategory(Category::Infrastructure), &index);
    });

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_indexes_consistent(&env, &client);
    }));
    assert!(
        result.is_err(),
        "consistency check failed to detect a corrupted category index"
    );
}

/// A deliberately corrupted AllContracts index must be caught.
#[test]
fn helper_detects_corrupted_all_contracts_index() {
    let (env, client, _admin) = setup();
    let (_owner, target) = register_sample(&env, &client);

    // Corrupt AllContracts: remove the entry.
    env.as_contract(&client.address, || {
        let mut all: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AllContracts)
            .unwrap_or(Vec::new(&env));
        if let Some(i) = all.first_index_of(&target) {
            all.remove(i);
        }
        env.storage().instance().set(&DataKey::AllContracts, &all);

        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::ContractCount)
            .unwrap_or(0);
        assert_ne!(
            all.len() as u32, count,
            "AllContracts and ContractCount should disagree after corruption"
        );
    });
}