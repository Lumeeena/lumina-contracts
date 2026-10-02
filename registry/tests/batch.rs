// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT

use lumina_registry::{
    LuminaRegistry, LuminaRegistryClient, ProposalAction, RegistryError, MAX_BATCH_ACTIONS,
    TIMELOCK_LEDGERS,
};
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    token, Address, Env, IntoVal, Symbol, Vec,
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
    let id = env.register(LuminaRegistry, (&admin,));
    let client = LuminaRegistryClient::new(&env, &id);
    (env, client, admin)
}

fn actions(env: &Env, values: &[ProposalAction]) -> Vec<ProposalAction> {
    Vec::from_slice(env, values)
}

fn wait(env: &Env) {
    env.ledger()
        .with_mut(|info| info.sequence_number += TIMELOCK_LEDGERS);
}

#[test]
fn batch_rotates_admins_in_order_under_one_proposal() {
    let (env, client, admin) = setup();
    let replacement_a = Address::generate(&env);
    let replacement_b = Address::generate(&env);
    let batch = actions(
        &env,
        &[
            ProposalAction::AddAdmin(replacement_a.clone()),
            ProposalAction::AddAdmin(replacement_b.clone()),
            ProposalAction::RemoveAdmin(admin.clone()),
            ProposalAction::ChangeThreshold(2),
        ],
    );
    let pid = client.propose_batch(&admin, &batch);
    let event: (u32, Address, Symbol, u32) = env.events().all().last().unwrap().2.into_val(&env);
    assert_eq!(
        event,
        (pid, admin.clone(), Symbol::new(&env, "batch"), 4u32)
    );
    assert_eq!(
        client.get_proposal(&pid).action,
        ProposalAction::Batch(batch)
    );
    assert_eq!(client.get_admins(), Vec::from_slice(&env, &[admin.clone()]));
    client.approve_proposal(&admin, &pid);
    wait(&env);
    client.execute_proposal(&pid);

    assert_eq!(
        client.get_admins(),
        Vec::from_slice(&env, &[replacement_a, replacement_b])
    );
    assert_eq!(client.get_threshold(), 2);
    assert!(client.get_proposal(&pid).executed);
    assert_eq!(
        client.try_execute_proposal(&pid),
        Err(Ok(RegistryError::AlreadyExecuted))
    );
}

#[test]
fn failing_action_rolls_back_earlier_changes_and_execution_marker() {
    let (env, client, admin) = setup();
    let new_admin = Address::generate(&env);
    let pid = client.propose_batch(
        &admin,
        &actions(
            &env,
            &[
                ProposalAction::AddAdmin(new_admin.clone()),
                ProposalAction::SetRegistrationFee(50),
                // Invalid with two admins, but can succeed after adding a third.
                ProposalAction::ChangeThreshold(3),
                ProposalAction::ConfigureMinimumStake(100),
            ],
        ),
    );
    client.approve_proposal(&admin, &pid);
    wait(&env);
    let before = client.get_proposal(&pid);
    assert_eq!(
        client.try_execute_proposal(&pid),
        Err(Ok(RegistryError::InvalidThreshold))
    );
    assert_eq!(client.get_admins(), Vec::from_slice(&env, &[admin.clone()]));
    assert_eq!(client.get_threshold(), 1);
    assert_eq!(client.get_registration_fee(), 0);
    assert_eq!(client.get_minimum_stake(), 0);
    assert_eq!(client.get_proposal(&pid), before);

    let extra_admin = Address::generate(&env);
    let add = client.propose_add_admin(&admin, &extra_admin);
    client.approve_proposal(&admin, &add);
    wait(&env);
    client.execute_proposal(&add);
    client.execute_proposal(&pid);
    assert!(client.get_admins().contains(&new_admin));
    assert_eq!(client.get_threshold(), 3);
    assert_eq!(client.get_registration_fee(), 50);
    assert_eq!(client.get_minimum_stake(), 100);
    assert!(client.get_proposal(&pid).executed);
}

#[test]
fn failed_batch_rolls_back_cross_contract_token_transfer() {
    let (env, client, admin) = setup();
    let asset = env.register_stellar_asset_contract_v2(admin.clone());
    let token_id = asset.address();
    let treasury = Address::generate(&env);
    let config = client.propose_configure_staking(&admin, &token_id, &treasury);
    client.approve_proposal(&admin, &config);
    wait(&env);
    client.execute_proposal(&config);
    token::StellarAssetClient::new(&env, &token_id).mint(&treasury, &100);
    // execute_proposal is permissionless. The treasury separately authorizes
    // the nested token transfer, without a root require_auth invocation.
    env.mock_all_auths_allowing_non_root_auth();
    let pid = client.propose_batch(
        &admin,
        &actions(
            &env,
            &[
                ProposalAction::WithdrawFromTreasury(40),
                ProposalAction::ChangeThreshold(0),
            ],
        ),
    );
    client.approve_proposal(&admin, &pid);
    wait(&env);
    assert_eq!(
        client.try_execute_proposal(&pid),
        Err(Ok(RegistryError::InvalidThreshold))
    );
    let token = token::Client::new(&env, &token_id);
    assert_eq!(token.balance(&treasury), 100);
    assert_eq!(token.balance(&client.address), 0);
    assert!(!client.get_proposal(&pid).executed);
}

