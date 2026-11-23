// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Dedicated contract-boundary regression tests for issues #272–#275.
//! Each assertion checks the exact RegistryError variant, not just failure.

use lumina_registry::{
    Category, LuminaRegistry, LuminaRegistryClient, RegistryError,
    SLASH_LOCK_LEDGERS, TIMELOCK_LEDGERS,
};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Env, String, Vec,
};

fn setup() -> (Env, LuminaRegistryClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|info| {
        info.min_persistent_entry_ttl = TIMELOCK_LEDGERS * 4;
        info.min_temp_entry_ttl = TIMELOCK_LEDGERS * 4;
        info.max_entry_ttl = TIMELOCK_LEDGERS * 8;
    });
    let admin = Address::generate(&env);
    let registry_id = env.register(LuminaRegistry, (&admin,));
    let client = LuminaRegistryClient::new(&env, &registry_id);
    (env, client, admin)
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

fn advance(env: &Env, ledgers: u32) {
    env.ledger().set_sequence_number(env.ledger().sequence() + ledgers);
}

fn execute(env: &Env, client: &LuminaRegistryClient, admin: &Address, proposal: u32) {
    client.approve_proposal(admin, &proposal);
    advance(env, TIMELOCK_LEDGERS);
    client.execute_proposal(admin, &proposal);
}

#[test]
fn insufficient_stake_rejects_withdrawal_when_deactivated_owner_has_no_collateral() {
    let (env, client, _) = setup();
    let (owner, target) = register(&env, &client);
    client.deactivate(&owner, &target);

    // Removing the zero-stake guard must make this exact error assertion fail.
    assert_eq!(
        client.try_withdraw_stake(&owner, &target),
        Err(Ok(RegistryError::InsufficientStake))
    );
    assert_eq!(client.get_stake(&target), 0);
}

#[test]
fn registration_active_prevents_withdrawal_until_deactivated() {
    let (env, client, _) = setup();
    let (owner, target) = register(&env, &client);

    // Active-registration check precedes stake and token configuration checks.
    assert_eq!(
        client.try_withdraw_stake(&owner, &target),
        Err(Ok(RegistryError::RegistrationActive))
    );
    client.deactivate(&owner, &target);
    assert_eq!(
        client.try_withdraw_stake(&owner, &target),
        Err(Ok(RegistryError::InsufficientStake))
    );
}

#[test]
fn no_categories_rejects_empty_registration_without_creating_an_entry() {
    let (env, client, _) = setup();
    let owner = Address::generate(&env);
    let target = Address::generate(&env);

    assert_eq!(
        client.try_register_contract(
            &owner,
            &target,
            &String::from_str(&env, "Uncategorized"),
            &String::from_str(&env, "Should not be registered"),
            &Vec::new(&env),
        ),
        Err(Ok(RegistryError::NoCategories))
    );
    assert!(!client.is_registered(&target));

    // A valid category is accepted, demonstrating that the error is specific.
    client.register_contract(
        &owner,
        &target,
        &String::from_str(&env, "Categorized"),
        &String::from_str(&env, "Valid registration"),
        &Vec::from_slice(&env, &[Category::Other]),
    );
    assert!(client.is_registered(&target));
}

#[test]
fn stake_locked_blocks_post_slash_withdrawal_until_lock_expires() {
    let (env, client, admin) = setup();
    let issuer = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(issuer).address();
    let treasury = Address::generate(&env);
    let configure = client.propose_configure_staking(&admin, &token_id, &treasury);
    execute(&env, &client, &admin, configure);

    let (owner, target) = register(&env, &client);
    token::StellarAssetClient::new(&env, &token_id).mint(&owner, &100);
    client.stake(&owner, &target, &100);

    let reason = String::from_str(&env, "slash-lock regression");
    let slash = client.propose_slash(&admin, &target, &20, &reason);
    execute(&env, &client, &admin, slash);
    client.deactivate(&owner, &target);

    assert_eq!(
        client.try_withdraw_stake(&owner, &target),
        Err(Ok(RegistryError::StakeLocked))
    );
    assert_eq!(client.get_stake(&target), 80);

    advance(&env, SLASH_LOCK_LEDGERS);
    assert_eq!(client.withdraw_stake(&owner, &target), 80);
    assert_eq!(client.get_stake(&target), 0);
}
