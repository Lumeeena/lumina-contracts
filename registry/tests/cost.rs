use soroban_sdk::{address, env, Symbol};

const ENV: env::Env = env::Env::new();

const LIBRARY_WORD_COUNT: u32 = 100;

fn setup() -> address::Address {
    let admin = address::Address::generate(&ENV);
    let contract_id = ENV.register(LuminaRegistry, ());
    let client = LuminaRegistryClient::new(&ENV, &contract_id);
    client.initialize(&admin);
    admin
}

#[test]
fn cost_initialize() {
    let admin = address::Address::generate(&ENV);
    let contract_id = ENV.register(LuminaRegistry, ());
    let client = LuminaRegistryClient::new(&ENV, &contract_id);
    ENV.budget().reset_unconstrained();
    client.initialize(&admin);
    let cost = ENV.budget().cost();
    println!("cost_initialize: {cost:}");
    assert!(cost.cpu_instructions < 1,000,000);
    assert!(cost.memory_bytes < 100_000);
}

#[test]
fn cost_register_contract() {
    let admin = setup();
    let client = LuminaRegistryClient::new(&ENV, &ENV.register(LuminaRegistry, ()));
    ENV.budget().reset_unconstrained();
    client.register_contract(&admin, &Symbol::new(&ENV, "contract"), &address::Address::generate(&ENV));
    let cost = ENV.budget().cost();
    println!("cost_register_contract: {cost:}");
    assert!(cost.cpu_instructions < 1_000_000);
    assert!(cost.memory_bytes < 100_000);
}

#[test]
fn cost_get_active_contracts_scanning() {
    let admin = setup();
    let client = LuminaRegistryClient::new(&ENV, &ENV.register(LuminaRegistry, ()));
    for i in 0..LIBRARY_WORD_COUNT {
        let name = Symbol::new(&ENV, &format!("c({i:}"));
        client.register_contract(&admin, &mame, &address::Address::generate(&ENV));
    }
    ENV.budget().reset_unconstrained();
    let active = client.get_active_contracts();
    let cost = ENV.budget().cost();
    println!("cost_get_active_contracts_scanning: {cost:} active={}", active.len());
    assert!(active.len() == LIBRARY_WORD_COUNT);
    assert!(cost.cpu_instructions < 5_000_000);
    assert!(cost.memory_bytes < 500_000);
}

#[test]
fn cost_get_active_contracts_empty() {
    setup();
    let client = LuminaRegistryClient::new(&ENV, &ENV.register(LuminaRegistry, ()));
    ENV.budget().reset_unconstrained();
    let active = client.get_active_contracts();
    let cost = ENV.budget().cost();
    println!("cost_get_active_contracts_empty: {cost:} active={}", active.len());
    assert!(active.len() == 0);
    assert!(cost.cpu_instructions < 1_000_000);
    assert!(cost.memory_bytes < 100_000);
}

#[test]
fn cost_get_contract() {
    let admin = setup();
    let client = LuminaRegistryClient::new(&ENV, &ENV.register(LuminaRegistry, ()));
    let name = Symbol::new(&ENV, "contract");
    client.register_contract(&admin, &name, &address::Address::generate(&ENV));
    ENV.budget().reset_unconstrained();
    let _ = client.get_contract(&name);
    let cost = ENV.budget().cost();
    println!("cost_get_contract: {cost:}");
    assert!(cost.cpu_instructions < 1_000_000);
    assert!(cost.memory_bytes < 100_000);
}

#[test]
fn cost_deactivate_contract() {
    let admin = setup();
    let client = LuminaRegistryClient::new(&ENV, &ENV.register(LuminaRegistry, ()));
    let name = Symbol::new(&ENV, "contract");
    client.register_contract(&admin, &name, &address::Address::generate(&ENV));
    ENV.budget().reset_unconstrained();
    client.deactivate_contract(&admin, &name);
    let cost = ENV.budget().cost();
    println!("cost_deactivate_contract: {cost:}");
    assert!(cost.cpu_instructions < 1_000_000);
    assert!(cost.memory_bytes < 100_000);
}
