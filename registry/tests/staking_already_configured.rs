// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Dedicated test for the `StakingAlreadyConfigured` error path.
//!
//! `RegistryError::StakingAlreadyConfigured` (code 35) is returned by
//! `propose_configure_staking` when the registry is already configured with
//! the exact same token and treasury that the proposal would set.  A caller
//! that branches on error code 35 must be able to rely on this check being
//! present and stable.
//!
//! Issue #290: Add a dedicated test for the StakingAlreadyConfigured error path.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env};

use lumina_registry::{LuminaRegistry, LuminaRegistryClient, RegistryError, TIMELOCK_LEDGERS};

/// Set up a registry with a single admin.
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

/// Drive a proposal through approve → timelock → execute.
fn pass_proposal(env: &Env, client: &LuminaRegistryClient, admin: &Address, pid: u32) {
    client.approve_proposal(admin, &pid);
    advance_ledger(env, TIMELOCK_LEDGERS);
    client.execute_proposal(admin, &pid);
}

// ─── StakingAlreadyConfigured ────────────────────────────────────────────────

/// Proposing `configure_staking` with the identical token and treasury that is
/// already active must return `StakingAlreadyConfigured`.
///
/// This is the primary acceptance-criterion test: removing the
/// `StakingAlreadyConfigured` check from `propose_configure_staking` must
/// cause this test to fail.
#[test]
fn propose_configure_staking_rejects_identical_config() {
    let (env, client, admin) = setup();

    // Register a real SEP-41 token — `execute_proposal` calls `decimals()` on
    // it, so a plain generated address is not sufficient here.
    let issuer = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(issuer).address();
    let treasury = Address::generate(&env);

    // First proposal: configure staking — must succeed.
    let pid = client.propose_configure_staking(&admin, &token_id, &treasury);
    pass_proposal(&env, &client, &admin, pid);

    // Second proposal with the same (token, treasury) pair must be rejected
    // before a proposal is even stored.
    assert_eq!(
        client.try_propose_configure_staking(&admin, &token_id, &treasury),
        Err(Ok(RegistryError::StakingAlreadyConfigured))
    );
}

/// Proposing `configure_staking` with a *different* treasury must succeed even
/// when staking is already configured, confirming the guard is specific to the
/// exact (token, treasury) pair rather than a blanket "already configured" block.
#[test]
fn propose_configure_staking_accepts_different_treasury() {
    let (env, client, admin) = setup();

    let issuer = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(issuer).address();
    let treasury_a = Address::generate(&env);
    let treasury_b = Address::generate(&env);

    let pid = client.propose_configure_staking(&admin, &token_id, &treasury_a);
    pass_proposal(&env, &client, &admin, pid);

    // Different treasury — must not produce StakingAlreadyConfigured.
    let result = client.try_propose_configure_staking(&admin, &token_id, &treasury_b);
    assert_ne!(
        result,
        Err(Ok(RegistryError::StakingAlreadyConfigured)),
        "a different treasury should not trigger StakingAlreadyConfigured"
    );
}

/// Proposing `configure_staking` with a *different* token must succeed even
/// when staking is already configured.
#[test]
fn propose_configure_staking_accepts_different_token() {
    let (env, client, admin) = setup();

    let issuer_a = Address::generate(&env);
    let token_a = env
        .register_stellar_asset_contract_v2(issuer_a)
        .address();
    let issuer_b = Address::generate(&env);
    let token_b = env
        .register_stellar_asset_contract_v2(issuer_b)
        .address();
    let treasury = Address::generate(&env);

    let pid = client.propose_configure_staking(&admin, &token_a, &treasury);
    pass_proposal(&env, &client, &admin, pid);

    // Different token — must not produce StakingAlreadyConfigured.
    let result = client.try_propose_configure_staking(&admin, &token_b, &treasury);
    assert_ne!(
        result,
        Err(Ok(RegistryError::StakingAlreadyConfigured)),
        "a different token should not trigger StakingAlreadyConfigured"
    );
}
