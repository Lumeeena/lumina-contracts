// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Dedicated test for the `ThresholdAlreadySet` error path.
//!
//! `RegistryError::ThresholdAlreadySet` (code 33) is returned by
//! `propose_change_threshold` when the requested value equals the threshold
//! that is already active.  A caller that branches on error code 33 must be
//! able to rely on this check being present and stable.
//!
//! Issue #288: Add a dedicated test for the ThresholdAlreadySet error path.

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

// ─── ThresholdAlreadySet ─────────────────────────────────────────────────────

/// Proposing `change_threshold` to the value that is already the current
/// threshold must return `ThresholdAlreadySet`.
///
/// This is the primary acceptance-criterion test: removing the
/// `ThresholdAlreadySet` check from `propose_change_threshold` must cause this
/// test to fail.
#[test]
fn propose_change_threshold_rejects_current_value() {
    let (_env, client, admin) = setup();

    // After construction the threshold is 1.
    let current = client.get_threshold();
    assert_eq!(current, 1);

    assert_eq!(
        client.try_propose_change_threshold(&admin, &current),
        Err(Ok(RegistryError::ThresholdAlreadySet))
    );
}

/// After governance raises the threshold the guard still fires for the new
/// value, confirming the check reads the live threshold rather than a
/// compile-time constant.
#[test]
fn propose_change_threshold_rejects_current_value_after_a_change() {
    let (env, client, admin) = setup();

    // Add a second admin so the set has two members, then raise the threshold
    // to 2 so the guard has a different value to enforce.
    let second_admin = Address::generate(&env);
    let add_pid = client.propose_add_admin(&admin, &second_admin);
    pass_proposal(&env, &client, &admin, add_pid);

    let change_pid = client.propose_change_threshold(&admin, &2);
    pass_proposal(&env, &client, &admin, change_pid);

    assert_eq!(client.get_threshold(), 2);

    // Proposing 2 again (now the live value) must be rejected.
    assert_eq!(
        client.try_propose_change_threshold(&admin, &2),
        Err(Ok(RegistryError::ThresholdAlreadySet))
    );
}

/// Proposing a *different* threshold must not produce `ThresholdAlreadySet`,
/// confirming the guard is not a blanket block on the entrypoint.
#[test]
fn propose_change_threshold_accepts_new_value() {
    let (env, client, admin) = setup();

    // Add a second admin so threshold 2 is a valid target.
    let second_admin = Address::generate(&env);
    let add_pid = client.propose_add_admin(&admin, &second_admin);
    pass_proposal(&env, &client, &admin, add_pid);

    // Proposing threshold 2 (different from the current 1) must not be
    // rejected with ThresholdAlreadySet.
    let result = client.try_propose_change_threshold(&admin, &2);
    assert_ne!(
        result,
        Err(Ok(RegistryError::ThresholdAlreadySet)),
        "a different threshold value should not trigger ThresholdAlreadySet"
    );
}
