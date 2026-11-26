// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Boundary coverage for the `slashed_total` sum in `reputation_of`.
//!
//! `reputation_of` reads a registration's slash history and sums
//! `record.amount` across every record. `slashed_total` is a lifetime
//! accumulator — it is never decremented. The sum is read on every
//! `get_reputation` / `get_contract_profile` / `get_active_profiles` call,
//! so an overflow there would turn a read into a trap under the workspace's
//! `overflow-checks = true` release profile.
//!
//! The contract uses `saturating_add` for this sum (see `reputation_of` in
//! `registry/src/lib.rs`). These tests pin that choice by injecting slash
//! records with amounts that would overflow an unchecked `i128` sum and
//! asserting that the read still succeeds and reports a saturated total.
//!
//! The scenarios go through persistent storage directly rather than the
//! `propose_slash` → `execute_proposal` flow: constructing a real slash of
//! `i128::MAX / 2` would require staking the same amount first, which the
//! mock token could support but at a scale that is unnecessary for
//! exercising a one-line arithmetic guard.
//!
//! Issue #236: Test numeric boundaries in reputation accounting.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env, String, Vec};

use lumina_registry::{
    Category, DataKey, LuminaRegistry, LuminaRegistryClient, SlashRecord, TIMELOCK_LEDGERS,
};

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

fn make_record(env: &Env, amount: i128, label: &str) -> SlashRecord {
    SlashRecord {
        amount,
        reason: String::from_str(env, label),
        slashed_at: 0,
        response: None,
    }
}

fn inject_slashes(
    env: &Env,
    contract_addr: &Address,
    target: &Address,
    records: Vec<SlashRecord>,
) {
    env.as_contract(contract_addr, || {
        env.storage()
            .persistent()
            .set(&DataKey::Slashes(target.clone()), &records);
    });
}

// ─── slashed_total at i128::MAX ─────────────────────────────────────────────

/// Two records that sum to exactly `i128::MAX` must be reported as such,
/// with no trap. This is the boundary below which overflow is impossible —
/// a check it still goes through here confirms the sum is computed, not
/// shortcut.
#[test]
fn reputation_sums_slash_amounts_up_to_i128_max() {
    let (env, client, _admin) = setup();
    let (_owner, target) = register(&env, &client);

    let mut records = Vec::new(&env);
    records.push_back(make_record(&env, i128::MAX / 2, "half-a"));
    // `MAX / 2` is `(MAX - 1) / 2` for odd MAX (which i128::MAX is), so
    // adding one more unit lands exactly on MAX.
    records.push_back(make_record(&env, i128::MAX / 2 + 1, "half-b"));
    inject_slashes(&env, &client.address, &target, records);

    let reputation = client.get_reputation(&target);
    assert_eq!(reputation.slashed_total, i128::MAX);
}

// ─── slashed_total overflows cleanly ────────────────────────────────────────

/// Three records whose sum exceeds `i128::MAX` must saturate at `MAX`
/// rather than trap. `saturating_add` in `reputation_of` guarantees this;
/// if the implementation were ever changed back to `+=`, the workspace's
/// `overflow-checks = true` release profile would turn this call into an
/// abort.
#[test]
fn reputation_saturates_slashed_total_on_overflow() {
    let (env, client, _admin) = setup();
    let (_owner, target) = register(&env, &client);

    let mut records = Vec::new(&env);
    records.push_back(make_record(&env, i128::MAX / 2, "half-a"));
    records.push_back(make_record(&env, i128::MAX / 2, "half-b"));
    records.push_back(make_record(&env, 100, "overflow-tail"));
    inject_slashes(&env, &client.address, &target, records);

    // The call must not trap; the total is pinned at MAX.
    let reputation = client.get_reputation(&target);
    assert_eq!(reputation.slashed_total, i128::MAX);

    // Reading again returns the same saturated value — the sum is
    // deterministic and keyed on stored records, not on an accumulating
    // storage slot.
    let again = client.get_reputation(&target);
    assert_eq!(again.slashed_total, i128::MAX);
}

/// A single `i128::MAX` record is reported verbatim; nothing is folded in
/// on top of it. A later refactor that initialised the accumulator to a
/// non-zero sentinel would fail here.
#[test]
fn reputation_reports_a_single_i128_max_slash_as_max() {
    let (env, client, _admin) = setup();
    let (_owner, target) = register(&env, &client);

    let mut records = Vec::new(&env);
    records.push_back(make_record(&env, i128::MAX, "max-slash"));
    inject_slashes(&env, &client.address, &target, records);

    assert_eq!(client.get_reputation(&target).slashed_total, i128::MAX);
}

// ─── get_contract_profile reads the same saturated value ─────────────────────

/// `get_contract_profile` performs the same sum in a separate loop, so the
/// saturation guarantee has to hold there too — a reader using the profile
/// endpoint sees the identical bounded value.
#[test]
fn contract_profile_also_saturates_slashed_total() {
    let (env, client, _admin) = setup();
    let (_owner, target) = register(&env, &client);

    let mut records = Vec::new(&env);
    records.push_back(make_record(&env, i128::MAX, "already-at-max"));
    records.push_back(make_record(&env, 1, "overflow-tail"));
    inject_slashes(&env, &client.address, &target, records);

    let profile = client.get_contract_profile(&target);
    assert_eq!(profile.reputation.slashed_total, i128::MAX);
}
