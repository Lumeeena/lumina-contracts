// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Dedicated test for the `InvalidThreshold` error path.
//!
//! `RegistryError::InvalidThreshold` (code 13) is returned when an admin set
//! and its approval threshold cannot coexist: zero, more than the number of
//! admins, or an admin removal that would leave the threshold unsatisfiable.
//! It is raised by `propose_change_threshold` at proposal time and by
//! `apply_action` when a `ChangeThreshold` or `RemoveAdmin` proposal executes.
//! Callers branch on the bare integer, so every raise site needs a failing
//! test if its check is removed.
//!
//! Issue #268: Add a dedicated test for the InvalidThreshold error path.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env};

use lumina_registry::{LuminaRegistry, LuminaRegistryClient, RegistryError, TIMELOCK_LEDGERS};

/// Set up a registry with a single admin (threshold = 1).
fn setup() -> (Env, LuminaRegistryClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|info| {
        info.min_persistent_entry_ttl = TIMELOCK_LEDGERS * 4;
        info.min_temp_entry_ttl = TIMELOCK_LEDGERS * 4;
        info.max_entry_ttl = TIMELOCK_LEDGERS * 32;
    });
    let admin = Address::generate(&env);
    let contract_id = env.register(LuminaRegistry, (&admin,));

    // Several proposals are approved in sequence, so keep the contract
    // instance loadable well past the accumulated timelock advances.
    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .extend_ttl(TIMELOCK_LEDGERS * 16, TIMELOCK_LEDGERS * 32);
    });

    let client = LuminaRegistryClient::new(&env, &contract_id);
    (env, client, admin)
}

/// Advance the mock ledger by `n` ledgers.
fn advance_ledger(env: &Env, n: u32) {
    let seq = env.ledger().sequence();
    env.ledger().set_sequence_number(seq + n);
}

/// Approve a proposal and wait out the timelock without executing it.
fn approve_and_wait(env: &Env, client: &LuminaRegistryClient, admin: &Address, pid: u32) {
    client.approve_proposal(admin, &pid);
    advance_ledger(env, TIMELOCK_LEDGERS);
}

/// Drive a single-admin proposal through approve → timelock → execute.
fn pass_proposal(env: &Env, client: &LuminaRegistryClient, admin: &Address, pid: u32) {
    approve_and_wait(env, client, admin, pid);
    client.execute_proposal(admin, &pid);
}

// ─── Proposal-time validation ────────────────────────────────────────────────

/// A zero threshold can never be satisfied, so `propose_change_threshold`
/// must reject it before creating a proposal.
#[test]
fn propose_change_threshold_rejects_zero() {
    let (_env, client, admin) = setup();

    assert_eq!(
        client.try_propose_change_threshold(&admin, &0u32),
        Err(Ok(RegistryError::InvalidThreshold))
    );
}

/// A threshold above the current admin count can never be satisfied either.
/// With the constructor's single admin, 2 is already out of range.
#[test]
fn propose_change_threshold_rejects_value_above_admin_count() {
    let (_env, client, admin) = setup();
    assert_eq!(client.get_admins().len(), 1);

    assert_eq!(
        client.try_propose_change_threshold(&admin, &2u32),
        Err(Ok(RegistryError::InvalidThreshold))
    );
}

// ─── Execution-time validation: ChangeThreshold ─────────────────────────────

/// A threshold proposal can become invalid while it waits out the timelock if
/// the admin set shrinks. Executing it must then fail with `InvalidThreshold`.
#[test]
fn executing_threshold_change_rejects_value_above_current_admin_count() {
    let (env, client, admin) = setup();
    let second = Address::generate(&env);
    let third = Address::generate(&env);

    pass_proposal(
        &env,
        &client,
        &admin,
        client.propose_add_admin(&admin, &second),
    );
    pass_proposal(
        &env,
        &client,
        &admin,
        client.propose_add_admin(&admin, &third),
    );
    assert_eq!(client.get_admins().len(), 3);

    // Valid when proposed: 3 admins support a threshold of 3.
    let change_pid = client.propose_change_threshold(&admin, &3u32);
    approve_and_wait(&env, &client, &admin, change_pid);

    // Shrink the set to 2 while the change proposal is still pending.
    pass_proposal(
        &env,
        &client,
        &admin,
        client.propose_remove_admin(&admin, &third),
    );
    assert_eq!(client.get_admins().len(), 2);

    assert_eq!(
        client.try_execute_proposal(&admin, &change_pid),
        Err(Ok(RegistryError::InvalidThreshold))
    );
    assert!(!client.get_proposal(&change_pid).executed);
    assert_eq!(client.get_threshold(), 1);
}

// ─── Execution-time validation: RemoveAdmin ─────────────────────────────────

/// Removing an admin that would leave fewer admins than the active threshold
/// is rejected at execution time, rolling the whole proposal back.
#[test]
fn executing_admin_removal_rejects_threshold_violation() {
    let (env, client, admin) = setup();
    let second = Address::generate(&env);
    let third = Address::generate(&env);

    pass_proposal(
        &env,
        &client,
        &admin,
        client.propose_add_admin(&admin, &second),
    );
    pass_proposal(
        &env,
        &client,
        &admin,
        client.propose_add_admin(&admin, &third),
    );
    pass_proposal(
        &env,
        &client,
        &admin,
        client.propose_change_threshold(&admin, &2u32),
    );
    assert_eq!(client.get_threshold(), 2);

    // 3 admins, threshold 2: removing one still satisfies the threshold.
    let first_remove = client.propose_remove_admin(&admin, &third);
    client.approve_proposal(&admin, &first_remove);
    client.approve_proposal(&second, &first_remove);
    advance_ledger(&env, TIMELOCK_LEDGERS);
    client.execute_proposal(&admin, &first_remove);
    assert_eq!(client.get_admins().len(), 2);

    // Now removing another would leave 1 < threshold 2.
    let second_remove = client.propose_remove_admin(&admin, &second);
    client.approve_proposal(&admin, &second_remove);
    client.approve_proposal(&second, &second_remove);
    advance_ledger(&env, TIMELOCK_LEDGERS);

    assert_eq!(
        client.try_execute_proposal(&admin, &second_remove),
        Err(Ok(RegistryError::InvalidThreshold))
    );
    assert!(!client.get_proposal(&second_remove).executed);
    assert_eq!(client.get_admins().len(), 2);
    assert_eq!(client.get_threshold(), 2);
}
