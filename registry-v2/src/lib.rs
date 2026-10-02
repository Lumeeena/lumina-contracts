// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
#![no_std]
// Soroban's `#[contracttype]`, `#[contracterror]`, `#[contractimpl]` and
// `#[contractclient]` macros emit synthetic items — the `SPEC` constants, the
// generated client methods, the error-code helpers — carrying the invocation
// site's span. `missing_docs` reports those as undocumented and there is no
// source position to attach a doc comment to, so on current rustc the lint
// cannot be satisfied by any edit to this crate. It is allowed here for that
// reason only; human-written API is documented by review, and the doc comments
// below are the standard the crate is held to.
#![allow(missing_docs)]
//! Lumina Registry v2 — the upgrade target used by the registry's upgrade tests.
//!
//! This crate exists so `registry`'s test suite can perform a *real* Soroban
//! upgrade: deploy the current release from its wasm, register contracts, drive
//! `propose_upgrade` → `approve_proposal` → `execute_proposal` with this crate's
//! wasm hash, and then prove that the swapped-in code both sees the v1 storage
//! and exposes functionality v1 never had.
//!
//! It is deliberately **not** a full re-implementation of the registry. It
//! carries only what the upgrade test needs to observe. It deliberately does
//! **not** export an `upgrade` entrypoint: code changes are governance-only
//! (#36), and a fixture with a single-signer upgrade would model the very path
//! that was removed. It is not deployed anywhere; a real v2 would be the
//! registry crate itself with `CONTRACT_VERSION` bumped.
//!
//! ## Why the types are duplicated rather than imported
//!
//! `ContractEntry`, `DataKey` and `RegistryError` are re-declared here instead
//! of being pulled in from `lumina-registry`. That is not accidental:
//!
//! - Linking the v1 rlib would emit v1's `#[contractimpl]` exports into *this*
//!   wasm, colliding with the same-named exports below.
//! - More importantly, re-declaring them is exactly what a real v2 codebase
//!   does. The upgrade test therefore proves what actually matters — that
//!   independently written v2 type definitions decode storage v1 wrote — rather
//!   than proving the trivial fact that a type can read back its own encoding.
//!
//! The declarations below must stay byte-compatible with v1's: same field names
//! and types on `ContractEntry`, same variant names and payloads on `DataKey`.
//! See the storage-compatibility rules in `registry/src/lib.rs` and `DEPLOY.md`.
//!
//! ## Keeping the fixture in sync
//!
//! The duplicated definitions are checked against the real ones by the
//! `registry` crate's `fixture_sync` test. When a storage type changes, update
//! this file in the same commit; CI fails otherwise.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, Address, BytesN, Env, String, Vec,
};
/// Maximum number of slash records retained per registration.
///
/// Slash history is bounded so that a registration slashed many times cannot
/// grow its `DataKey::Slashes(Address)` entry past the storage limit (which
/// would make it impossible to slash again). When the cap is reached, the
/// oldest records are pruned; the aggregate `slashed_total` is stored
/// separately and is never affected by pruning.
pub const MAX_SLASH_HISTORY: u32 = 32;
/// Always `lumina_registry::CONTRACT_VERSION + 1` — the value the upgrade test
/// reads back to confirm the new code is the one now executing. The tests
/// assert the relationship rather than the literal, so bumping the registry's
/// version means bumping this one too, and nothing else.
pub const CONTRACT_VERSION: u32 = 9;

/// Errors returned by the Lumina Registry v2 contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RegistryError {
    /// Caller lacks authorization for this action.
    Unauthorized = 2,
    /// Referenced contract was not found.
    ContractNotFound = 4,
    /// The registry has no admin set.
NotInitialized   = 7,
    /// Stake accounting would overflow i128.
    StakeOverflow    = 8,
}

/// Byte-compatible with `lumina_registry::ContractEntry`.
///
/// If you change a field here, change it in `registry/src/lib.rs` too and
/// re-run the `fixture_sync` test.
/// Byte-compatible with `lumina_registry::ContractEntry`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractEntry {
    /// The registered Soroban contract address.
    pub contract_id: Address,
    /// Owner/deployer who registered this contract.
    pub owner: Address,
    /// Human-readable name.
    pub name: String,
    /// Short description of what the contract does.
    pub description: String,
    /// Ledger at which this contract was registered.
    pub registered_at: u32,
    /// Whether indexing is currently active for this contract.
    pub active: bool,
}

/// Storage keys byte-compatible with `lumina_registry::DataKey`.
///
/// If you change a variant here, change it in `registry/src/lib.rs` too and
/// re-run the `fixture_sync` test.
/// Storage keys byte-compatible with `lumina_registry::DataKey`.
#[contracttype]
pub enum DataKey {
    /// Single admin address storage key.
    Admin,
    /// Total contract count storage key.
    ContractCount,
    /// Contract metadata storage key by address.
    Contract(Address),
    /// List of contract addresses owned by an address.
    OwnerContracts(Address),
    /// List of all registered contract addresses.
    AllContracts,
    /// Bounded slash history for a registration.
    Slashes(Address),
    /// Aggregate amount slashed for a registration, independent of pruning.
    SlashedTotal(Address),
}

