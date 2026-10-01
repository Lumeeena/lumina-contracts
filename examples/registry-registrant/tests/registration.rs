// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT

//! Integration tests for registry self-registration.
//!
//! These tests deploy the registry, deploy the example contract with
//! self-registration, and verify the registration succeeded and is queryable.

#![cfg(test)]

use lumina_registry_interface::{Category, ContractEntry, RegistryInterfaceClient};
use soroban_sdk::{
    testutils::{Address as _, AuthorizedFunction, AuthorizedInvocation},
    Address, Env, IntoVal, String, Symbol, Vec,
};

// Import the registry wasm for deployment
mod registry_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/lumina_registry.wasm");
}

// Import the example contract wasm
mod example_wasm {
    soroban_sdk::contractimport!(
        file = "../../target/wasm32v1-none/release/lumina_registry_registrant_example.wasm"
    );
}

/// Test fixture with deployed registry and test accounts.
struct Fixture {
    env: Env,
    registry: Address,
    admin: Address,
    owner: Address,
}

impl Fixture {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let owner = Address::generate(&env);

        // Deploy the registry with the admin as bootstrap
        let registry = env.register(registry_wasm::WASM, (admin.clone() /* bootstrap_admin */,));

        // Initialize the registry with multi-sig governance
        let registry_client = registry_wasm::Client::new(&env, &registry);
        let admins = Vec::from_array(&env, [admin.clone()]);
        registry_client.initialize(&admins, &1u32);

        Self {
            env,
            registry,
            admin,
            owner,
        }
    }

    /// Deploy the example contract, which registers itself during deployment.
    fn deploy_example(&self, name: &str, description: &str, categories: &[Category]) -> Address {
        let name_str = String::from_str(&self.env, name);
        let desc_str = String::from_str(&self.env, description);
        let cats = Vec::from_slice(&self.env, categories);

        self.env.register(
            example_wasm::WASM,
            (
                self.registry.clone(),
                self.owner.clone(),
                name_str,
                desc_str,
                cats,
            ),
        )
    }

    fn registry_client(&self) -> RegistryInterfaceClient {
        RegistryInterfaceClient::new(&self.env, &self.registry)
    }

    fn example_client(&self, addr: &Address) -> example_wasm::Client {
        example_wasm::Client::new(&self.env, addr)
    }
}

#[test]
fn example_contract_registers_itself_on_deploy() {
    let f = Fixture::new();

    // Deploy the example contract - it should auto-register
    let example = f.deploy_example(
        "Example DeFi Protocol",
        "A demonstration protocol",
        &[Category::DeFi, Category::Payments],
    );

    // Verify the example contract knows it's registered
    let client = f.example_client(&example);
    assert!(client.is_registered());
    assert_eq!(client.get_registry(), f.registry);

    // Verify the registry has the entry
    let registry = f.registry_client();
    let contracts = registry.get_active_contracts(&0, &10);

    assert_eq!(contracts.len(), 1);
    let entry = contracts.get(0).unwrap();

    assert_eq!(entry.contract_id, example);
    assert_eq!(entry.owner, f.owner);
    assert_eq!(
        entry.name,
        String::from_str(&f.env, "Example DeFi Protocol")
    );
    assert!(entry.active);
}

#[test]
fn registration_appears_in_category_listings() {
    let f = Fixture::new();

    let example = f.deploy_example(
        "DeFi Protocol",
        "A DeFi example",
        &[Category::DeFi, Category::Payments],
    );

    let registry = f.registry_client();

    // Should appear in DeFi category
    let defi_contracts = registry.get_active_contracts_by_category(&Category::DeFi, &0, &10);
    assert_eq!(defi_contracts.len(), 1);
    assert_eq!(defi_contracts.get(0).unwrap().contract_id, example);

    // Should appear in Payments category
    let payment_contracts = registry.get_active_contracts_by_category(&Category::Payments, &0, &10);
    assert_eq!(payment_contracts.len(), 1);
    assert_eq!(payment_contracts.get(0).unwrap().contract_id, example);

    // Should NOT appear in other categories
    let nft_contracts = registry.get_active_contracts_by_category(&Category::Nft, &0, &10);
    assert_eq!(nft_contracts.len(), 0);
}

