// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Dedicated test for the `AlreadyExecuted` error path.
//!
//! `RegistryError::AlreadyExecuted` (code 14) is returned when a governance
//! action is attempted on a proposal that has already been executed. It is
//! raised in two places: `execute_proposal` (re-execution) and
//! `approve_proposal` (late approval). Callers branch on the bare integer, so
//! both checks need a failing test if either is removed.
//!
//! Issue #269: Add a dedicated test for the AlreadyExecuted error path.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env};

use lumina_registry::{
    LuminaRegistry, LuminaRegistryClient, ProposalAction, RegistryError, TIMELOCK_LEDGERS,
};

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

/// Drive an admin-addition proposal through approve → timelock → execute.
///
/// `AddAdmin` is used because it is a stable, self-contained action that needs
/// no companion setup (unlike a slash or deactivate proposal).
fn execute_add_admin(
    env: &Env,
    client: &LuminaRegistryClient,
    admin: &Address,
    new_admin: &Address,
) -> u32 {
    let pid = client.propose_add_admin(admin, new_admin);
    client.approve_proposal(admin, &pid);
    advance_ledger(env, TIMELOCK_LEDGERS);
    client.execute_proposal(admin, &pid);
    assert!(client.get_proposal(&pid).executed);
    pid
}

// ─── AlreadyExecuted ─────────────────────────────────────────────────────────

/// Re-executing an executed proposal must return `AlreadyExecuted`.
///
/// Primary acceptance-criterion test: removing the `proposal.executed` guard
/// from `execute_proposal` must cause this test to fail. A second execution of
/// the `AddAdmin` action would instead fail with `AlreadyAdmin`.
#[test]
fn executed_proposal_cannot_be_executed_again() {
    let (env, client, admin) = setup();
    let new_admin = Address::generate(&env);
    let pid = execute_add_admin(&env, &client, &admin, &new_admin);

    assert_eq!(
        client.try_execute_proposal(&admin, &pid),
        Err(Ok(RegistryError::AlreadyExecuted))
    );
}

/// Approving an already-executed proposal must also return `AlreadyExecuted`.
///
/// This covers the second check that raises the error. Removing it would make
/// a late approval fall through to `AlreadyApproved` instead, which this test
/// would catch.
#[test]
fn executed_proposal_cannot_be_approved_after_execution() {
    let (env, client, admin) = setup();
    let new_admin = Address::generate(&env);
    let pid = execute_add_admin(&env, &client, &admin, &new_admin);

    assert_eq!(
        client.try_approve_proposal(&admin, &pid),
        Err(Ok(RegistryError::AlreadyExecuted))
    );
}

/// The guard is per proposal, not a global execution lock: a fresh proposal
/// must still be executable after an earlier one has completed.
#[test]
fn a_fresh_proposal_still_executes_after_an_executed_one() {
    let (env, client, admin) = setup();
    let first_admin = Address::generate(&env);
    let second_admin = Address::generate(&env);

    let first = execute_add_admin(&env, &client, &admin, &first_admin);
    let second = execute_add_admin(&env, &client, &admin, &second_admin);

    assert_ne!(second, first);
    assert_eq!(
        client.get_proposal(&second).action,
        ProposalAction::AddAdmin(second_admin)
    );
    assert_eq!(client.get_admins().len(), 3);
}
