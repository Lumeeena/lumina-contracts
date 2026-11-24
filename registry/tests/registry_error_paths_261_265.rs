// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Dedicated tests for the `NotOwner`, `ProposalNotFound`, `ThresholdNotMet`
//! and `TimelockNotElapsed` error paths (issues #261, #263, #264, #265).
//!
//! Error variants cross the contract boundary as bare integers, so each test
//! asserts the exact `RegistryError` variant, not just that the call failed.
//! Removing the check that raises a variant makes its test fail.

use lumina_registry::{
    Category, LuminaRegistry, LuminaRegistryClient, RegistryError, TIMELOCK_LEDGERS,
};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env, String, Vec,
};

/// A registry with one admin (threshold = 1).
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

fn advance(env: &Env, ledgers: u32) {
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + ledgers);
}

/// Approve, wait out the timelock and execute a proposal as the sole admin.
fn pass(env: &Env, client: &LuminaRegistryClient, admin: &Address, proposal: u32) {
    client.approve_proposal(admin, &proposal);
    advance(env, TIMELOCK_LEDGERS);
    client.execute_proposal(admin, &proposal);
}

/// Register a contract and return its owner and address.
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

// ─── NotOwner (#261) ─────────────────────────────────────────────────────────

/// Only the recorded owner may deregister a contract. A stranger gets
/// `NotOwner`, and the check runs before the "still active" check, so the
/// result is `NotOwner` rather than `RegistrationActive`.
#[test]
fn deregister_by_a_non_owner_returns_not_owner() {
    let (env, client, _admin) = setup();
    let (_owner, target) = register(&env, &client);
    let stranger = Address::generate(&env);

    assert_eq!(
        client.try_deregister(&stranger, &target),
        Err(Ok(RegistryError::NotOwner))
    );
    // The registration is untouched.
    assert!(client.get_contract(&target).active);
}

/// The same guard protects metadata edits.
#[test]
fn update_metadata_by_a_non_owner_returns_not_owner() {
    let (env, client, _admin) = setup();
    let (_owner, target) = register(&env, &client);
    let stranger = Address::generate(&env);

    assert_eq!(
        client.try_update_metadata(
            &stranger,
            &target,
            &String::from_str(&env, "Hijacked"),
            &String::from_str(&env, "Hijacked"),
        ),
        Err(Ok(RegistryError::NotOwner))
    );
    assert_eq!(
        client.get_contract(&target).name,
        String::from_str(&env, "Example")
    );
}

// ─── ProposalNotFound (#263) ─────────────────────────────────────────────────

/// Approving a proposal id that was never created returns `ProposalNotFound`.
#[test]
fn approving_an_unknown_proposal_returns_proposal_not_found() {
    let (_env, client, admin) = setup();

    assert_eq!(
        client.try_approve_proposal(&admin, &42),
        Err(Ok(RegistryError::ProposalNotFound))
    );
}

/// Executing an unknown proposal id returns the same error, and the id right
/// after the last created proposal is still unknown.
#[test]
fn executing_an_unknown_proposal_returns_proposal_not_found() {
    let (env, client, admin) = setup();
    let new_admin = Address::generate(&env);
    let pid = client.propose_add_admin(&admin, &new_admin);

    assert_eq!(
        client.try_execute_proposal(&admin, &(pid + 1)),
        Err(Ok(RegistryError::ProposalNotFound))
    );
    assert_eq!(
        client.try_execute_proposal(&admin, &9_999),
        Err(Ok(RegistryError::ProposalNotFound))
    );
}

// ─── ThresholdNotMet (#264) ──────────────────────────────────────────────────

/// With a threshold of two, one approval is not enough even after the
/// timelock has passed.
#[test]
fn execute_below_threshold_returns_threshold_not_met() {
    let (env, client, a1) = setup();
    let a2 = Address::generate(&env);

    // Grow the admin set to two and raise the threshold to two.
    let add_a2 = client.propose_add_admin(&a1, &a2);
    pass(&env, &client, &a1, add_a2);
    let raise = client.propose_change_threshold(&a1, &2);
    client.approve_proposal(&a1, &raise);
    client.approve_proposal(&a2, &raise);
    advance(&env, TIMELOCK_LEDGERS);
    client.execute_proposal(&a1, &raise);

    // One approval out of two, then wait out the timelock.
    let a3 = Address::generate(&env);
    let pid = client.propose_add_admin(&a1, &a3);
    client.approve_proposal(&a1, &pid);
    advance(&env, TIMELOCK_LEDGERS + 1);

    assert_eq!(
        client.try_execute_proposal(&a1, &pid),
        Err(Ok(RegistryError::ThresholdNotMet))
    );
    assert!(!client.get_proposal(&pid).executed);

    // The second approval is what unlocks it.
    client.approve_proposal(&a2, &pid);
    advance(&env, TIMELOCK_LEDGERS);
    client.execute_proposal(&a1, &pid);
    assert!(client.get_proposal(&pid).executed);
}

/// A proposal nobody has approved yet also returns `ThresholdNotMet`, even
/// with the default threshold of one.
#[test]
fn execute_with_no_approvals_returns_threshold_not_met() {
    let (env, client, admin) = setup();
    let new_admin = Address::generate(&env);
    let pid = client.propose_add_admin(&admin, &new_admin);
    advance(&env, TIMELOCK_LEDGERS + 1);

    assert_eq!(
        client.try_execute_proposal(&admin, &pid),
        Err(Ok(RegistryError::ThresholdNotMet))
    );
}

// ─── TimelockNotElapsed (#265) ───────────────────────────────────────────────

/// Once the proposal has its approvals, execution is still refused until
/// `TIMELOCK_LEDGERS` ledgers have passed: one ledger short fails, the exact
/// boundary succeeds.
#[test]
fn execute_before_the_timelock_returns_timelock_not_elapsed() {
    let (env, client, admin) = setup();
    let new_admin = Address::generate(&env);
    let pid = client.propose_add_admin(&admin, &new_admin);
    client.approve_proposal(&admin, &pid);

    // Straight after approval.
    assert_eq!(
        client.try_execute_proposal(&admin, &pid),
        Err(Ok(RegistryError::TimelockNotElapsed))
    );

    // One ledger short of the timelock.
    advance(&env, TIMELOCK_LEDGERS - 1);
    assert_eq!(
        client.try_execute_proposal(&admin, &pid),
        Err(Ok(RegistryError::TimelockNotElapsed))
    );
    assert!(!client.get_proposal(&pid).executed);

    // Exactly at the timelock it runs.
    advance(&env, 1);
    client.execute_proposal(&admin, &pid);
    assert!(client.get_proposal(&pid).executed);
}
