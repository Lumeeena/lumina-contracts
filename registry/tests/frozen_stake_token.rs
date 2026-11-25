// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Public-facing coverage of what happens when the stake token refuses a
//! transfer — the real-world scenario being a Stellar asset whose issuer
//! has un-authorised the registry's (or a staker's) trustline.
//!
//! The `#[cfg(test)]` module in `registry/src/lib.rs` already pins these
//! invariants end-to-end with a shared `FailingToken` mock. This file
//! rebuilds a minimal failing token here and drives it through the public
//! client, so an external reader can see the guarantees without having to
//! open the test module: no bookkeeping change outlives a failed transfer,
//! and a retry once the token recovers goes through exactly once.
//!
//! Issue #253: Test what happens when the stake token is paused or frozen.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, String, Vec};

use lumina_registry::{
    Category, LuminaRegistry, LuminaRegistryClient, RegistryError, TIMELOCK_LEDGERS,
};

// ─── A SEP-41-shaped token whose `transfer` can be switched off ─────────────

#[contracttype]
enum FailingKey {
    Balance(Address),
    Failing,
}

#[contract]
struct FailingToken;

#[contractimpl]
impl FailingToken {
    pub fn decimals(_env: Env) -> u32 {
        7
    }

    pub fn mint(env: Env, to: Address, amount: i128) {
        let key = FailingKey::Balance(to);
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(current + amount));
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&FailingKey::Balance(id))
            .unwrap_or(0)
    }

    pub fn set_failing(env: Env, failing: bool) {
        env.storage().instance().set(&FailingKey::Failing, &failing);
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        if env
            .storage()
            .instance()
            .get(&FailingKey::Failing)
            .unwrap_or(false)
        {
            panic!("transfer refused: trustline unauthorised");
        }
        let from_balance = Self::balance(env.clone(), from.clone());
        if from_balance < amount {
            panic!("transfer refused: insufficient balance");
        }
        env.storage()
            .persistent()
            .set(&FailingKey::Balance(from), &(from_balance - amount));
        Self::mint(env, to, amount);
    }
}

// ─── Setup ──────────────────────────────────────────────────────────────────

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

fn advance_ledger(env: &Env, n: u32) {
    let seq = env.ledger().sequence();
    env.ledger().set_sequence_number(seq + n);
}

fn pass_proposal(env: &Env, client: &LuminaRegistryClient, admin: &Address, pid: u32) {
    client.approve_proposal(admin, &pid);
    advance_ledger(env, TIMELOCK_LEDGERS);
    client.execute_proposal(admin, &pid);
}

