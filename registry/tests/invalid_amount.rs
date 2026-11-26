// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Dedicated test for the `InvalidAmount` error path.
//!
//! `RegistryError::InvalidAmount` (code 16) is the contract's catch-all for
//! "amount is zero or negative and should not be". Callers branch on the bare
//! integer code, so each raise site needs a failing test if the guard is ever
//! removed.
//!
//! Current raise sites, all tested below:
//!
//! - `stake` — via `validate_positive_amount(amount)`; `amount <= 0` rejects.
//! - `propose_slash` — via `validate_positive_amount(amount)`.
//! - `propose_set_registration_fee` — `fee < 0` rejects; zero is a valid
//!   "fee disabled" value and is intentionally not covered here.
//! - `propose_configure_minimum_stake` — `minimum < 0` rejects.
//!
//! The `apply_action` branches for `SetRegistrationFee`, `ConfigureMinimumStake`
//! and `WithdrawFromTreasury` are defensive copies of the same check — they
//! fire only if the propose-time guard is bypassed, which is unreachable
//! through the public interface. They are not asserted directly here.
//!
//! Issue #271: Add a dedicated test for the `InvalidAmount` error path.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{token, Address, Env, String, Vec};

use lumina_registry::{
    Category, LuminaRegistry, LuminaRegistryClient, RegistryError, TIMELOCK_LEDGERS,
};

/// A registry with a single admin (threshold = 1).
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

/// Open staking against a fresh SEP-41 token and return `(token_id, treasury)`.
/// Staking must be configured for `stake` and `propose_slash` to reach their
/// amount validation — without it they fail earlier with `StakingNotConfigured`.
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

/// Register a contract and return `(owner, contract_id)`. The owner has no
/// balance; callers that need one must mint it.
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

// ─── stake ──────────────────────────────────────────────────────────────────

/// `stake` with a zero amount returns `InvalidAmount`.
/// This is the primary acceptance-criterion test: if
/// `validate_positive_amount` is removed from the stake path, this fails.
#[test]
fn stake_rejects_zero_amount() {
    let (env, client, admin) = setup();
    let (token_id, _) = open_staking(&env, &client, &admin);
    let (owner, target) = register(&env, &client);
    // Enough balance that balance is not the reason the call fails.
    token::StellarAssetClient::new(&env, &token_id).mint(&owner, &100);

    assert_eq!(
        client.try_stake(&owner, &target, &0),
        Err(Ok(RegistryError::InvalidAmount)),
    );
}

/// `stake` with a negative amount is also rejected. The guard is
/// `amount <= 0`, so this catches a future refactor that weakens it to
/// `amount == 0`.
#[test]
fn stake_rejects_negative_amount() {
    let (env, client, admin) = setup();
    let (token_id, _) = open_staking(&env, &client, &admin);
    let (owner, target) = register(&env, &client);
    token::StellarAssetClient::new(&env, &token_id).mint(&owner, &100);

    assert_eq!(
        client.try_stake(&owner, &target, &-1),
        Err(Ok(RegistryError::InvalidAmount)),
    );
}

// ─── propose_slash ──────────────────────────────────────────────────────────

/// `propose_slash` with a zero amount returns `InvalidAmount` at proposal
/// time, before anything is stored.
#[test]
fn propose_slash_rejects_zero_amount() {
    let (env, client, admin) = setup();
    let (_, _) = open_staking(&env, &client, &admin);
    let (_owner, target) = register(&env, &client);

    assert_eq!(
        client.try_propose_slash(
            &admin,
            &target,
            &0,
            &String::from_str(&env, "reason"),
        ),
        Err(Ok(RegistryError::InvalidAmount)),
    );
}

/// `propose_slash` with a negative amount is also rejected.
#[test]
fn propose_slash_rejects_negative_amount() {
    let (env, client, admin) = setup();
    let (_, _) = open_staking(&env, &client, &admin);
    let (_owner, target) = register(&env, &client);

    assert_eq!(
        client.try_propose_slash(
            &admin,
            &target,
            &-100,
            &String::from_str(&env, "reason"),
        ),
        Err(Ok(RegistryError::InvalidAmount)),
    );
}

// ─── propose_set_registration_fee ───────────────────────────────────────────

/// `propose_set_registration_fee` with a negative fee returns `InvalidAmount`.
/// Zero is valid (disables the fee) and is intentionally not tested here;
/// adding an "accepts zero" assertion would double as a specification of
/// what zero means, outside this test's scope.
#[test]
fn propose_set_registration_fee_rejects_negative_fee() {
    let (_env, client, admin) = setup();

    assert_eq!(
        client.try_propose_set_registration_fee(&admin, &-1),
        Err(Ok(RegistryError::InvalidAmount)),
    );
}

// ─── propose_configure_minimum_stake ────────────────────────────────────────

/// `propose_configure_minimum_stake` with a negative minimum returns
/// `InvalidAmount`. Zero is valid (no minimum) and not covered here.
#[test]
fn propose_configure_minimum_stake_rejects_negative_minimum() {
    let (_env, client, admin) = setup();

    assert_eq!(
        client.try_propose_configure_minimum_stake(&admin, &-1),
        Err(Ok(RegistryError::InvalidAmount)),
    );
}
