// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Invariants over the admin set and the approval threshold.
//!
//! Two properties make governance able to act at all:
//!
//! 1. the admin set is never empty (issue #229), and
//! 2. the approval threshold never exceeds the number of admins (issue #230).
//!
//! Today each is enforced by a separate guard: `RemoveAdmin` refuses a removal
//! that would leave fewer admins than the threshold, and `ChangeThreshold`
//! refuses a threshold above the current admin count. Two independent guards
//! are exactly how an invariant eventually gets violated, because a third path
//! added later will not know to check it. These tests therefore do not test the
//! guards directly: they assert the invariants themselves after every
//! governance operation, including a randomised sequence of mixed operations.
//!
//! The assertion is deliberately on the observed state (`get_admins` /
//! `get_threshold`), not on the error a guard happens to return, so it stays
//! valid if the guards are refactored into a single shared check.
//!
//! Acceptance: removing either guard makes a test below fail. The deterministic
//! sequences assert the specific error in addition to the invariant, so
//! removing a guard fails even when another guard would still refuse the
//! operation.

use proptest::proptest;
use proptest::strategy::Strategy;
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env};

use lumina_registry::{
    LuminaRegistry, LuminaRegistryClient, RegistryError, MIN_ADMINS, TIMELOCK_LEDGERS,
};

/// Number of addresses the randomised sequence may add or remove.
const ADMIN_POOL: usize = 5;

/// Minimum entry TTLs used by the rest of the governance tests, so a sequence
/// of timelocks never archives the instance entry mid-test.
fn setup_with_admins(count: usize) -> (Env, LuminaRegistryClient<'static>, Vec<Address>) {
    assert!(count >= 1);

    let env = Env::default();
    env.mock_all_auths();
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
        assert_governance_invariants(&client);
        admins.push(added);
    }

    (env, client, admins)
}

fn advance_ledger(env: &Env, n: u32) {
    let seq = env.ledger().sequence();
    env.ledger().set_sequence_number(seq + n);
}

/// Approve a proposal with every current admin, then wait out the timelock.
fn approve_and_wait(env: &Env, client: &LuminaRegistryClient, pid: u32) {
    for admin in client.get_admins().iter() {
        client.approve_proposal(&admin, &pid);
    }
    advance_ledger(env, TIMELOCK_LEDGERS);
}

/// Execute a proposal that is expected to succeed.
fn execute(_env: &Env, client: &LuminaRegistryClient, pid: u32) {
    let executor = client.get_admins().get(0).unwrap();
    client.execute_proposal(&executor, &pid);
}

/// Execute a proposal that is expected to be refused, asserting the exact
/// error path that refused it.
fn assert_execution_fails_with(
    _env: &Env,
    client: &LuminaRegistryClient,
    pid: u32,
    expected: RegistryError,
) {
    let executor = client.get_admins().get(0).unwrap();
    assert_eq!(
        client.try_execute_proposal(&executor, &pid),
        Err(Ok(expected)),
    );
}

/// The invariant this file exists for.
///
/// Read through `try_get_admins` rather than `get_admins`: an empty set makes
/// the view return `NotInitialized`, and the test should report that as a
/// violated invariant rather than as a missing function.
fn assert_governance_invariants(client: &LuminaRegistryClient) {
    // `try_*` returns the contract's own `Result` nested inside the invocation
    // error, so both layers are unwrapped: an empty admin set surfaces as the
    // view's `NotInitialized` error, which is exactly the violation to report.
    let admins = client
        .try_get_admins()
        .expect("admin set view must always be reachable")
        .expect("admin set must always be readable (and therefore non-empty)");
    assert!(!admins.is_empty(), "admin set must never be empty");
    assert!(
        admins.len() >= MIN_ADMINS,
        "admin set ({}) fell below MIN_ADMINS ({})",
        admins.len(),
        MIN_ADMINS,
    );

    let threshold = client
        .try_get_threshold()
        .expect("threshold view must always be reachable")
        .expect("threshold must always be readable");
    assert!(threshold > 0, "threshold must never be zero");
    assert!(
        threshold <= admins.len(),
        "threshold {threshold} exceeds admin count {}",
        admins.len(),
    );
}