#[test]
fn owner_can_update_registration_metadata() {
    let f = Fixture::new();

    let example = f.deploy_example("Old Name", "Old description", &[Category::DeFi]);

    let client = f.example_client(&example);

    // Owner updates the metadata
    client.update_registry_metadata(
        &f.owner,
        &String::from_str(&f.env, "New Name"),
        &String::from_str(&f.env, "New description"),
    );

    // Verify the registry reflects the update
    let registry = f.registry_client();
    let contracts = registry.get_active_contracts(&0, &10);
    let entry = contracts.get(0).unwrap();

    assert_eq!(entry.name, String::from_str(&f.env, "New Name"));
    assert_eq!(
        entry.description,
        String::from_str(&f.env, "New description")
    );
}

#[test]
fn example_contract_business_logic_works() {
    let f = Fixture::new();

    let example = f.deploy_example("Protocol", "Description", &[Category::DeFi]);
    let client = f.example_client(&example);

    // Test the greeting function
    let greeting = client.greet(&String::from_str(&f.env, "World"));
    assert_eq!(greeting, String::from_str(&f.env, "Hello, World!"));

    // Test the swap function (mock)
    let user = Address::generate(&f.env);
    let token_in = Address::generate(&f.env);
    let token_out = Address::generate(&f.env);

    let amount_out = client.swap_tokens(&user, &token_in, &1000, &token_out, &950);
    assert_eq!(amount_out, 950);
}

#[test]
fn multiple_contracts_can_register() {
    let f = Fixture::new();

    // Deploy three different contracts
    let protocol_a = f.deploy_example("Protocol A", "First protocol", &[Category::DeFi]);
    let protocol_b = f.deploy_example("Protocol B", "Second protocol", &[Category::Payments]);
    let protocol_c = f.deploy_example("Protocol C", "Third protocol", &[Category::Infrastructure]);

    // All should be in the registry
    let registry = f.registry_client();
    let contracts = registry.get_active_contracts(&0, &10);

    assert_eq!(contracts.len(), 3);

    let ids: Vec<Address> = contracts.iter().map(|e| e.contract_id.clone()).collect();
    assert!(ids.contains(&protocol_a));
    assert!(ids.contains(&protocol_b));
    assert!(ids.contains(&protocol_c));
}

#[test]
fn registration_is_owned_by_specified_owner() {
    let f = Fixture::new();

    let example = f.deploy_example("Protocol", "Description", &[Category::DeFi]);

    // Verify the owner can access the registration
    let registry = f.registry_client();
    let owner_contracts = registry.get_contracts_by_owner(&f.owner, &0, &10);

    assert_eq!(owner_contracts.len(), 1);
    assert_eq!(owner_contracts.get(0).unwrap().contract_id, example);
    assert_eq!(owner_contracts.get(0).unwrap().owner, f.owner);
}

#[test]
#[should_panic(expected = "HostError: Error(Contract, #3)")]
fn cannot_register_same_contract_twice() {
    let f = Fixture::new();

    // First registration succeeds
    let example = f.deploy_example("Protocol", "Description", &[Category::DeFi]);

    // Try to register the same address again - should fail
    // (This would require calling register_contract directly, not via constructor)
    let registry = f.registry_client();
    registry.register_contract(
        &f.owner,
        &example,
        &String::from_str(&f.env, "Duplicate"),
        &String::from_str(&f.env, "Should fail"),
        &Vec::from_array(&f.env, [Category::DeFi]),
    );
}

#[test]
fn deployment_emits_event() {
    let f = Fixture::new();

    let example = f.deploy_example("Protocol", "Description", &[Category::DeFi]);

    // Check that the contract emitted its deployment event
    let events = f.env.events().all();
    let contract_events: Vec<_> = events.iter().filter(|e| e.0 == example).collect();

    // Should have emitted at least one event from the example contract
    assert!(!contract_events.is_empty());

    // The last event should be our custom "contract_deployed" event
    let last_event = contract_events.last().unwrap();
    let topics: Vec<soroban_sdk::Val> = last_event.1.clone().into();

    // Verify the event topic matches
    assert_eq!(topics.len(), 1);
}
