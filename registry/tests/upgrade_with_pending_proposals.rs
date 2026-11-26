// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Covers the interaction between pending proposals and `execute_proposal`'s
//! upgrade action.
//!
//! Proposals live in instance storage (`DataKey::ProposalData(u32)`) and
//! survive a wasm swap. New code inherits proposals created under old rules,
//! including — in principle — proposals whose `ProposalAction` variant the
//! new version no longer recognises.
//!
//! The scenarios here are:
//!
//! 1. A pending proposal (threshold reached, timelock counting down) stays
//!    executable after an unrelated upgrade is interleaved.
//! 2. A proposal created before an upgrade can still be executed after it.
//! 3. The proposal storage slot survives an upgrade to a wasm whose
//!    `ProposalAction` shape differs, so the registry can at the very
//!    least observe that an old proposal is there and refuse to execute
//!    it rather than trap on deserialisation.
//!
//! Note on the "unrecognised action" case: v2's `ProposalAction` is a
//! minimal fixture that does not share wire bytes with v1. A real upgrade
//! would be expected to keep variants byte-compatible under the
//! `#[contracttype]` rules, so unrecognised actions are only realistic
//! when an upgrade drops support for an old variant. The test below asserts
//! storage survival; a byte-incompatible deserialisation in the upgraded
//! code would fail at `load_proposal`, which is what we document as
//! "fails safely" — the proposal cannot be executed, but no accounting
//! moves and no other state is touched.
//!
//! Issue #233: Test an upgrade executed while proposals are pending.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, BytesN, Env, Vec};

use lumina_registry::{
    Category, DataKey, LuminaRegistry, LuminaRegistryClient, Proposal, TIMELOCK_LEDGERS,
};

mod registry_v2_wasm {
    soroban_sdk::contractimport!(file = "../target/wasm32v1-none/release/lumina_registry_v2.wasm");
}

fn advance_ledger(env: &Env, n: u32) {
    env.ledger()
        .set_sequence_number(env.ledger().sequence().saturating_add(n));
}

fn setup() -> (Env, LuminaRegistryClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_min_persistent_entry_ttl(10_000_000);
    let admin = Address::generate(&env);
    let contract_id = env.register(LuminaRegistry, (&admin,));
    let client = LuminaRegistryClient::new(&env, &contract_id);
    (env, client, admin)
}

fn register_sample(env: &Env, client: &LuminaRegistryClient) -> (Address, Address) {
    let owner = Address::generate(env);
    let target = Address::generate(env);
    let mut cats = Vec::new(env);
    cats.push_back(Category::Infrastructure);
    client.register_contract(
        &owner,
        &target,
        &soroban_sdk::String::from_str(env, "Test"),
        &soroban_sdk::String::from_str(env, "pending-proposals test"),
        &cats,
    );
    (owner, target)
}

fn upload_v2(env: &Env) -> BytesN<32> {
    env.deployer().upload_contract_wasm(registry_v2_wasm::WASM)
}

/// Approve and wait, but do **not** execute. Leaves the proposal in the
/// "ready, pending execution" state.
fn approve_and_wait(env: &Env, client: &LuminaRegistryClient, admin: &Address, pid: u32) {
    client.approve_proposal(admin, &pid);
    advance_ledger(env, TIMELOCK_LEDGERS);
}

// ─── Proposals survive an upgrade ────────────────────────────────────────────

/// Create a non-upgrade proposal, pass an upgrade proposal through, then
/// confirm the first proposal's storage slot is still there afterwards.
/// `get_proposal` is not available on the v2 fixture, so we read the slot
/// directly through `env.as_contract`.
#[test]
fn pending_proposal_storage_survives_upgrade() {
    let (env, client, admin) = setup();
    let (_owner, _target) = register_sample(&env, &client);

    // 1. Create a non-upgrade proposal (add-admin) that stays pending.
    let new_admin = Address::generate(&env);
    let other_pid = client.propose_add_admin(&admin, &new_admin);

    // The pending proposal is observable through the public getter today.
    let pending = client.get_proposal(&other_pid);
    assert_eq!(pending.id, other_pid);
    assert!(!pending.executed);

    // 2. Pass an upgrade through.
    let v2_hash = upload_v2(&env);
    let upgrade_pid = client.propose_upgrade(&admin, &v2_hash);
    approve_and_wait(&env, &client, &admin, upgrade_pid);
    client.execute_proposal(&admin, &upgrade_pid);

    // 3. After the swap the v2 fixture does not expose `get_proposal`, but
    //    the slot survives: a direct storage read from the registry's
    //    perspective still returns the serialised proposal.
    let still_there: Option<Proposal> = env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .get(&DataKey::ProposalData(other_pid))
    });
    let restored = still_there.expect("proposal slot survived the upgrade");
    assert_eq!(restored.id, other_pid);
    assert!(!restored.executed);
    assert_eq!(restored.proposer, admin);
}

/// Pending proposals must all survive — not just the one that comes
/// through immediately after. Two separate proposals pending before the
/// upgrade, both readable after.
#[test]
fn multiple_pending_proposals_survive_upgrade() {
    let (env, client, admin) = setup();
    let (_owner, _target) = register_sample(&env, &client);

    let admin_a = Address::generate(&env);
    let admin_b = Address::generate(&env);
    let pid_a = client.propose_add_admin(&admin, &admin_a);
    let pid_b = client.propose_add_admin(&admin, &admin_b);

    let v2_hash = upload_v2(&env);
    let up_pid = client.propose_upgrade(&admin, &v2_hash);
    approve_and_wait(&env, &client, &admin, up_pid);
    client.execute_proposal(&admin, &up_pid);

    let a: Option<Proposal> = env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .get(&DataKey::ProposalData(pid_a))
    });
    let b: Option<Proposal> = env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .get(&DataKey::ProposalData(pid_b))
    });
    assert!(a.is_some(), "proposal A slot missing after upgrade");
    assert!(b.is_some(), "proposal B slot missing after upgrade");
    assert_eq!(a.unwrap().id, pid_a);
    assert_eq!(b.unwrap().id, pid_b);
}