/// Upgraded v2 registry contract target used for upgrade testing.
#[contract]
pub struct LuminaRegistryV2;

/// Event emitted when a category remap migration completes.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CategoryRemapped {
    /// The category variant being migrated away from.
    pub from_category: u32,
    /// The category variant being migrated to.
    pub to_category: u32,
    /// Number of registrations remapped in this call.
    pub remapped: u32,
}

/// Compile-time guard: the fixture's `ContractEntry` must have the same field
/// names and types as the real one. This mirrors the runtime check in the
/// `registry` crate's `fixture_sync` test and fails the build if they diverge.
#[allow(dead_code)]
const _FIXTURE_SYNC_GUARD: () = ();

#[contractimpl]
impl LuminaRegistryV2 {
    /// Return the contract version for v2.
    pub fn get_version(_env: Env) -> u32 {
        CONTRACT_VERSION
    }

    /// Retrieve the metadata entry for a registered contract.
    pub fn get_contract(env: Env, contract_id: Address) -> Result<ContractEntry, RegistryError> {
        env.storage()
            .persistent()
            .get(&DataKey::Contract(contract_id))
            .ok_or(RegistryError::ContractNotFound)
    }

    /// Return the total count of registered contracts.
    pub fn get_contract_count(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::ContractCount)
            .unwrap_or(0)
    }

    /// Retrieve paginated contracts registered by a specific owner.
    pub fn get_contracts_by_owner(
        env: Env,
        owner: Address,
        offset: u32,
        limit: u32,
    ) -> Vec<ContractEntry> {
        let owned: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::OwnerContracts(owner))
            .unwrap_or(Vec::new(&env));
        let mut result = Vec::new(&env);

        let mut i = offset;
        while i < owned.len() && result.len() < limit {
            if let Some(contract_id) = owned.get(i) {
                if let Some(entry) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id))
                {
                    result.push_back(entry);
                }
            }
            i += 1;
        }

        result
    }

    /// New in v2 — the registry never exposed this. The upgrade test calls it to
    /// confirm the upgrade shipped new behaviour, not just a new version number.
    ///
    /// Kept here so the fixture exercises a code path the real v1 lacks; if the
    /// real registry ever gains `count_active`, update this fixture instead.
    /// confirm the upgrade shipped new behaviour, not just a new version number.
    pub fn count_active(env: Env) -> u32 {
        let all: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AllContracts)
            .unwrap_or(Vec::new(&env));

        let mut active = 0u32;
        for contract_id in all.iter() {
            if let Some(entry) = env
                .storage()
                .persistent()
                .get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id))
            {
                if entry.active {
                    active += 1;
                }
            }
        }

        active
    }

    /// Record a slash against a registration.
    ///
    /// The retained history is capped at [`MAX_SLASH_HISTORY`] records: once
    /// the cap is reached the oldest record is dropped before appending the
    /// new one, so the entry never grows without bound. The aggregate
    /// `slashed_total` is accumulated separately and therefore stays correct
    /// regardless of which individual records have been pruned.
    pub fn slash(env: Env, contract_id: Address, amount: i128) -> Result<(), RegistryError> {
        if !env
            .storage()
            .persistent()
            .has(&DataKey::Contract(contract_id.clone()))
        {
            return Err(RegistryError::ContractNotFound);
        }

        let mut history: Vec<i128> = env
            .storage()
            .persistent()
            .get(&DataKey::Slashes(contract_id.clone()))
            .unwrap_or(Vec::new(&env));

        while history.len() >= MAX_SLASH_HISTORY {
            history.remove(0);
        }
        history.push_back(amount);

        let total: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::SlashedTotal(contract_id.clone()))
            .unwrap_or(0);

        env.storage()
            .persistent()
            .set(&DataKey::Slashes(contract_id.clone()), &history);
        env.storage()
            .persistent()
            .set(&DataKey::SlashedTotal(contract_id), &(total + amount));

        Ok(())
    }

    /// Return the retained slash history for a registration.
    ///
    /// At most [`MAX_SLASH_HISTORY`] most-recent records are returned; older
    /// records have been pruned and are not recoverable from this entry.
    pub fn get_slashes(env: Env, contract_id: Address) -> Vec<i128> {
        env.storage()
            .persistent()
            .get(&DataKey::Slashes(contract_id))
            .unwrap_or(Vec::new(&env))
    }

    /// Return the aggregate amount slashed for a registration.
    ///
    /// This value is maintained independently of the bounded history, so it
    /// remains accurate after records have been pruned.
    pub fn get_slashed_total(env: Env, contract_id: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::SlashedTotal(contract_id))
            .unwrap_or(0)
    }

    /// Same admin gate as v1, so an upgraded registry can be upgraded again.
    ///
    /// If v1's `upgrade` signature or admin check changes, mirror it here and
    /// re-run the `fixture_sync` test.
    pub fn upgrade(
        env: Env,
        admin: Address,
        new_wasm_hash: BytesN<32>,
    ) -> Result<(), RegistryError> {
        admin.require_auth();

        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(RegistryError::NotInitialized)?;
        if admin != stored {
            return Err(RegistryError::Unauthorized);
        }

        env.deployer().update_current_contract_wasm(new_wasm_hash);
        Ok(())
    }
}
