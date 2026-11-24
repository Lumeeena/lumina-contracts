// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Dedicated contract-boundary regression tests for issues #256, #257, #258
//! and #260. Each assertion checks the exact `RegistryError` variant, not just
//! that the call failed, because callers branch on the numeric code.
//!
//! - #256 `AlreadyInitialized`
//! - #257 `Unauthorized`
//! - #258 `AlreadyRegistered`
//! - #260 `InvalidMetadata`

use lumina_registry::{
    Category, LuminaRegistry, LuminaRegistryClient, RegistryError, MAX_DESCRIPTION_LEN,
    MAX_NAME_LEN,
};
use soroban_sdk::{testutils::Address as _, Address, Env, String, Vec};

fn setup() -> (Env, LuminaRegistryClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let registry_id = env.register(LuminaRegistry, (&admin,));
    let client = LuminaRegistryClient::new(&env, &registry_id);
    (env, client, admin)
}

fn categories(env: &Env) -> Vec<Category> {
    Vec::from_slice(env, &[Category::Infrastructure])
}

fn register(env: &Env, client: &LuminaRegistryClient) -> (Address, Address) {
    let owner = Address::generate(env);
    let target = Address::generate(env);
    client.register_contract(
        &owner,
        &target,
        &String::from_str(env, "Example"),
        &String::from_str(env, "Error path regression"),
        &categories(env),
    );
    (owner, target)
}

// ─── #256 AlreadyInitialized ───────────────────────────────────────────────

#[test]
fn already_initialized_when_initialize_is_called_after_the_constructor() {
    let (env, client, admin) = setup();
    let mut admins = Vec::new(&env);
    admins.push_back(admin);

    // Removing the `Admins` existence check must make this fail.
    assert_eq!(
        client.try_initialize(&admins, &1),
        Err(Ok(RegistryError::AlreadyInitialized))
    );
}

#[test]
fn already_initialized_blocks_a_second_party_from_taking_over() {
    let (env, client, admin) = setup();
    let attacker = Address::generate(&env);
    let mut admins = Vec::new(&env);
    admins.push_back(attacker.clone());

    assert_eq!(
        client.try_initialize(&admins, &1),
        Err(Ok(RegistryError::AlreadyInitialized))
    );
    // The original admin set is untouched.
    assert_eq!(client.get_admin(), admin);
    assert!(!client.get_admins().contains(&attacker));
}

// ─── #257 Unauthorized ─────────────────────────────────────────────────────

#[test]
fn unauthorized_when_a_non_owner_deactivates_a_contract() {
    let (env, client, _admin) = setup();
    let (_owner, target) = register(&env, &client);
    let stranger = Address::generate(&env);

    // Removing the owner check in `deactivate` must make this fail.
    assert_eq!(
        client.try_deactivate(&stranger, &target),
        Err(Ok(RegistryError::Unauthorized))
    );
    assert!(client.get_contract(&target).active);
}

#[test]
fn unauthorized_applies_to_the_admin_too_without_a_governance_proposal() {
    let (env, client, admin) = setup();
    let (_owner, target) = register(&env, &client);

    // Admins deactivating someone else's contract must go through governance.
    assert_eq!(
        client.try_deactivate(&admin, &target),
        Err(Ok(RegistryError::Unauthorized))
    );
    assert!(client.get_contract(&target).active);
}

#[test]
fn the_owner_is_not_unauthorized() {
    let (env, client, _admin) = setup();
    let (owner, target) = register(&env, &client);
    client.deactivate(&owner, &target);
    assert!(!client.get_contract(&target).active);
}

// ─── #258 AlreadyRegistered ────────────────────────────────────────────────

#[test]
fn already_registered_when_the_same_contract_is_registered_twice() {
    let (env, client, _admin) = setup();
    let (owner, target) = register(&env, &client);

    // Removing the existing-entry check must make this fail.
    assert_eq!(
        client.try_register_contract(
            &owner,
            &target,
            &String::from_str(&env, "Second"),
            &String::from_str(&env, "Second attempt"),
            &categories(&env),
        ),
        Err(Ok(RegistryError::AlreadyRegistered))
    );
}

#[test]
fn already_registered_applies_to_a_different_owner_and_keeps_the_first_entry() {
    let (env, client, _admin) = setup();
    let (owner, target) = register(&env, &client);
    let rival = Address::generate(&env);

    assert_eq!(
        client.try_register_contract(
            &rival,
            &target,
            &String::from_str(&env, "Hijack"),
            &String::from_str(&env, "Not the owner"),
            &categories(&env),
        ),
        Err(Ok(RegistryError::AlreadyRegistered))
    );
    assert_eq!(client.get_contract(&target).owner, owner);
}

#[test]
fn already_registered_is_reported_before_invalid_metadata() {
    let (env, client, _admin) = setup();
    let (owner, target) = register(&env, &client);

    assert_eq!(
        client.try_register_contract(
            &owner,
            &target,
            &String::from_str(&env, ""),
            &String::from_str(&env, ""),
            &categories(&env),
        ),
        Err(Ok(RegistryError::AlreadyRegistered))
    );
}

// ─── #260 InvalidMetadata ──────────────────────────────────────────────────

/// The error a fresh registration with this metadata returns, or `None` when it succeeds.
fn register_error(
    env: &Env,
    client: &LuminaRegistryClient,
    name: &str,
    description: &str,
) -> Option<RegistryError> {
    client
        .try_register_contract(
            &Address::generate(env),
            &Address::generate(env),
            &String::from_str(env, name),
            &String::from_str(env, description),
            &categories(env),
        )
        .err()
        .and_then(|e| e.ok())
}

#[test]
fn invalid_metadata_for_an_empty_name() {
    let (env, client, _admin) = setup();
    assert_eq!(
        register_error(&env, &client, "", "A description"),
        Some(RegistryError::InvalidMetadata)
    );
}

#[test]
fn invalid_metadata_for_a_whitespace_only_name() {
    let (env, client, _admin) = setup();
    assert_eq!(
        register_error(&env, &client, "   ", "A description"),
        Some(RegistryError::InvalidMetadata)
    );
}

#[test]
fn invalid_metadata_for_a_name_longer_than_the_limit() {
    let (env, client, _admin) = setup();
    let too_long = "n".repeat(MAX_NAME_LEN as usize + 1);
    assert_eq!(
        register_error(&env, &client, &too_long, "A description"),
        Some(RegistryError::InvalidMetadata)
    );
}

#[test]
fn invalid_metadata_for_a_description_longer_than_the_limit() {
    let (env, client, _admin) = setup();
    let too_long = "d".repeat(MAX_DESCRIPTION_LEN as usize + 1);
    assert_eq!(
        register_error(&env, &client, "Name", &too_long),
        Some(RegistryError::InvalidMetadata)
    );
}

#[test]
fn metadata_exactly_at_the_limits_is_accepted() {
    let (env, client, _admin) = setup();
    let name = "n".repeat(MAX_NAME_LEN as usize);
    let description = "d".repeat(MAX_DESCRIPTION_LEN as usize);
    assert_eq!(register_error(&env, &client, &name, &description), None);
}
