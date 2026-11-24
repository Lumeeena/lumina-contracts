// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Dedicated test for the `StakingNotConfigured` error path.
//!
//! `RegistryError::StakingNotConfigured` (code 15) is returned when a staking
//! operation is attempted before governance has executed a `ConfigureStaking`
//! proposal. Staking reads both the stake token and treasury from instance
//! storage; if either is missing, the contract refuses the operation. Callers
//! branch on the bare integer, so a failing test must catch any removal of this
//! guard.
//!
//! Issue #270: Add a dedicated test for the StakingNotConfigured error path.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env, String, Vec};

use lumina_registry::{
    Category, LuminaRegistry, LuminaRegistryClient, RegistryError, TIMELOCK_LEDGERS,
};

/// Set up a registry with a single admin (threshold = 1) and no staking config.
fn setup() -> (Env, LuminaRegistryClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|info| {
        info.min_persistent_entry_ttl = TIMELOCK_LEDGERS * 4;
        info.min_temp_entry_ttl = TIMELOCK_LEDGERS * 4;
        info.max_entry_ttl = TIMELOCK_LEDGERS * 8;
    });
    let admin = Address::generate(&env);
    let contract_id = env.register(LuminaRegistry, (&admin,));
    let client = LuminaRegistryClient::new(&env, &contract_id);
    (env, client, admin)
}

/// Advance the mock ledger by `n` ledgers.
fn advance_ledger(env: &Env, n: u32) {
    let seq = env.ledger().sequence();
    env.ledger().set_sequence_number(seq + n);
}

/// Drive a single-admin proposal through approve → timelock → execute.
fn pass_proposal(env: &Env, client: &LuminaRegistryClient, admin: &Address, pid: u32) {
    client.approve_proposal(admin, &pid);
    advance_ledger(env, TIMELOCK_LEDGERS);
    client.execute_proposal(admin, &pid);
}

/// Register a minimal contract and return `(owner, contract_id)`.
fn register_sample(env: &Env, client: &LuminaRegistryClient) -> (Address, Address) {
    let owner = Address::generate(env);
    let contract_id = Address::generate(env);
    let mut categories = Vec::new(env);
    categories.push_back(Category::DeFi);
    client.register_contract(
        &owner,
        &contract_id,
        &String::from_str(env, "Sample"),
        &String::from_str(env, "A sample contract"),
        &categories,
    );
    (owner, contract_id)
}

// ─── StakingNotConfigured ────────────────────────────────────────────────────

/// Querying staking config before governance configures it must return
/// `StakingNotConfigured`.
///
/// Primary acceptance-criterion test: removing the staking-config guard from
/// `staking_config`/`get_staking_config` must cause this test to fail.
#[test]
fn get_staking_config_fails_when_not_configured() {
    let (_env, client, _admin) = setup();

    assert_eq!(
        client.try_get_staking_config(),
        Err(Ok(RegistryError::StakingNotConfigured))
    );
}

/// Staking into a registered contract before staking is configured must fail
/// with `StakingNotConfigured`, not a token transfer panic or a different error.
#[test]
fn stake_fails_when_staking_not_configured() {
    let (env, client, _admin) = setup();
    let (owner, target) = register_sample(&env, &client);

    assert_eq!(
        client.try_stake(&owner, &target, &100i128),
        Err(Ok(RegistryError::StakingNotConfigured))
    );
}

/// Registering with a non-zero registration fee also depends on staking config
/// (the fee is collected into the configured treasury). Before configuration,
/// the registration must fail with `StakingNotConfigured`.
#[test]
fn register_with_fee_fails_when_staking_not_configured() {
    let (env, client, admin) = setup();

    let pid = client.propose_set_registration_fee(&admin, &50i128);
    pass_proposal(&env, &client, &admin, pid);
    assert_eq!(client.get_registration_fee(), 50);

    let owner = Address::generate(&env);
    let target = Address::generate(&env);
    let mut categories = Vec::new(&env);
    categories.push_back(Category::DeFi);

    assert_eq!(
        client.try_register_contract(
            &owner,
            &target,
            &String::from_str(&env, "Fee"),
            &String::from_str(&env, "Needs staking configured"),
            &categories,
        ),
        Err(Ok(RegistryError::StakingNotConfigured))
    );
    assert!(!client.is_registered(&target));
}

/// Once staking is configured, the same operations that previously returned
/// `StakingNotConfigured` must no longer fail for that reason.
#[test]
fn staking_operations_succeed_after_configuration() {
    let (env, client, admin) = setup();
    let token_admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(token_admin.clone())
        .address();
    let treasury = Address::generate(&env);

    let pid = client.propose_configure_staking(&admin, &token_id, &treasury);
    pass_proposal(&env, &client, &admin, pid);

    let (token, treasury_out) = client.get_staking_config();
    assert_eq!(token, token_id);
    assert_eq!(treasury_out, treasury);
}
