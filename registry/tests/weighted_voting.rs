// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Weighted admin voting (#247).
//!
//! Approval is evaluated by **summed weight** against the threshold, not by
//! admin count. Admins whose weight has never been set vote with the default
//! weight of 1, so pre-weighting deployments migrate to the weighted model
//! without a storage write. These tests cover:
//!
//!   - the migration invariant: default weight is 1 and `total_admin_weight`
//!     equals the admin count for a fresh contract.
//!   - approval: a heavily-weighted admin alone can clear a threshold that
//!     two equally-weighted admins would not.
//!   - execution: the weighted check also fires in `execute_proposal`.
//!   - admin removal: cannot leave the remaining summed weight below threshold.
//!   - invalid input: weight = 0 is rejected.
//!
//! See `docs/weighted_governance.md` for the user-facing semantics.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env};

use lumina_registry::{
    LuminaRegistry, LuminaRegistryClient, ProposalAction, RegistryError, TIMELOCK_LEDGERS,
};

fn setup_two_admins() -> (Env, LuminaRegistryClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|info| {
        info.min_persistent_entry_ttl = TIMELOCK_LEDGERS * 4;
        info.min_temp_entry_ttl = TIMELOCK_LEDGERS * 4;
        info.max_entry_ttl = TIMELOCK_LEDGERS * 8;
    });
    let admin_a = Address::generate(&env);
    let admin_b = Address::generate(&env);
    // Bootstrap with admin_a, then add admin_b under threshold=1.
    let contract_id = env.register(LuminaRegistry, (&admin_a,));
    let client = LuminaRegistryClient::new(&env, &contract_id);
    let pid = client.propose_add_admin(&admin_a, &admin_b);
    client.approve_proposal(&admin_a, &pid);
    let seq = env.ledger().sequence();
    env.ledger().set_sequence_number(seq + TIMELOCK_LEDGERS);
    client.execute_proposal(&admin_a, &pid);
    (env, client, admin_a, admin_b)
}

fn advance_timelock(env: &Env) {
    let seq = env.ledger().sequence();
    env.ledger().set_sequence_number(seq + TIMELOCK_LEDGERS);
}

#[test]
fn default_weight_is_one_and_total_equals_admin_count() {
    let (_env, client, admin_a, admin_b) = setup_two_admins();
    assert_eq!(client.get_admin_weight(&admin_a), 1);
    assert_eq!(client.get_admin_weight(&admin_b), 1);
    assert_eq!(client.get_total_admin_weight(), 2);
}

#[test]
fn weighted_admin_alone_can_clear_a_raised_threshold() {
    let (env, client, admin_a, admin_b) = setup_two_admins();

    // Raise admin_a's weight to 3; the second vote-weight is now 3+1 = 4.
    let pid = client.propose_set_admin_weight(&admin_a, &admin_a, &3u32);
    client.approve_proposal(&admin_a, &pid);
    advance_timelock(&env);
    client.execute_proposal(&admin_a, &pid);

    // Raise the threshold to 3.
    let pid = client.propose_change_threshold(&admin_a, &3u32);
    client.approve_proposal(&admin_a, &pid);
    advance_timelock(&env);
    client.execute_proposal(&admin_a, &pid);

    // admin_b (weight 1) approving alone must not be enough.
    let new_admin = Address::generate(&env);
    let pid = client.propose_add_admin(&admin_b, &new_admin);
    client.approve_proposal(&admin_b, &pid);
    advance_timelock(&env);
    assert_eq!(
        client.try_execute_proposal(&admin_b, &pid),
        Err(Ok(RegistryError::ThresholdNotMet))
    );

    // admin_a (weight 3) approving alone IS enough.
    client.approve_proposal(&admin_a, &pid);
    advance_timelock(&env);
    client.execute_proposal(&admin_a, &pid);
    assert!(client.get_proposal(&pid).executed);
}

#[test]
fn zero_weight_is_rejected() {
    let (_env, client, admin_a, _admin_b) = setup_two_admins();
    assert_eq!(
        client.try_propose_set_admin_weight(&admin_a, &admin_a, &0u32),
        Err(Ok(RegistryError::InvalidAdminWeight))
    );
}

#[test]
fn set_admin_weight_rejects_non_admin() {
    let (env, client, admin_a, _admin_b) = setup_two_admins();
    let stranger = Address::generate(&env);
    assert_eq!(
        client.try_propose_set_admin_weight(&admin_a, &stranger, &2u32),
        Err(Ok(RegistryError::AdminNotFound))
    );
}

#[test]
fn removing_heavy_admin_must_leave_enough_weight_for_threshold() {
    let (env, client, admin_a, admin_b) = setup_two_admins();

    // Add a third admin so MIN_ADMINS is still satisfied after removal.
    let admin_c = Address::generate(&env);
    let pid = client.propose_add_admin(&admin_a, &admin_c);
    client.approve_proposal(&admin_a, &pid);
    advance_timelock(&env);
    client.execute_proposal(&admin_a, &pid);

    // Weight admin_a=5, admin_b=1, admin_c=1; total=7. Raise threshold to 6.
    let pid = client.propose_set_admin_weight(&admin_a, &admin_a, &5u32);
    client.approve_proposal(&admin_a, &pid);
    advance_timelock(&env);
    client.execute_proposal(&admin_a, &pid);
    let pid = client.propose_change_threshold(&admin_a, &6u32);
    client.approve_proposal(&admin_a, &pid);
    advance_timelock(&env);
    client.execute_proposal(&admin_a, &pid);

    // Removing admin_a would leave weight = 2 < 6; the apply step rejects it.
    let pid = client.propose_remove_admin(&admin_a, &admin_a);
    client.approve_proposal(&admin_a, &pid);
    advance_timelock(&env);
    assert_eq!(
        client.try_execute_proposal(&admin_a, &pid),
        Err(Ok(RegistryError::InvalidThreshold))
    );

    // Removing a weight-1 admin is fine: weight still 7-1 = 6 >= 6.
    let pid = client.propose_remove_admin(&admin_a, &admin_b);
    client.approve_proposal(&admin_a, &pid);
    advance_timelock(&env);
    client.execute_proposal(&admin_a, &pid);
    assert!(client.get_proposal(&pid).executed);
}

#[test]
fn change_threshold_cannot_exceed_total_weight() {
    let (env, client, admin_a, _admin_b) = setup_two_admins();
    // Total weight is 2, so 3 must be rejected.
    let pid = client.propose_change_threshold(&admin_a, &3u32);
    client.approve_proposal(&admin_a, &pid);
    advance_timelock(&env);
    assert_eq!(
        client.try_execute_proposal(&admin_a, &pid),
        Err(Ok(RegistryError::InvalidThreshold))
    );
}

#[test]
fn single_weighted_admin_can_also_approve_set_admin_weight_itself() {
    // A weight bump that reaches threshold on one call still fires the ready
    // transition; this is really a guard against the approve_proposal rework
    // regressing to counting approvals by length.
    let (env, client, admin_a, _admin_b) = setup_two_admins();
    let pid = client.propose_set_admin_weight(&admin_a, &admin_a, &2u32);
    client.approve_proposal(&admin_a, &pid);
    let before = client.get_proposal(&pid);
    assert_ne!(before.ready_at, u32::MAX);
    advance_timelock(&env);
    client.execute_proposal(&admin_a, &pid);
    assert_eq!(client.get_admin_weight(&admin_a), 2);
}
