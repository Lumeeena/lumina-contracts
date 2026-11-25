// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Pause interaction with the staking endpoints.
//!
//! The global pause flag is for a worst-case stop: governance flips it to
//! block every new write and give operators time to investigate. Two things
//! are deliberately kept open in that state:
//!
//! - reads, so downstream indexers do not go blind;
//! - `withdraw_stake`, so stakers already inside the registry are not
//!   trapped there.
//!
//! The pause / stake interaction was previously only exercised in the
//! register-contract path. This file pins the staking side of the same
//! contract — adding `stake` to the paused writers, leaving `withdraw_stake`
//! out of the gate.
//!
//! Issue #234: Test staking behaviour while the contract is paused.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{token, Address, Env, String, Vec};

use lumina_registry::{
    Category, LuminaRegistry, LuminaRegistryClient, RegistryError, TIMELOCK_LEDGERS,
};

fn setup() -> (Env, LuminaRegistryClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|info| {
        info.min_persistent_entry_ttl = TIMELOCK_LEDGERS * 4;
        info.min_temp_entry_ttl = TIMELOCK_LEDGERS * 4;
        info.max_entry_ttl = TIMELOCK_LEDGERS * 16;
    });
    let admin = Address::generate(&env);
    let contract_id = env.register(LuminaRegistry, (&admin,));
    let client = LuminaRegistryClient::new(&env, &contract_id);
    (env, client, admin)
}

fn advance_ledger(env: &Env, n: u32) {
    let seq = env.ledger().sequence();
    env.ledger().set_sequence_number(seq + n);
}

fn pass_proposal(env: &Env, client: &LuminaRegistryClient, admin: &Address, pid: u32) {
    client.approve_proposal(admin, &pid);
    advance_ledger(env, TIMELOCK_LEDGERS);
    client.execute_proposal(admin, &pid);
}

fn open_staking(
    env: &Env,
    client: &LuminaRegistryClient,
    admin: &Address,
) -> (Address, Address) {
    let issuer = Address::generate(env);
    let token_id = env.register_stellar_asset_contract_v2(issuer).address();
    let treasury = Address::generate(env);
    let pid = client.propose_configure_staking(admin, &token_id, &treasury);
    pass_proposal(env, client, admin, pid);
    (token_id, treasury)
}

fn register(env: &Env, client: &LuminaRegistryClient) -> (Address, Address) {
    let owner = Address::generate(env);
    let target = Address::generate(env);
    let mut cats = Vec::new(env);
    cats.push_back(Category::Infrastructure);
    client.register_contract(
        &owner,
        &target,
        &String::from_str(env, "Test Contract"),
        &String::from_str(env, "A test contract"),
        &cats,
    );
    (owner, target)
}

/// Walk a `SetPaused(paused)` proposal through to execution.
fn set_paused(env: &Env, client: &LuminaRegistryClient, admin: &Address, paused: bool) {
    let pid = client.propose_set_paused(admin, &paused);
    pass_proposal(env, client, admin, pid);
}

// ─── Staking refused while paused ────────────────────────────────────────────

/// `stake` must return `Paused` while the global pause flag is set.
#[test]
fn stake_is_refused_while_paused() {
    let (env, client, admin) = setup();
    let (token_id, _treasury) = open_staking(&env, &client, &admin);
    let (owner, target) = register(&env, &client);
    token::StellarAssetClient::new(&env, &token_id).mint(&owner, &1_000);

    set_paused(&env, &client, &admin, true);

    assert_eq!(
        client.try_stake(&owner, &target, &500),
        Err(Ok(RegistryError::Paused)),
    );
    assert_eq!(client.get_stake(&target), 0);
    // No tokens moved either.
    assert_eq!(token::Client::new(&env, &token_id).balance(&owner), 1_000);
}

// ─── Withdrawal keeps working while paused ───────────────────────────────────

/// A staker inside the registry when pause is flipped must still be able to
/// deactivate and withdraw. This is the "not trapped" guarantee.
#[test]
fn withdraw_still_succeeds_while_paused() {
    let (env, client, admin) = setup();
    let (token_id, _treasury) = open_staking(&env, &client, &admin);
    let (owner, target) = register(&env, &client);
    token::StellarAssetClient::new(&env, &token_id).mint(&owner, &1_000);

    // Stake before the pause.
    client.stake(&owner, &target, &750);
    assert_eq!(client.get_stake(&target), 750);

    set_paused(&env, &client, &admin, true);

    // Owner deactivates and withdraws under the pause. Both succeed.
    client.deactivate(&owner, &target);
    assert_eq!(client.withdraw_stake(&owner, &target), 750);
    assert_eq!(client.get_stake(&target), 0);
    assert_eq!(token::Client::new(&env, &token_id).balance(&owner), 1_000);
}

// ─── Unpausing restores staking ──────────────────────────────────────────────

/// After an unpause proposal executes, new stakes go through normally.
#[test]
fn stake_is_restored_after_unpause() {
    let (env, client, admin) = setup();
    let (token_id, _treasury) = open_staking(&env, &client, &admin);
    let (owner, target) = register(&env, &client);
    token::StellarAssetClient::new(&env, &token_id).mint(&owner, &1_000);

    set_paused(&env, &client, &admin, true);
    assert_eq!(
        client.try_stake(&owner, &target, &500),
        Err(Ok(RegistryError::Paused)),
    );

    set_paused(&env, &client, &admin, false);

    client.stake(&owner, &target, &500);
    assert_eq!(client.get_stake(&target), 500);
    assert_eq!(token::Client::new(&env, &token_id).balance(&owner), 500);
}

// ─── Governance can still move while paused ──────────────────────────────────

/// The pause does not block `execute_proposal`. If it did, there would be
/// no governance path to unpause and the pause would be permanent.
#[test]
fn execute_proposal_is_not_blocked_by_pause() {
    let (env, client, admin) = setup();
    let (_token_id, _treasury) = open_staking(&env, &client, &admin);

    set_paused(&env, &client, &admin, true);

    // An unpause proposal still passes through approve → timelock → execute
    // while the pause is in effect.
    let pid = client.propose_set_paused(&admin, &false);
    client.approve_proposal(&admin, &pid);
    advance_ledger(&env, TIMELOCK_LEDGERS);
    // execute_proposal itself must not be gated by the pause.
    client.execute_proposal(&admin, &pid);

    // And the pause is now lifted.
    let (owner, target) = register(&env, &client);
    token::StellarAssetClient::new(&env, &token_id_from_config(&client))
        .mint(&owner, &100);
    client.stake(&owner, &target, &100);
    assert_eq!(client.get_stake(&target), 100);
}

/// Helper: read the configured staking token out of the registry.
fn token_id_from_config(client: &LuminaRegistryClient) -> Address {
    let (token_id, _) = client.get_staking_config();
    token_id
}