/// Register a stake token mock and configure the registry to use it.
fn setup_with_failing_token() -> (
    Env,
    LuminaRegistryClient<'static>,
    Address,
    FailingTokenClient<'static>,
    Address,
) {
    let (env, client, admin) = setup();
    let token_id = env.register(FailingToken, ());
    let token = FailingTokenClient::new(&env, &token_id);
    let treasury = Address::generate(&env);
    let pid = client.propose_configure_staking(&admin, &token_id, &treasury);
    pass_proposal(&env, &client, &admin, pid);
    (env, client, admin, token, treasury)
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

/// What the token says the registry holds must always match the sum of
/// stakes the registry has recorded against its registrations.
fn assert_solvency(
    client: &LuminaRegistryClient,
    token: &FailingTokenClient,
    registrations: &[&Address],
) {
    let recorded: i128 = registrations.iter().map(|r| client.get_stake(r)).sum();
    assert_eq!(recorded, token.balance(&client.address));
}

// ─── Stake ──────────────────────────────────────────────────────────────────

/// A `stake` call that the token refuses must leave no trace: no recorded
/// stake, no debit, and no drift between the registry's books and the
/// token's balances. A later successful `stake` counts exactly once.
#[test]
fn stake_fails_cleanly_when_the_token_refuses_the_transfer() {
    let (env, client, _admin, token, _treasury) = setup_with_failing_token();
    let (owner, target) = register(&env, &client);
    token.mint(&owner, &1_000);

    token.set_failing(&true);
    assert!(client.try_stake(&owner, &target, &400).is_err());

    assert_eq!(client.get_stake(&target), 0);
    assert_eq!(token.balance(&owner), 1_000);
    assert_solvency(&client, &token, &[&target]);

    token.set_failing(&false);
    client.stake(&owner, &target, &400);
    assert_eq!(client.get_stake(&target), 400);
    assert_eq!(token.balance(&owner), 600);
    assert_solvency(&client, &token, &[&target]);
}

/// A failed top-up on an existing stake leaves the existing stake untouched.
#[test]
fn top_up_fails_cleanly_and_leaves_the_existing_stake() {
    let (env, client, _admin, token, _treasury) = setup_with_failing_token();
    let (owner, target) = register(&env, &client);
    token.mint(&owner, &1_000);
    client.stake(&owner, &target, &400);

    token.set_failing(&true);
    assert!(client.try_stake(&owner, &target, &100).is_err());

    assert_eq!(client.get_stake(&target), 400);
    assert_eq!(token.balance(&owner), 600);
    assert_solvency(&client, &token, &[&target]);
}

// ─── Withdraw ───────────────────────────────────────────────────────────────

/// A failed withdrawal keeps the stake recorded. Zeroing the registry's
/// books without the tokens actually leaving would strand them in the
/// contract with no registration able to claim them.
#[test]
fn withdraw_fails_cleanly_and_keeps_the_stake_recorded() {
    let (env, client, _admin, token, _treasury) = setup_with_failing_token();
    let (owner, target) = register(&env, &client);
    token.mint(&owner, &750);
    client.stake(&owner, &target, &750);
    client.deactivate(&owner, &target);

    token.set_failing(&true);
    assert!(client.try_withdraw_stake(&owner, &target).is_err());

    assert_eq!(client.get_stake(&target), 750);
    assert_eq!(token.balance(&owner), 0);
    assert_solvency(&client, &token, &[&target]);

    // Once the trustline is re-authorised, the withdrawal goes through as
    // normal and the stake lands back with the owner.
    token.set_failing(&false);
    assert_eq!(client.withdraw_stake(&owner, &target), 750);
    assert_eq!(client.get_stake(&target), 0);
    assert_eq!(token.balance(&owner), 750);
    assert_solvency(&client, &token, &[&target]);
}

// ─── Slash ──────────────────────────────────────────────────────────────────

/// A `Slash` proposal whose execution hits a frozen trustline fails the
/// whole transaction: no stake moved, no slash recorded, and (critically)
/// the proposal is **not** left in an executed state, so the slash can be
/// retried once the trustline recovers.
#[test]
fn slash_execution_fails_cleanly_when_the_token_refuses_the_transfer() {
    let (env, client, admin, token, treasury) = setup_with_failing_token();
    let (owner, target) = register(&env, &client);
    let staked = 1_000i128;
    token.mint(&owner, &staked);
    client.stake(&owner, &target, &staked);

    let reason = String::from_str(&env, "stake-token-test slash");
    let pid = client.propose_slash(&admin, &target, &400, &reason);
    client.approve_proposal(&admin, &pid);
    advance_ledger(&env, TIMELOCK_LEDGERS);

    token.set_failing(&true);
    assert!(client.try_execute_proposal(&admin, &pid).is_err());

    assert_eq!(client.get_stake(&target), staked);
    assert_eq!(client.get_slashes(&target).len(), 0);
    assert!(!client.get_proposal(&pid).executed);
    assert_eq!(token.balance(&treasury), 0);
    assert_solvency(&client, &token, &[&target]);

    token.set_failing(&false);
    client.execute_proposal(&admin, &pid);
    assert_eq!(client.get_stake(&target), staked - 400);
    assert_eq!(token.balance(&treasury), 400);
    assert_solvency(&client, &token, &[&target]);

    // The retried slash applied its withdraw lock as normal.
    client.deactivate(&owner, &target);
    assert_eq!(
        client.try_withdraw_stake(&owner, &target),
        Err(Ok(RegistryError::StakeLocked)),
    );
}
