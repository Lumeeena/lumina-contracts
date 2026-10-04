// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT

//! Example contract that registers itself with the Lumina Registry on deployment.
//!
//! This demonstrates:
//! - How to integrate registry registration into a contract's deployment flow
//! - Using the typed `RegistryInterfaceClient` for type-safe cross-contract calls
//! - A working pattern other projects can copy
//!
//! The contract is a simple DeFi token swap with a greeting function. During
//! deployment (via `__constructor`), it registers itself with the Lumina Registry
//! so that indexers automatically start watching its events.

#![no_std]

use lumina_registry_interface::Category;
use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, Address, Env, String, Vec,
};

#[contractclient(name = "RegistryClient")]
pub trait RegistryClientTrait {
    fn register_contract(
        env: Env,
        owner: Address,
        contract_id: Address,
        name: String,
        description: String,
        categories: Vec<Category>,
    );
    fn update_metadata(
        env: Env,
        owner: Address,
        contract_id: Address,
        name: String,
        description: String,
    );
}

/// Errors this example contract can return.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// Registration with the Lumina Registry failed.
    RegistrationFailed = 1,
    /// Contract is not yet registered.
    NotRegistered = 2,
}

/// A simple example contract that demonstrates registry self-registration.
///
/// This is a minimal DeFi-like contract (token swap placeholder) that registers
/// itself with the Lumina Registry during deployment.
#[contract]
pub struct ExampleDeFiProtocol;

#[contractimpl]
impl ExampleDeFiProtocol {
    /// Constructor: Deploys the contract and registers it with the Lumina Registry.
    ///
    /// # Arguments
    /// - `registry_address`: The deployed Lumina Registry contract address
    /// - `owner`: The address that will own this registration (usually deployer)
    /// - `name`: Protocol name (e.g., "Example DeFi Protocol")
    /// - `description`: Protocol description
    /// - `categories`: Registry categories (e.g., ["DeFi", "Payments"])
    ///
    /// # Example
    /// ```bash
    /// stellar contract deploy \
    ///   --wasm example.wasm \
    ///   --source deployer \
    ///   --network testnet \
    ///   -- \
    ///   --registry_address CAYU... \
    ///   --owner deployer \
    ///   --name "My DeFi Protocol" \
    ///   --description "A demonstration protocol" \
    ///   --categories '["DeFi"]'
    /// ```
    pub fn __constructor(
        env: Env,
        registry_address: Address,
        owner: Address,
        name: String,
        description: String,
        categories: Vec<Category>,
    ) {
        // Get our own contract address
        let self_address = env.current_contract_address();

        // Create a client for the registry
        let registry = RegistryClient::new(&env, &registry_address);

        // Register ourselves
        // Note: The owner must authorize this call. During deployment,
        // the deployer typically authorizes on behalf of the owner.
        registry.register_contract(&owner, &self_address, &name, &description, &categories);

        // Store registry address for future reference
        env.storage()
            .instance()
            .set(&DataKey::RegistryAddress, &registry_address);
        env.storage()
            .instance()
            .set(&DataKey::RegistryOwner, &owner);
        env.storage().instance().set(&DataKey::Registered, &true);

        // Emit an event so we can verify registration happened
        env.events().publish(
            (String::from_str(&env, "contract_deployed"),),
            (self_address.clone(), registry_address.clone()),
        );
    }

    /// Returns whether this contract is registered with the registry.
    pub fn is_registered(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Registered)
            .unwrap_or(false)
    }

    /// Returns the registry address this contract registered with.
    pub fn get_registry(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::RegistryAddress)
            .ok_or(Error::NotRegistered)
    }

    /// Example business logic: A greeting function.
    ///
    /// This represents the actual functionality of your contract.
    /// The registry registration is just the integration layer.
    pub fn greet(env: Env, _to: String) -> String {
        String::from_str(&env, "Hello, World!")
    }

    /// Example business logic: Simulated token swap.
    ///
    /// This would be your DeFi logic, oracle integration, or other functionality.
    /// The registry just makes sure indexers know to watch your events.
    pub fn swap_tokens(
        env: Env,
        from: Address,
        token_in: Address,
        amount_in: i128,
        token_out: Address,
        min_amount_out: i128,
    ) -> i128 {
        from.require_auth();

        // In a real contract, this would:
        // 1. Transfer token_in from the caller
        // 2. Execute the swap logic
        // 3. Transfer token_out to the caller
        // 4. Emit events that Lumina will index

        // For this example, just emit a mock event
        env.events().publish(
            (String::from_str(&env, "swap_executed"),),
            (from, token_in, amount_in, token_out, min_amount_out),
        );

        // Mock return: amount out
        min_amount_out
    }

    /// Update registration metadata (callable by owner).
    ///
    /// Demonstrates how to keep your registry entry current.
    pub fn update_registry_metadata(
        env: Env,
        owner: Address,
        new_name: String,
        new_description: String,
    ) -> Result<(), Error> {
        owner.require_auth();

        let registry_address: Address = env
            .storage()
            .instance()
            .get(&DataKey::RegistryAddress)
            .ok_or(Error::NotRegistered)?;

        let stored_owner: Address = env
            .storage()
            .instance()
            .get(&DataKey::RegistryOwner)
            .ok_or(Error::NotRegistered)?;

        if owner != stored_owner {
            return Err(Error::RegistrationFailed);
        }

        let registry = RegistryClient::new(&env, &registry_address);
        let self_address = env.current_contract_address();

        registry.update_metadata(&owner, &self_address, &new_name, &new_description);

        Ok(())
    }
}

/// Storage keys for this contract.
#[contracttype]
#[derive(Clone)]
enum DataKey {
    /// The address of the registry this contract registered with.
    RegistryAddress,
    /// The owner of our registry entry.
    RegistryOwner,
    /// Whether we've successfully registered.
    Registered,
}
