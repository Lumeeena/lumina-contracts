// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Dedicated contract-boundary regression tests for issues #259, #262, #266
//! and #267. Each assertion checks the exact `RegistryError` variant, not just
//! that the call failed, because callers branch on the numeric code.
//!
//! - #259 `ContractNotFound` (4)
//! - #262 `NotInitialized` (7)
//! - #266 `AlreadyApproved` (11)
//! - #267 `NotAdmin` (12)

use lumina_registry::{Category, DataKey, LuminaRegistry, LuminaRegistryClient, RegistryError};
use soroban_sdk::{testutils::Address as _, Address, Env, String, Vec};

fn setup() -> (Env, LuminaRegistryClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let registry_id = env.register(LuminaRegistry, (&admin,));
    let client = LuminaRegistryClient::new(&env, &registry_id);
    (env, client, admin, registry_id)
}

fn register(env: &Env, client: &LuminaRegistryClient) -> (Address, Address) {
    let owner = Address::generate(env);
    let target = Address::generate(env);
    client.register_contract(
        &owner,
        &target,
        &String::from_str(env, "Example"),
        &String::from_str(env, "Error path regression"),
        &Vec::from_slice(env, &[Category::Infrastructure]),
    );
    (owner, target)
}

// ─── #259 ContractNotFound ─────────────────────────────────────────────────

#[test]
fn contract_not_found_when_acting_on_an_unregistered_contract() {
    let (env, client, _admin, _id) = setup();
    let owner = Address::generate(&env);
    let unregistered = Address::generate(&env);
    assert!(!client.is_registered(&unregistered));

    // Removing the existence check in `deactivate` must make this fail.
    assert_eq!(
        client.try_deactivate(&owner, &unregistered),
        Err(Ok(RegistryError::ContractNotFound))
    );
}

#[test]
fn contract_not_found_is_specific_to_the_missing_entry() {
    let (env, client, _admin, _id) = setup();
    let (owner, registered) = register(&env, &client);
    let unregistered = Address::generate(&env);

    // A registered contract does not hit the error, an unregistered one does.
    client.deactivate(&owner, &registered);
    assert_eq!(
        client.try_deactivate(&owner, &unregistered),
        Err(Ok(RegistryError::ContractNotFound))
    );
}

// ─── #262 NotInitialized ───────────────────────────────────────────────────

/// `__constructor` always installs a bootstrap admin, so the only way to reach
/// a registry with no admin set is to remove those keys from instance storage.
/// This models a deployment whose governance state is missing; it is not a
/// path an ordinary caller can create.
fn wipe_governance_state(env: &Env, registry_id: &Address) {
    env.as_contract(registry_id, || {
        let storage = env.storage().instance();
        storage.remove(&DataKey::Admins);
        storage.remove(&DataKey::Admin);
        storage.remove(&DataKey::Threshold);
    });
}

#[test]
fn not_initialized_when_the_admin_set_is_missing() {
    let (env, client, _admin, id) = setup();
    wipe_governance_state(&env, &id);

    // Removing the `NotInitialized` branches must make these fail.
    assert_eq!(client.try_get_admins(), Err(Ok(RegistryError::NotInitialized)));
    assert_eq!(client.try_get_admin(), Err(Ok(RegistryError::NotInitialized)));
    assert_eq!(client.try_get_threshold(), Err(Ok(RegistryError::NotInitialized)));
}

#[test]
fn not_initialized_takes_precedence_over_not_admin_for_governance_calls() {
    let (env, client, admin, id) = setup();
    let (_owner, target) = register(&env, &client);
    wipe_governance_state(&env, &id);

    // With no admin set at all, even the former admin gets NotInitialized,
    // not NotAdmin.
    assert_eq!(
        client.try_propose_deactivate(&admin, &target),
        Err(Ok(RegistryError::NotInitialized))
    );
}

#[test]
fn an_initialised_registry_does_not_report_not_initialized() {
    let (_env, client, admin, _id) = setup();
    assert_eq!(client.get_admin(), admin);
    assert_eq!(client.get_threshold(), 1);
    assert_eq!(client.get_admins().len(), 1);
}

// ─── #266 AlreadyApproved ──────────────────────────────────────────────────

#[test]
fn already_approved_when_the_same_admin_approves_twice() {
    let (env, client, admin, _id) = setup();
    let (_owner, target) = register(&env, &client);

    let proposal = client.propose_deactivate(&admin, &target);
    client.approve_proposal(&admin, &proposal);

    // Removing the duplicate-approval check must make this fail.
    assert_eq!(
        client.try_approve_proposal(&admin, &proposal),
        Err(Ok(RegistryError::AlreadyApproved))
    );
}

#[test]
fn already_approved_applies_per_proposal() {
    let (env, client, admin, _id) = setup();
    let (_owner, first) = register(&env, &client);
    let (_owner2, second) = register(&env, &client);

    let p1 = client.propose_deactivate(&admin, &first);
    let p2 = client.propose_deactivate(&admin, &second);
    client.approve_proposal(&admin, &p1);

    // The same admin may still approve a different proposal once.
    client.approve_proposal(&admin, &p2);
    assert_eq!(
        client.try_approve_proposal(&admin, &p1),
        Err(Ok(RegistryError::AlreadyApproved))
    );
    assert_eq!(
        client.try_approve_proposal(&admin, &p2),
        Err(Ok(RegistryError::AlreadyApproved))
    );
}

// ─── #267 NotAdmin ─────────────────────────────────────────────────────────

#[test]
fn not_admin_when_a_stranger_proposes() {
    let (env, client, _admin, _id) = setup();
    let (_owner, target) = register(&env, &client);
    let stranger = Address::generate(&env);

    // Removing the admin-set membership check must make this fail.
    assert_eq!(
        client.try_propose_deactivate(&stranger, &target),
        Err(Ok(RegistryError::NotAdmin))
    );
}

#[test]
fn not_admin_when_a_stranger_approves() {
    let (env, client, admin, _id) = setup();
    let (_owner, target) = register(&env, &client);
    let proposal = client.propose_deactivate(&admin, &target);
    let stranger = Address::generate(&env);

    assert_eq!(
        client.try_approve_proposal(&stranger, &proposal),
        Err(Ok(RegistryError::NotAdmin))
    );
    // The real admin is still allowed to approve it.
    client.approve_proposal(&admin, &proposal);
}