fn set_threshold(env: &Env, client: &LuminaRegistryClient, new_threshold: u32) {
    let admin = client.get_admins().get(0).unwrap();
    let pid = client.propose_change_threshold(&admin, &new_threshold);
    approve_and_wait(env, client, pid);
    execute(env, client, pid);
    assert_governance_invariants(client);
}

// ─── #229: the admin set is never empty ──────────────────────────────────────

#[test]
fn threshold_guard_is_what_keeps_the_admin_set_non_empty() {
    let (env, client, _pool) = setup_with_admins(3);
    assert_governance_invariants(&client);

    let admins = client.get_admins();
    let a1 = admins.get(0).unwrap();
    let a2 = admins.get(1).unwrap();
    let a3 = admins.get(2).unwrap();

    // 3 admins, threshold 2: one removal is allowed.
    set_threshold(&env, &client, 2);
    let pid = client.propose_remove_admin(&a1, &a3);
    approve_and_wait(&env, &client, pid);
    execute(&env, &client, pid);
    assert_governance_invariants(&client);
    assert_eq!(client.get_admins().len(), 2);

    // 2 admins, threshold 2: removing either one would leave the threshold
    // unsatisfiable, and in particular the set could never reach empty. The
    // removal must be refused by the threshold guard itself.
    let pid = client.propose_remove_admin(&a1, &a2);
    approve_and_wait(&env, &client, pid);
    assert_execution_fails_with(&env, &client, pid, RegistryError::InvalidThreshold);
    assert_governance_invariants(&client);
    assert_eq!(client.get_admins().len(), 2);

    // Lowering the threshold to 1 removes the threshold objection; the floor
    // at MIN_ADMINS still keeps the set non-empty. Both guards, checked here
    // as real behaviour, not as an implication of each other.
    set_threshold(&env, &client, 1);
    let pid = client.propose_remove_admin(&a1, &a2);
    approve_and_wait(&env, &client, pid);
    assert_execution_fails_with(&env, &client, pid, RegistryError::AdminSetTooSmall);
    assert_governance_invariants(&client);
    assert_eq!(client.get_admins().len(), 2);
}

// ─── #230: threshold never exceeds the admin count ───────────────────────────

#[test]
fn mixed_removal_and_threshold_changes_preserve_the_invariant() {
    let (env, client, pool) = setup_with_admins(4);
    assert_governance_invariants(&client);
    let a1 = pool[0].clone();
    let a2 = pool[1].clone();
    let a3 = pool[2].clone();
    let a4 = pool[3].clone();
    assert_eq!(client.get_admins().len(), 4);
    assert_eq!(client.get_threshold(), 1);

    // `propose_change_threshold` validates against the admin count at proposal
    // time, so this is legal now: 4 admins support a threshold of 3.
    let pending_change = client.propose_change_threshold(&a1, &3);
    client.approve_proposal(&a1, &pending_change);
    client.approve_proposal(&a2, &pending_change);
    advance_ledger(&env, TIMELOCK_LEDGERS);
    assert_governance_invariants(&client);

    // Remove one admin while the change is pending: 4 -> 3 still satisfies the
    // active threshold (1) and leaves room for a threshold of 3.
    let removal = client.propose_remove_admin(&a1, &a4);
    client.approve_proposal(&a1, &removal);
    client.approve_proposal(&a2, &removal);
    advance_ledger(&env, TIMELOCK_LEDGERS);
    execute(&env, &client, removal);
    assert_governance_invariants(&client);
    assert_eq!(client.get_admins().len(), 3);

    // Remove another: 3 -> 2 still satisfies the active threshold (1), but the
    // pending change to 3 is now unsatisfiable.
    let removal = client.propose_remove_admin(&a1, &a3);
    client.approve_proposal(&a1, &removal);
    client.approve_proposal(&a2, &removal);
    advance_ledger(&env, TIMELOCK_LEDGERS);
    execute(&env, &client, removal);
    assert_governance_invariants(&client);
    assert_eq!(client.get_admins().len(), 2);
    assert_eq!(client.get_threshold(), 1);

    // The execution-time ChangeThreshold guard: the proposal was valid when
    // created, but executing it now would leave threshold 3 over 2 admins.
    assert_execution_fails_with(
        &env,
        &client,
        pending_change,
        RegistryError::InvalidThreshold,
    );
    assert_governance_invariants(&client);
    assert_eq!(client.get_threshold(), 1);
    assert!(!client.get_proposal(&pending_change).executed);

    // A threshold above the current admin count is also refused when proposed.
    assert_eq!(
        client.try_propose_change_threshold(&a1, &3),
        Err(Ok(RegistryError::InvalidThreshold)),
    );
    assert_governance_invariants(&client);

    // With two admins, raising the threshold to 2 is legal and leaves no slack.
    set_threshold(&env, &client, 2);
    assert_eq!(client.get_threshold(), 2);

    // Removing either admin would leave one against a threshold of two: the
    // RemoveAdmin guard. With MIN_ADMINS = 2 the alternative guard cannot be
    // what refuses it, so removing this check lets the set fall below the
    // threshold here.
    let removal = client.propose_remove_admin(&a1, &a2);
    approve_and_wait(&env, &client, removal);
    assert_execution_fails_with(&env, &client, removal, RegistryError::InvalidThreshold);
    assert_governance_invariants(&client);
    assert_eq!(client.get_admins().len(), 2);

    // Lower to 1: only MIN_ADMINS now stands between the set and one member.
    // The invariant still holds after every attempt.
    set_threshold(&env, &client, 1);
    let removal = client.propose_remove_admin(&a1, &a2);
    approve_and_wait(&env, &client, removal);
    assert_execution_fails_with(&env, &client, removal, RegistryError::AdminSetTooSmall);
    assert_governance_invariants(&client);
    assert_eq!(client.get_admins().len(), 2);
}

