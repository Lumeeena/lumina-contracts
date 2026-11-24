// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Executing a proposal after the admin set or threshold has changed.
//!
//! A proposal is approved by a set of admins and executed later, after the
//! timelock. The two are not atomic, so the state the proposal was approved
//! against can differ from the state it executes against. Issue #232 asks for
//! that behaviour to be decided, documented and asserted rather than left as
//! an accident of the implementation.
//!
//! ## Intended behaviour
//!
//! Execution is checked against the threshold in force **at execution time**,
//! and against the approvals recorded on the proposal — approvals are not
//! re-validated against the current admin set.
//!
//! * Approvals are acts: each was cast by an address that was an admin when
//!   casting it (`approve_proposal` checks that), and removing an admin does
//!   not erase the act. If removals invalidated past approvals, a shrinking
//!   admin set could permanently strand a proposal that had legitimately
//!   reached quorum, even though the remaining admins still want it.
//! * The current admin set still controls the outcome. Execution requires the
//!   number of recorded approvals to meet the *current* threshold, and the
//!   current admins can raise that threshold through governance. So a stale
//!   quorum cannot override a current majority that has acted to stop it.
//! * Raising the threshold above an already-recorded quorum blocks execution
//!   until enough current admins add their approvals. Because readiness is
//!   recorded when the threshold is first met and is not reset, the proposal
//!   becomes executable as soon as the larger quorum arrives, without a second
//!   timelock — the original timelock, which is the notice period that
//!   matters, has already run.
//!
//! Both sequences below assert exactly that.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env, String};

use lumina_registry::{
    Category, LuminaRegistry, LuminaRegistryClient, RegistryError, MIN_ADMINS, TIMELOCK_LEDGERS,
};

/// Set up a registry with `count` admins at threshold 1 (the constructor's
/// default), then raise the threshold to 2.
fn setup_with_admins(count: usize) -> (Env, LuminaRegistryClient<'static>, Vec<Address>) {
    assert!(count >= MIN_ADMINS as usize);

    let env = Env::default();
    env.mock_all_auths();
    // The constructor's persistent keys are written with the minimum TTL, and
    // this test advances several timelocks before registering anything. Keep
    // those entries alive for the whole sequence.
    env.ledger().with_mut(|info| {
        info.min_persistent_entry_ttl = TIMELOCK_LEDGERS * 16;
        info.min_temp_entry_ttl = TIMELOCK_LEDGERS * 16;
        info.max_entry_ttl = TIMELOCK_LEDGERS * 32;
    });

    let bootstrap = Address::generate(&env);
    let contract_id = env.register(LuminaRegistry, (&bootstrap,));
    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .extend_ttl(TIMELOCK_LEDGERS * 24, TIMELOCK_LEDGERS * 32);
    });
    let client = LuminaRegistryClient::new(&env, &contract_id);

    let mut admins = std::vec::Vec::new();
    admins.push(bootstrap.clone());
    while admins.len() < count {
        let added = Address::generate(&env);
        let pid = client.propose_add_admin(&bootstrap, &added);
        approve_and_wait(&env, &client, pid);
        execute(&env, &client, pid);
        assert_admin_invariants(&client);
        admins.push(added);
    }

    // Threshold 2 (the minimum meaningful multi-sig) for every scenario here.
    let pid = client.propose_change_threshold(&bootstrap, &2u32);
    client.approve_proposal(&bootstrap, &pid);
    advance_ledger(&env, TIMELOCK_LEDGERS);
    execute(&env, &client, pid);
    assert_admin_invariants(&client);

    (env, client, admins)
}

fn advance_ledger(env: &Env, n: u32) {
    let seq = env.ledger().sequence();
    env.ledger().set_sequence_number(seq + n);
}

fn approve_and_wait(env: &Env, client: &LuminaRegistryClient, pid: u32) {
    for admin in client.get_admins().iter() {
        client.approve_proposal(&admin, &pid);
    }
    advance_ledger(env, TIMELOCK_LEDGERS);
}

fn execute(_env: &Env, client: &LuminaRegistryClient, pid: u32) {
    let executor = client.get_admins().get(0).unwrap();
    client.execute_proposal(&executor, &pid);
}

/// The governance invariants from #229/#230, re-asserted after every operation
/// in these sequences: admins non-empty and threshold satisfiable.
fn assert_admin_invariants(client: &LuminaRegistryClient) {
    let admins = client
        .try_get_admins()
        .expect("admin set view must always be reachable")
        .expect("admin set must always be readable (and therefore non-empty)");
    assert!(!admins.is_empty(), "admin set must never be empty");
    let threshold = client
        .try_get_threshold()
        .expect("threshold view must always be reachable")
        .expect("threshold must always be readable");
    assert!(
        threshold <= admins.len(),
        "threshold {threshold} exceeds admin count {}",
        admins.len(),
    );
}