#[test]
fn maximum_length_batch_executes_every_action() {
    let (env, client, admin) = setup();
    let mut batch = Vec::new(&env);
    let mut added = Vec::new(&env);
    for _ in 0..MAX_BATCH_ACTIONS {
        let address = Address::generate(&env);
        batch.push_back(ProposalAction::AddAdmin(address.clone()));
        added.push_back(address);
    }
    let pid = client.propose_batch(&admin, &batch);
    client.approve_proposal(&admin, &pid);
    wait(&env);
    client.execute_proposal(&pid);
    let admins = client.get_admins();
    assert_eq!(admins.len(), MAX_BATCH_ACTIONS + 1);
    for address in added.iter() {
        assert!(admins.contains(&address));
    }
}

#[test]
fn rejects_empty_oversized_and_nested_batches_without_allocating_proposals() {
    let (env, client, admin) = setup();
    assert_eq!(
        client.try_propose_batch(&admin, &Vec::new(&env)),
        Err(Ok(RegistryError::InvalidBatchSize))
    );
    let mut oversized = Vec::new(&env);
    for _ in 0..=MAX_BATCH_ACTIONS {
        oversized.push_back(ProposalAction::SetRegistrationFee(1));
    }
    assert_eq!(
        client.try_propose_batch(&admin, &oversized),
        Err(Ok(RegistryError::InvalidBatchSize))
    );
    for inner in [
        Vec::new(&env),
        actions(&env, &[ProposalAction::SetRegistrationFee(1)]),
    ] {
        assert_eq!(
            client.try_propose_batch(
                &admin,
                &actions(
                    &env,
                    &[
                        ProposalAction::SetRegistrationFee(2),
                        ProposalAction::Batch(inner),
                    ],
                ),
            ),
            Err(Ok(RegistryError::NestedBatch))
        );
    }
    assert_eq!(
        client.propose_batch(
            &admin,
            &actions(&env, &[ProposalAction::SetRegistrationFee(1)])
        ),
        0
    );
}

#[test]
fn batch_obeys_approval_threshold_and_timelock() {
    let (env, client, admin) = setup();
    let pid = client.propose_batch(
        &admin,
        &actions(&env, &[ProposalAction::SetRegistrationFee(10)]),
    );
    assert_eq!(
        client.try_execute_proposal(&pid),
        Err(Ok(RegistryError::ThresholdNotMet))
    );
    client.approve_proposal(&admin, &pid);
    assert_eq!(
        client.try_execute_proposal(&pid),
        Err(Ok(RegistryError::TimelockNotElapsed))
    );
    assert_eq!(client.get_registration_fee(), 0);
    wait(&env);
    client.execute_proposal(&pid);
    assert_eq!(client.get_registration_fee(), 10);
}

#[test]
fn batch_requires_the_full_multisig_threshold() {
    let (env, client, admin) = setup();
    let second_admin = Address::generate(&env);
    let configure = client.propose_batch(
        &admin,
        &actions(
            &env,
            &[
                ProposalAction::AddAdmin(second_admin.clone()),
                ProposalAction::ChangeThreshold(2),
            ],
        ),
    );
    client.approve_proposal(&admin, &configure);
    wait(&env);
    client.execute_proposal(&configure);
    let pid = client.propose_batch(
        &admin,
        &actions(&env, &[ProposalAction::SetRegistrationFee(10)]),
    );
    client.approve_proposal(&admin, &pid);
    wait(&env);
    assert_eq!(
        client.try_execute_proposal(&pid),
        Err(Ok(RegistryError::ThresholdNotMet))
    );
    client.approve_proposal(&second_admin, &pid);
    assert_eq!(
        client.try_execute_proposal(&pid),
        Err(Ok(RegistryError::TimelockNotElapsed))
    );
    wait(&env);
    client.execute_proposal(&pid);
    assert_eq!(client.get_registration_fee(), 10);
}

#[test]
fn batch_requires_an_admin_proposer() {
    let (env, client, _) = setup();
    let stranger = Address::generate(&env);
    assert_eq!(
        client.try_propose_batch(
            &stranger,
            &actions(&env, &[ProposalAction::SetRegistrationFee(1)]),
        ),
        Err(Ok(RegistryError::NotAdmin))
    );
}

#[test]
fn batch_requires_the_proposers_signature() {
    let (env, client, admin) = setup();
    env.mock_auths(&[]);
    assert!(client
        .try_propose_batch(
            &admin,
            &actions(&env, &[ProposalAction::SetRegistrationFee(1)])
        )
        .is_err());
}
