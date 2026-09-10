#![no_std]
//! Lumina Registry v2 — the upgrade target used by the registry's upgrade tests.
//!
//! This crate exists so `registry`'s test suite can perform a *real* Soroban
//! upgrade: deploy v1 from its wasm, register contracts, call `upgrade()` with
//! this crate's wasm hash, and then prove that the swapped-in code both sees the
//! v1 storage and exposes functionality v1 never had.
//!
//! It is deliberately **not** a full re-implementation of the registry. It
//! carries only what the upgrade test needs to observe, plus `upgrade()` itself
//! so an upgraded registry stays upgradeable. It is not deployed anywhere; a
//! real v2 would be the registry crate itself with `CONTRACT_VERSION` bumped.
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

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, Address, BytesN, Env, Vec,
};

/// Always `lumina_registry::CONTRACT_VERSION + 1` — the value the upgrade test
/// reads back to confirm the new code is the one now executing. The tests
/// assert the relationship rather than the literal, so bumping the registry's
/// version means bumping this one too, and nothing else.
pub const CONTRACT_VERSION: u32 = 3;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RegistryError {
    Unauthorized     = 2,
    ContractNotFound = 4,
    NotInitialized   = 7,
}

/// Byte-compatible with `lumina_registry::ContractEntry`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractEntry {
    pub contract_id: Address,
    pub owner: Address,
    pub name: soroban_sdk::String,
    pub description: soroban_sdk::String,
    pub registered_at: u32,
    pub active: bool,
}

/// Byte-compatible with `lumina_registry::DataKey`.
#[contracttype]
pub enum DataKey {
    Admin,
    ContractCount,
    Contract(Address),
    OwnerContracts(Address),
    AllContracts,
}

#[contract]
pub struct LuminaRegistryV2;

#[contractimpl]
impl LuminaRegistryV2 {
    pub fn get_version(_env: Env) -> u32 {
        CONTRACT_VERSION
    }

    pub fn get_contract(env: Env, contract_id: Address) -> Result<ContractEntry, RegistryError> {
        env.storage()
            .persistent()
            .get(&DataKey::Contract(contract_id))
            .ok_or(RegistryError::ContractNotFound)
    }

    pub fn get_contract_count(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::ContractCount).unwrap_or(0)
    }

    pub fn get_contracts_by_owner(env: Env, owner: Address, offset: u32, limit: u32) -> Vec<ContractEntry> {
        let owned: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::OwnerContracts(owner))
            .unwrap_or(Vec::new(&env));
        let mut result = Vec::new(&env);

        let mut i = offset;
        while i < owned.len() && result.len() < limit {
            let contract_id = owned.get(i).unwrap();
            if let Some(entry) = env
                .storage()
                .persistent()
                .get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id))
            {
                result.push_back(entry);
            }
            i += 1;
        }

        result
    }

    /// New in v2 — the registry never exposed this. The upgrade test calls it to
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

    /// Same admin gate as v1, so an upgraded registry can be upgraded again.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) -> Result<(), RegistryError> {
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