/// Register a contract with the given owner so governance has something to act
/// on, and return its address.
fn register_target(env: &Env, client: &LuminaRegistryClient, owner: &Address) -> Address {
    let target = Address::generate(env);
    let mut categories = soroban_sdk::Vec::new(env);
    categories.push_back(Category::Infrastructure);
    client.register_contract(
        owner,
        &target,
        &String::from_str(env, "Governance Target"),
        &String::from_str(env, "A registration governed by the admin set"),
        &categories,
    );
    target
}

// ─── Approvals survive the removal of the admins who cast them ──────────────

#[test]
fn proposal_executes_after_two_of_its_approvers_are_removed() {
    let (env, client, pool) = setup_with_admins(4);
    assert_admin_invariants(&client);
    let a1 = pool[0].clone();
    let a2 = pool[1].clone();
    let a3 = pool[2].clone();
    let a4 = pool[3].clone();

    let owner = Address::generate(&env);
    let target = register_target(&env, &client, &owner);

    // A proposal approved by three admins when the threshold is two.
    let proposal = client.propose_deactivate(&a1, &target);
    client.approve_proposal(&a1, &proposal);
    client.approve_proposal(&a2, &proposal);
    client.approve_proposal(&a3, &proposal);
    assert!(client.get_proposal(&proposal).approvals.len() >= client.get_threshold());
    advance_ledger(&env, TIMELOCK_LEDGERS);
    assert_admin_invariants(&client);

    // Two of its approvers are removed. Each removal keeps the threshold
    // satisfiable (4 -> 3 -> 2 admins at threshold 2).
    for removed in [&a2, &a3] {
        let removal = client.propose_remove_admin(&a1, removed);
        client.approve_proposal(&a1, &removal);
        client.approve_proposal(&a4, &removal);
        advance_ledger(&env, TIMELOCK_LEDGERS);
        execute(&env, &client, removal);
        assert_admin_invariants(&client);
    }

    let remaining = client.get_admins();
    assert_eq!(remaining.len(), 2);
    assert!(remaining.contains(&a1));
    assert!(remaining.contains(&a4));

    // Intended: the recorded approvals outlive the admins who cast them, so
    // the proposal still executes. `execute_proposal` compares the approval
    // count with the current threshold (2), which is unchanged and met.
    execute(&env, &client, proposal);

    assert!(client.get_proposal(&proposal).executed);
    assert!(!client.get_contract(&target).active);
    assert_admin_invariants(&client);
}

// ─── A raised threshold gates an already-ready proposal ─────────────────────

#[test]
fn raising_the_threshold_requires_more_approvals_before_execution() {
    let (env, client, pool) = setup_with_admins(3);
    assert_admin_invariants(&client);
    let a1 = pool[0].clone();
    let a2 = pool[1].clone();
    let a3 = pool[2].clone();
    assert_eq!(client.get_threshold(), 2);

    let owner = Address::generate(&env);
    let target = register_target(&env, &client, &owner);

    // Reach the threshold of 2 and let the timelock elapse.
    let proposal = client.propose_deactivate(&a1, &target);
    client.approve_proposal(&a1, &proposal);
    client.approve_proposal(&a2, &proposal);
    advance_ledger(&env, TIMELOCK_LEDGERS);
    assert_admin_invariants(&client);

    // Raise the threshold to 3 while the proposal waits.
    let raise = client.propose_change_threshold(&a1, &3u32);
    client.approve_proposal(&a1, &raise);
    client.approve_proposal(&a2, &raise);
    advance_ledger(&env, TIMELOCK_LEDGERS);
    execute(&env, &client, raise);
    assert_eq!(client.get_threshold(), 3);
    assert_admin_invariants(&client);

    // Intended: execution is evaluated against the threshold now in force, so
    // the two recorded approvals are no longer a quorum. The proposal is not
    // executed (and, because the whole invocation rolls back, not marked).
    let executor = client.get_admins().get(0).unwrap();
    assert_eq!(
        client.try_execute_proposal(&executor, &proposal),
        Err(Ok(RegistryError::ThresholdNotMet)),
    );
    assert!(!client.get_proposal(&proposal).executed);
    assert!(client.get_contract(&target).active);
    assert_admin_invariants(&client);

    // Intended: the readiness timestamp is not reset by the threshold change.
    // Once the third current admin approves, the larger quorum is met and the
    // proposal executes without a second timelock — the original notice
    // period has already elapsed.
    client.approve_proposal(&a3, &proposal);
    client.execute_proposal(&executor, &proposal);

    assert!(client.get_proposal(&proposal).executed);
    assert!(!client.get_contract(&target).active);
    assert_admin_invariants(&client);
}
