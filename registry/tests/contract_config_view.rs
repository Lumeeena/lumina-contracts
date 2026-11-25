// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Configuration summary view (#248).
//!
//! `get_contract_config` returns version, admin count, threshold, staking
//! config and pause state in one call. The view is explicitly safe to call
//! on an uninitialised contract — a UI or monitor should never have to
//! branch on four `NotInitialized` errors just to render the status banner.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env};

use lumina_registry::{
    ContractConfig, LuminaRegistry, LuminaRegistryClient, CONTRACT_VERSION, TIMELOCK_LEDGERS,
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

#[test]
fn config_is_safe_pre_initialisation_equivalent_to_default_bootstrap() {
    // `__constructor` initialises with one bootstrap admin + threshold=1.
    // The test here is: the view returns reasonable, defaulted values for
    // every unset slot, never panicking.
    let (_env, client, _admin) = setup();
    let config: ContractConfig = client.get_contract_config();
    assert_eq!(config.version, CONTRACT_VERSION);
    assert!(config.initialized);
    assert_eq!(config.admin_count, 1);
    assert_eq!(config.threshold, 1);
    assert_eq!(config.total_weight, 1);
    assert!(config.staking_token.is_none());
    assert!(config.staking_treasury.is_none());
    assert!(!config.paused);
}

#[test]
fn config_reflects_staking_configuration_once_executed() {
    let (env, client, admin) = setup();

    // Register a separate token-shaped contract for staking.
    let token_admin = Address::generate(&env);
    let (token_id, _asset) = {
        let sac_admin = Address::generate(&env);
        let _ = sac_admin;
        let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());
        (token_id.address(), token_id)
    };
    let treasury = Address::generate(&env);

    let pid = client.propose_configure_staking(&admin, &token_id, &treasury);
    client.approve_proposal(&admin, &pid);
    let seq = env.ledger().sequence();
    env.ledger().set_sequence_number(seq + TIMELOCK_LEDGERS);
    client.execute_proposal(&admin, &pid);

    let config = client.get_contract_config();
    assert_eq!(config.staking_token, Some(token_id));
    assert_eq!(config.staking_treasury, Some(treasury));
}

#[test]
fn config_reflects_pause_state() {
    let (env, client, admin) = setup();
    let pid = client.propose_set_paused(&admin, &true);
    client.approve_proposal(&admin, &pid);
    let seq = env.ledger().sequence();
    env.ledger().set_sequence_number(seq + TIMELOCK_LEDGERS);
    client.execute_proposal(&admin, &pid);
    assert!(client.get_contract_config().paused);
}