// ─── Property-based coverage ─────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
enum GovOp {
    AddAdmin(usize),
    RemoveAdmin(usize),
    ChangeThreshold(u32),
}

fn gov_op_strategy() -> impl Strategy<Value = GovOp> {
    proptest::prop_oneof![
        (0..ADMIN_POOL).prop_map(GovOp::AddAdmin),
        (0..ADMIN_POOL).prop_map(GovOp::RemoveAdmin),
        (0..=(ADMIN_POOL as u32 + 1)).prop_map(GovOp::ChangeThreshold),
    ]
}

/// Run one proposed governance operation to completion, tolerating the
/// proposal-time and execution-time refusals any random sequence will produce.
/// The point is not whether it succeeds but what the state looks like
/// afterwards.
fn apply_gov_op(env: &Env, client: &LuminaRegistryClient, pool: &[Address], op: GovOp) {
    let proposer = client.get_admins().get(0).unwrap();
    let proposed = match op {
        GovOp::AddAdmin(i) => client.try_propose_add_admin(&proposer, &pool[i]),
        GovOp::RemoveAdmin(i) => client.try_propose_remove_admin(&proposer, &pool[i]),
        GovOp::ChangeThreshold(t) => client.try_propose_change_threshold(&proposer, &t),
    };

    let Ok(Ok(pid)) = proposed else {
        // A refusal at proposal time changes nothing; still check the state.
        assert_governance_invariants(client);
        return;
    };

    approve_and_wait(env, client, pid);
    let executor = client.get_admins().get(0).unwrap();
    // A refusal here is expected (and must roll back); either way the
    // invariant is re-asserted below.
    let _ = client.try_execute_proposal(&executor, &pid);
    assert_governance_invariants(client);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(24))]

    /// No sequence of add, remove and threshold-change operations — whether
    /// each proposal passes or is refused — can leave the admin set empty or
    /// the threshold above the admin count.
    #[test]
    fn admin_invariants_hold_across_any_governance_sequence(
        seq in proptest::collection::vec(gov_op_strategy(), 0..=8)
    ) {
        let (env, client, pool) = setup_with_admins(ADMIN_POOL);
        assert_governance_invariants(&client);
        for op in seq {
            apply_gov_op(&env, &client, &pool, op);
        }
    }
}
