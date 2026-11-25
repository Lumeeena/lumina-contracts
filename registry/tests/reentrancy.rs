// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Reentrancy guard tests across every external-call path.
//!
//! Checks-effects-interactions (CEI) ordering is the primary defence against
//! reentrancy, and `ReentrancyGuard` complements — does not replace — it by
//! making the protection explicit at every entrypoint that calls out to an
//! external token contract:
//!
//! 1. `withdraw_stake` (`token::Client::transfer`)
//! 2. `stake` (`token::Client::transfer`)
//! 3. `register_contract` (`token::Client::transfer` when `RegistrationFee > 0`)
//! 4. `register_contracts` (`token::Client::transfer` when `RegistrationFee > 0`)
//! 5. `execute_proposal` (`ConfigureStaking` -> `decimals`, `Slash` -> `balance`
//!    and `transfer`, `WithdrawFromTreasury` -> `transfer`, and `Batch`
//!    proposals combining them)
//!
//! Each path is tested in two ways using a malicious `ReentrantToken` contract:
//!
//! - **`AttackMode::CrossContract`**: the token performs a real cross-contract
//!   `try_*` call back into the registry during its callback, confirming that a
//!   reentrant call is rejected and leaves state/solvency intact.
//! - **`AttackMode::ContractGuard`**: while the token callback is live on the
//!   stack, the token re-enters the registry's Rust entrypoint directly via
//!   `env.as_contract`, bypassing Soroban's host-level `ContractReentryMode`
//!   check so the contract's own `ReentrancyGuard` is exercised directly. This
//!   asserts that `DataKey::ReentrancyLock` is held during the external call,
//!   that re-entry returns `Err(RegistryError::ReentrantCall)`, and that a
//!   rejected reentrant call does not clear the outer lock. Removing
//!   `ReentrancyGuard::enter` from any guarded entrypoint fails these tests.

use lumina_registry::{
    Category, DataKey, LuminaRegistry, LuminaRegistryClient, ProposalAction, RegistrationEntry,
    RegistryError, TIMELOCK_LEDGERS,
};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, String, Vec};

// ─── Malicious SEP-41 token used to test reentrancy ─────────────────────────

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum AttackTrigger {
    Transfer,
    Balance,
    Decimals,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum AttackMode {
    CrossContract,
    ContractGuard,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum AttackTarget {
    WithdrawStake(Address, Address),
    Stake(Address, Address, i128),
    RegisterContract(Address, Address),
    RegisterContracts(Address, Address),
    ExecuteProposal(Address, u32),
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct AttackConfig {
    pub registry: Address,
    pub trigger: AttackTrigger,
    pub mode: AttackMode,
    pub target: AttackTarget,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct AttackReport {
    pub attempts: u32,
    pub rejected: u32,
    pub lock_seen_before: bool,
    pub lock_seen_after: bool,
    pub guard_error_matched: bool,
}

#[contracttype]
enum TokenKey {
    Balance(Address),
    Attack,
    Report,
    Failing,
}

#[contract]
pub struct ReentrantToken;

#[contractimpl]
impl ReentrantToken {
    pub fn decimals(env: Env) -> u32 {
        Self::maybe_reenter(&env, AttackTrigger::Decimals);
        7
    }

    pub fn mint(env: Env, to: Address, amount: i128) {
        let key = TokenKey::Balance(to);
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(current + amount));
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        Self::maybe_reenter(&env, AttackTrigger::Balance);
        env.storage()
            .persistent()
            .get(&TokenKey::Balance(id))
            .unwrap_or(0)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        if env
            .storage()
            .instance()
            .get(&TokenKey::Failing)
            .unwrap_or(false)
        {
            panic!("transfer refused: token forced failure");
        }

        Self::maybe_reenter(&env, AttackTrigger::Transfer);

        let from_key = TokenKey::Balance(from);
        let from_balance: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
        if from_balance < amount {
            panic!("transfer refused: insufficient balance");
        }
        env.storage()
            .persistent()
            .set(&from_key, &(from_balance - amount));
        Self::mint(env, to, amount);
    }

    pub fn set_attack(env: Env, config: AttackConfig) {
        env.storage().instance().set(&TokenKey::Attack, &config);
        env.storage().instance().set(
            &TokenKey::Report,
            &AttackReport {
                attempts: 0,
                rejected: 0,
                lock_seen_before: false,
                lock_seen_after: false,
                guard_error_matched: false,
            },
        );
    }

    pub fn clear_attack(env: Env) {
        env.storage().instance().remove(&TokenKey::Attack);
    }

    pub fn set_failing(env: Env, failing: bool) {
        env.storage().instance().set(&TokenKey::Failing, &failing);
    }

    pub fn get_report(env: Env) -> AttackReport {
        env.storage()
            .instance()
            .get(&TokenKey::Report)
            .unwrap_or(AttackReport {
                attempts: 0,
                rejected: 0,
                lock_seen_before: false,
                lock_seen_after: false,
                guard_error_matched: false,
            })
    }
}

impl ReentrantToken {
    fn maybe_reenter(env: &Env, trigger: AttackTrigger) {
        let Some(config) = env
            .storage()
            .instance()
            .get::<TokenKey, AttackConfig>(&TokenKey::Attack)
        else {
            return;
        };
        if config.trigger != trigger {
            return;
        }

        let mut report = Self::get_report(env.clone());
        // Fire once per armed attack so an unguarded regression fails an
        // assertion cleanly instead of recursing until stack exhaustion.
        if report.attempts > 0 {
            return;
        }
        report.attempts += 1;
        env.storage().instance().set(&TokenKey::Report, &report);

        match config.mode {
            AttackMode::CrossContract => {
                let registry = LuminaRegistryClient::new(env, &config.registry);
                let rejected = match &config.target {
                    AttackTarget::WithdrawStake(staker, contract_id) => {
                        registry.try_withdraw_stake(staker, contract_id).is_err()
                    }
                    AttackTarget::Stake(staker, contract_id, amount) => {
                        registry.try_stake(staker, contract_id, amount).is_err()
                    }
                    AttackTarget::RegisterContract(owner, contract_id) => registry
                        .try_register_contract(
                            owner,
                            contract_id,
                            &String::from_str(env, "Reentrant"),
                            &String::from_str(env, "Cross-contract reentrant registration"),
                            &Vec::from_slice(env, &[Category::Other]),
                        )
                        .is_err(),
                    AttackTarget::RegisterContracts(owner, contract_id) => {
                        let mut entries = Vec::new(env);
                        entries.push_back(RegistrationEntry {
                            contract_id: contract_id.clone(),
                            name: String::from_str(env, "Reentrant Batch"),
                            description: String::from_str(env, "Cross-contract reentrant batch"),
                            categories: Vec::from_slice(env, &[Category::Other]),
                        });
                        registry.try_register_contracts(owner, &entries).is_err()
                    }
                    AttackTarget::ExecuteProposal(executor, proposal_id) => registry
                        .try_execute_proposal(executor, proposal_id)
                        .is_err(),
                };
                if rejected {
                    report.rejected += 1;
                }
            }
            AttackMode::ContractGuard => {
                let (lock_before, matched, lock_after) = env.as_contract(&config.registry, || {
                    let lock_before = env.storage().temporary().has(&DataKey::ReentrancyLock);
                    let res: Result<(), RegistryError> = match &config.target {
                        AttackTarget::WithdrawStake(staker, contract_id) => {
                            LuminaRegistry::withdraw_stake(
                                env.clone(),
                                staker.clone(),
                                contract_id.clone(),
                            )
                            .map(|_| ())
                        }
                        AttackTarget::Stake(staker, contract_id, amount) => LuminaRegistry::stake(
                            env.clone(),
                            staker.clone(),
                            contract_id.clone(),
                            *amount,
                        ),
                        AttackTarget::RegisterContract(owner, contract_id) => {
                            LuminaRegistry::register_contract(
                                env.clone(),
                                owner.clone(),
                                contract_id.clone(),
                                String::from_str(env, "Reentrant"),
                                String::from_str(env, "Direct reentrant registration"),
                                Vec::from_slice(env, &[Category::Other]),
                            )
                        }
                        AttackTarget::RegisterContracts(owner, contract_id) => {
                            let mut entries = Vec::new(env);
                            entries.push_back(RegistrationEntry {
                                contract_id: contract_id.clone(),
                                name: String::from_str(env, "Reentrant Batch"),
                                description: String::from_str(env, "Direct reentrant batch"),
                                categories: Vec::from_slice(env, &[Category::Other]),
                            });
                            LuminaRegistry::register_contracts(env.clone(), owner.clone(), entries)
                        }
                        AttackTarget::ExecuteProposal(executor, proposal_id) => {
                            LuminaRegistry::execute_proposal(
                                env.clone(),
                                executor.clone(),
                                *proposal_id,
                            )
                        }
                    };
                    let lock_after = env.storage().temporary().has(&DataKey::ReentrancyLock);
                    (
                        lock_before,
                        res == Err(RegistryError::ReentrantCall),
                        lock_after,
                    )
                });
                report.lock_seen_before = lock_before;
                report.lock_seen_after = lock_after;
                report.guard_error_matched = matched;
                if matched {
                    report.rejected += 1;
                }
            }
        }

        env.storage().instance().set(&TokenKey::Report, &report);
    }
}

// ─── Test helpers ───────────────────────────────────────────────────────────

fn setup_env() -> (Env, LuminaRegistryClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();
    env.ledger().with_mut(|info| {
        info.min_persistent_entry_ttl = TIMELOCK_LEDGERS * 32;
        info.min_temp_entry_ttl = TIMELOCK_LEDGERS * 32;
        info.max_entry_ttl = TIMELOCK_LEDGERS * 64;
    });
    let admin = Address::generate(&env);
    let registry_id = env.register(LuminaRegistry, (&admin,));
    env.as_contract(&registry_id, || {
        env.storage()
            .instance()
            .extend_ttl(TIMELOCK_LEDGERS * 48, TIMELOCK_LEDGERS * 64);
    });
    let client = LuminaRegistryClient::new(&env, &registry_id);
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

fn setup_reentrant_staking() -> (
    Env,
    LuminaRegistryClient<'static>,
    Address,
    ReentrantTokenClient<'static>,
    Address,
) {
    let (env, client, admin) = setup_env();
    let token_id = env.register(ReentrantToken, ());
    let token = ReentrantTokenClient::new(&env, &token_id);
    let treasury = Address::generate(&env);

    let pid = client.propose_configure_staking(&admin, &token_id, &treasury);
    pass_proposal(&env, &client, &admin, pid);

    (env, client, admin, token, treasury)
}

fn register_sample(env: &Env, client: &LuminaRegistryClient) -> (Address, Address) {
    let owner = Address::generate(env);
    let target = Address::generate(env);
    client.register_contract(
        &owner,
        &target,
        &String::from_str(env, "Sample"),
        &String::from_str(env, "Sample contract"),
        &Vec::from_slice(env, &[Category::DeFi]),
    );
    (owner, target)
}

fn assert_lock_cleared(env: &Env, registry: &Address) {
    let still_locked = env.as_contract(registry, || {
        env.storage().temporary().has(&DataKey::ReentrancyLock)
    });
    assert!(
        !still_locked,
        "DataKey::ReentrancyLock must be cleared after the call finishes"
    );
}

fn assert_contract_guard_fired(report: &AttackReport) {
    assert_eq!(report.attempts, 1, "reentrant callback must have fired");
    assert!(
        report.lock_seen_before,
        "reentrancy lock must be held during the external token call"
    );
    assert!(
        report.guard_error_matched,
        "reentrant call must be rejected with RegistryError::ReentrantCall"
    );
    assert!(
        report.lock_seen_after,
        "a rejected reentrant call must not clear the outer invocation's lock"
    );
    assert_eq!(report.rejected, 1);
}

// ─── 1. withdraw_stake ──────────────────────────────────────────────────────

#[test]
fn withdraw_stake_rejects_reentrant_token_cross_contract() {
    let (env, client, _admin, token, _treasury) = setup_reentrant_staking();
    let (owner, target) = register_sample(&env, &client);
    // Fund registry with extra tokens so a double-withdraw would have succeeded
    // if reentrancy were not blocked.
    token.mint(&owner, &500);
    token.mint(&client.address, &500);
    client.stake(&owner, &target, &500);
    client.deactivate(&owner, &target);

    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Transfer,
        mode: AttackMode::CrossContract,
        target: AttackTarget::WithdrawStake(owner.clone(), target.clone()),
    });

    let withdrawn = client.withdraw_stake(&owner, &target);
    assert_eq!(withdrawn, 500);

    let report = token.get_report();
    assert_eq!(report.attempts, 1);
    assert_eq!(report.rejected, 1);
    assert_eq!(token.balance(&owner), 500);
    assert_eq!(client.get_stake(&target), 0);
    assert_lock_cleared(&env, &client.address);
}

#[test]
fn withdraw_stake_holds_guard_and_rejects_same_and_cross_entrypoint_reentry() {
    let (env, client, _admin, token, _treasury) = setup_reentrant_staking();
    let (owner, target) = register_sample(&env, &client);
    token.mint(&owner, &1_000);
    client.stake(&owner, &target, &500);
    client.deactivate(&owner, &target);

    // Same-entrypoint reentry: withdraw_stake -> transfer -> withdraw_stake.
    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Transfer,
        mode: AttackMode::ContractGuard,
        target: AttackTarget::WithdrawStake(owner.clone(), target.clone()),
    });
    assert_eq!(client.withdraw_stake(&owner, &target), 500);
    assert_contract_guard_fired(&token.get_report());
    assert_eq!(token.balance(&owner), 1_000);
    assert_eq!(client.get_stake(&target), 0);
    assert_lock_cleared(&env, &client.address);

    // Cross-entrypoint reentry: withdraw_stake -> transfer -> stake.
    let (owner2, target2) = register_sample(&env, &client);
    token.clear_attack();
    token.mint(&owner2, &600);
    client.stake(&owner2, &target2, &400);
    client.deactivate(&owner2, &target2);

    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Transfer,
        mode: AttackMode::ContractGuard,
        target: AttackTarget::Stake(owner2.clone(), target2.clone(), 100),
    });
    assert_eq!(client.withdraw_stake(&owner2, &target2), 400);
    assert_contract_guard_fired(&token.get_report());
    assert_eq!(client.get_stake(&target2), 0);
    assert_lock_cleared(&env, &client.address);
}

// ─── 2. stake ───────────────────────────────────────────────────────────────

#[test]
fn stake_rejects_reentrant_token_cross_contract_and_via_contract_guard() {
    let (env, client, _admin, token, _treasury) = setup_reentrant_staking();
    let (owner, target) = register_sample(&env, &client);
    token.mint(&owner, &1_000);

    // Real cross-contract reentry during stake transfer.
    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Transfer,
        mode: AttackMode::CrossContract,
        target: AttackTarget::Stake(owner.clone(), target.clone(), 200),
    });
    client.stake(&owner, &target, &300);
    let report = token.get_report();
    assert_eq!(report.attempts, 1);
    assert_eq!(report.rejected, 1);
    assert_eq!(client.get_stake(&target), 300);
    assert_eq!(token.balance(&client.address), 300);
    assert_lock_cleared(&env, &client.address);

    // Direct guard verification during stake transfer.
    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Transfer,
        mode: AttackMode::ContractGuard,
        target: AttackTarget::Stake(owner.clone(), target.clone(), 200),
    });
    client.stake(&owner, &target, &200);
    assert_contract_guard_fired(&token.get_report());
    assert_eq!(client.get_stake(&target), 500);
    assert_eq!(token.balance(&client.address), 500);
    assert_lock_cleared(&env, &client.address);
}

// ─── 3. register_contract (fee transfer) ────────────────────────────────────

#[test]
fn register_contract_rejects_reentrant_fee_token() {
    let (env, client, admin, token, treasury) = setup_reentrant_staking();
    let fee_pid = client.propose_set_registration_fee(&admin, &100);
    pass_proposal(&env, &client, &admin, fee_pid);

    let owner = Address::generate(&env);
    token.mint(&owner, &1_000);
    let target1 = Address::generate(&env);
    let reentrant_target = Address::generate(&env);

    // Real cross-contract reentry during registration fee transfer.
    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Transfer,
        mode: AttackMode::CrossContract,
        target: AttackTarget::RegisterContract(owner.clone(), reentrant_target.clone()),
    });
    client.register_contract(
        &owner,
        &target1,
        &String::from_str(&env, "First"),
        &String::from_str(&env, "Paid registration"),
        &Vec::from_slice(&env, &[Category::Infrastructure]),
    );
    let report = token.get_report();
    assert_eq!(report.attempts, 1);
    assert_eq!(report.rejected, 1);
    assert!(client.is_registered(&target1));
    assert!(!client.is_registered(&reentrant_target));
    assert_eq!(token.balance(&treasury), 100);
    assert_lock_cleared(&env, &client.address);

    // Direct guard verification during registration fee transfer (testing both
    // same-entrypoint and cross-entrypoint reentry into `register_contracts`).
    let target2 = Address::generate(&env);
    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Transfer,
        mode: AttackMode::ContractGuard,
        target: AttackTarget::RegisterContracts(owner.clone(), reentrant_target.clone()),
    });
    client.register_contract(
        &owner,
        &target2,
        &String::from_str(&env, "Second"),
        &String::from_str(&env, "Paid registration"),
        &Vec::from_slice(&env, &[Category::Infrastructure]),
    );
    assert_contract_guard_fired(&token.get_report());
    assert!(client.is_registered(&target2));
    assert!(!client.is_registered(&reentrant_target));
    assert_eq!(token.balance(&treasury), 200);
    assert_lock_cleared(&env, &client.address);
}

// ─── 4. register_contracts (batch fee transfer) ─────────────────────────────

#[test]
fn register_contracts_rejects_reentrant_fee_token() {
    let (env, client, admin, token, treasury) = setup_reentrant_staking();
    let fee_pid = client.propose_set_registration_fee(&admin, &50);
    pass_proposal(&env, &client, &admin, fee_pid);

    let owner = Address::generate(&env);
    token.mint(&owner, &1_000);
    let batch_c1 = Address::generate(&env);
    let batch_c2 = Address::generate(&env);
    let reentrant_target = Address::generate(&env);

    let mut entries1 = Vec::new(&env);
    entries1.push_back(RegistrationEntry {
        contract_id: batch_c1.clone(),
        name: String::from_str(&env, "Batch 1"),
        description: String::from_str(&env, "Batch entry 1"),
        categories: Vec::from_slice(&env, &[Category::DeFi]),
    });

    // Real cross-contract reentry during batch fee transfer.
    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Transfer,
        mode: AttackMode::CrossContract,
        target: AttackTarget::RegisterContracts(owner.clone(), reentrant_target.clone()),
    });
    client.register_contracts(&owner, &entries1);
    let report = token.get_report();
    assert_eq!(report.attempts, 1);
    assert_eq!(report.rejected, 1);
    assert!(client.is_registered(&batch_c1));
    assert!(!client.is_registered(&reentrant_target));
    assert_eq!(token.balance(&treasury), 50);
    assert_lock_cleared(&env, &client.address);

    // Direct guard verification during batch fee transfer.
    let mut entries2 = Vec::new(&env);
    entries2.push_back(RegistrationEntry {
        contract_id: batch_c2.clone(),
        name: String::from_str(&env, "Batch 2"),
        description: String::from_str(&env, "Batch entry 2"),
        categories: Vec::from_slice(&env, &[Category::Payments]),
    });
    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Transfer,
        mode: AttackMode::ContractGuard,
        target: AttackTarget::RegisterContract(owner.clone(), reentrant_target.clone()),
    });
    client.register_contracts(&owner, &entries2);
    assert_contract_guard_fired(&token.get_report());
    assert!(client.is_registered(&batch_c2));
    assert!(!client.is_registered(&reentrant_target));
    assert_eq!(token.balance(&treasury), 100);
    assert_lock_cleared(&env, &client.address);
}

// ─── 5. execute_proposal (Slash, WithdrawFromTreasury, ConfigureStaking, Batch)

#[test]
fn execute_proposal_slash_rejects_reentry_on_balance_and_transfer() {
    let (env, client, admin, token, treasury) = setup_reentrant_staking();
    let (owner, target) = register_sample(&env, &client);
    token.mint(&owner, &1_000);
    client.stake(&owner, &target, &1_000);
    // Deactivate so a reentrant `withdraw_stake` would otherwise be eligible
    // before the slash writes the lock window.
    client.deactivate(&owner, &target);

    // Reentry during `Slash`'s `token.balance()` solvency check.
    let pid_balance =
        client.propose_slash(&admin, &target, &200, &String::from_str(&env, "slash 1"));
    client.approve_proposal(&admin, &pid_balance);
    advance_ledger(&env, TIMELOCK_LEDGERS);

    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Balance,
        mode: AttackMode::ContractGuard,
        target: AttackTarget::WithdrawStake(owner.clone(), target.clone()),
    });
    client.execute_proposal(&admin, &pid_balance);
    assert_contract_guard_fired(&token.get_report());
    assert_eq!(client.get_stake(&target), 800);
    assert_eq!(token.balance(&treasury), 200);
    assert_lock_cleared(&env, &client.address);

    // Reentry during `Slash`'s `token.transfer()` — both cross-contract and
    // direct guard check attempting to re-execute the same proposal and to
    // withdraw stake.
    let pid_transfer_cc =
        client.propose_slash(&admin, &target, &150, &String::from_str(&env, "slash 2"));
    client.approve_proposal(&admin, &pid_transfer_cc);
    advance_ledger(&env, TIMELOCK_LEDGERS);

    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Transfer,
        mode: AttackMode::CrossContract,
        target: AttackTarget::ExecuteProposal(admin.clone(), pid_transfer_cc),
    });
    client.execute_proposal(&admin, &pid_transfer_cc);
    let report = token.get_report();
    assert_eq!(report.attempts, 1);
    assert_eq!(report.rejected, 1);
    assert_eq!(client.get_stake(&target), 650);
    assert_eq!(token.balance(&treasury), 350);
    assert_lock_cleared(&env, &client.address);

    let pid_transfer_guard =
        client.propose_slash(&admin, &target, &150, &String::from_str(&env, "slash 3"));
    client.approve_proposal(&admin, &pid_transfer_guard);
    advance_ledger(&env, TIMELOCK_LEDGERS);

    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Transfer,
        mode: AttackMode::ContractGuard,
        target: AttackTarget::ExecuteProposal(admin.clone(), pid_transfer_guard),
    });
    client.execute_proposal(&admin, &pid_transfer_guard);
    assert_contract_guard_fired(&token.get_report());
    assert_eq!(client.get_stake(&target), 500);
    assert_eq!(token.balance(&treasury), 500);
    assert_eq!(token.balance(&client.address), 500);
    assert_lock_cleared(&env, &client.address);
}

#[test]
fn execute_proposal_withdraw_from_treasury_rejects_reentry() {
    let (env, client, admin, token, treasury) = setup_reentrant_staking();
    token.mint(&treasury, &1_000);

    let pid1 = client.propose_withdraw_from_treasury(&admin, &300);
    client.approve_proposal(&admin, &pid1);
    advance_ledger(&env, TIMELOCK_LEDGERS);

    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Transfer,
        mode: AttackMode::CrossContract,
        target: AttackTarget::ExecuteProposal(admin.clone(), pid1),
    });
    client.execute_proposal(&admin, &pid1);
    let report = token.get_report();
    assert_eq!(report.attempts, 1);
    assert_eq!(report.rejected, 1);
    assert_eq!(token.balance(&treasury), 700);
    assert_eq!(token.balance(&client.address), 300);
    assert_lock_cleared(&env, &client.address);

    let pid2 = client.propose_withdraw_from_treasury(&admin, &200);
    client.approve_proposal(&admin, &pid2);
    advance_ledger(&env, TIMELOCK_LEDGERS);

    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Transfer,
        mode: AttackMode::ContractGuard,
        target: AttackTarget::ExecuteProposal(admin.clone(), pid2),
    });
    client.execute_proposal(&admin, &pid2);
    assert_contract_guard_fired(&token.get_report());
    assert_eq!(token.balance(&treasury), 500);
    assert_eq!(token.balance(&client.address), 500);
    assert_lock_cleared(&env, &client.address);
}

#[test]
fn execute_proposal_configure_staking_rejects_reentry_on_decimals() {
    let (env, client, admin) = setup_env();
    let token_id = env.register(ReentrantToken, ());
    let token = ReentrantTokenClient::new(&env, &token_id);
    let treasury = Address::generate(&env);

    let pid = client.propose_configure_staking(&admin, &token_id, &treasury);
    client.approve_proposal(&admin, &pid);
    advance_ledger(&env, TIMELOCK_LEDGERS);

    // Direct guard check when `ConfigureStaking` calls `token.decimals()`.
    let owner = Address::generate(&env);
    let target = Address::generate(&env);
    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Decimals,
        mode: AttackMode::ContractGuard,
        target: AttackTarget::RegisterContract(owner.clone(), target.clone()),
    });
    client.execute_proposal(&admin, &pid);
    assert_contract_guard_fired(&token.get_report());
    assert!(!client.is_registered(&target));
    assert_eq!(client.get_staking_config(), (token_id.clone(), treasury));
    assert_lock_cleared(&env, &client.address);

    // Cross-contract check on a second ConfigureStaking proposal.
    let treasury2 = Address::generate(&env);
    let pid2 = client.propose_configure_staking(&admin, &token_id, &treasury2);
    client.approve_proposal(&admin, &pid2);
    advance_ledger(&env, TIMELOCK_LEDGERS);

    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Decimals,
        mode: AttackMode::CrossContract,
        target: AttackTarget::ExecuteProposal(admin.clone(), pid2),
    });
    client.execute_proposal(&admin, &pid2);
    let report = token.get_report();
    assert_eq!(report.attempts, 1);
    assert_eq!(report.rejected, 1);
    assert_eq!(client.get_staking_config(), (token_id, treasury2));
    assert_lock_cleared(&env, &client.address);
}

#[test]
fn execute_proposal_batch_with_multiple_token_calls_does_not_self_deadlock() {
    let (env, client, admin, token, treasury) = setup_reentrant_staking();
    let (owner, target) = register_sample(&env, &client);
    token.mint(&owner, &1_000);
    token.mint(&treasury, &500);
    client.stake(&owner, &target, &1_000);

    // A batch containing two token-calling actions must execute cleanly while
    // still rejecting any reentrant token callback.
    let mut actions = Vec::new(&env);
    actions.push_back(ProposalAction::Slash(
        target.clone(),
        250,
        String::from_str(&env, "batch slash"),
    ));
    actions.push_back(ProposalAction::WithdrawFromTreasury(200));

    let pid = client.propose_batch(&admin, &actions);
    client.approve_proposal(&admin, &pid);
    advance_ledger(&env, TIMELOCK_LEDGERS);

    token.set_attack(&AttackConfig {
        registry: client.address.clone(),
        trigger: AttackTrigger::Transfer,
        mode: AttackMode::ContractGuard,
        target: AttackTarget::Stake(owner.clone(), target.clone(), 100),
    });
    client.execute_proposal(&admin, &pid);
    assert_contract_guard_fired(&token.get_report());
    assert_eq!(client.get_stake(&target), 750);
    assert_eq!(token.balance(&treasury), 550);
    assert_eq!(token.balance(&client.address), 950);
    assert_lock_cleared(&env, &client.address);
}

// ─── 6. Lock cleanup on errors and token panics ─────────────────────────────

#[test]
fn reentrancy_lock_is_released_after_errors_and_token_panics() {
    let (env, client, admin, token, _treasury) = setup_reentrant_staking();
    let (owner, target) = register_sample(&env, &client);
    token.mint(&owner, &1_000);

    // Early Err returns on every guarded entrypoint must not wedge the lock.
    assert_eq!(
        client.try_stake(&owner, &target, &0),
        Err(Ok(RegistryError::InvalidAmount))
    );
    assert_lock_cleared(&env, &client.address);

    assert_eq!(
        client.try_withdraw_stake(&owner, &target),
        Err(Ok(RegistryError::RegistrationActive))
    );
    assert_lock_cleared(&env, &client.address);

    assert_eq!(
        client.try_execute_proposal(&admin, &999),
        Err(Ok(RegistryError::ProposalNotFound))
    );
    assert_lock_cleared(&env, &client.address);

    assert_eq!(
        client.try_register_contract(
            &owner,
            &Address::generate(&env),
            &String::from_str(&env, ""),
            &String::from_str(&env, "empty name"),
            &Vec::from_slice(&env, &[Category::Other]),
        ),
        Err(Ok(RegistryError::InvalidMetadata))
    );
    assert_lock_cleared(&env, &client.address);

    let empty_batch: Vec<RegistrationEntry> = Vec::new(&env);
    assert_eq!(
        client.try_register_contracts(&owner, &empty_batch),
        Err(Ok(RegistryError::InvalidMetadata))
    );
    assert_lock_cleared(&env, &client.address);

    // A token panic mid-transfer must also leave the lock clear via rollback.
    token.set_failing(&true);
    assert!(client.try_stake(&owner, &target, &400).is_err());
    assert_lock_cleared(&env, &client.address);

    // Follow-up legitimate call succeeds once the token recovers.
    token.set_failing(&false);
    client.stake(&owner, &target, &400);
    assert_eq!(client.get_stake(&target), 400);
    assert_lock_cleared(&env, &client.address);
}

// ─── 7. Exhaustive coverage check over every external token call site ───────

/// Ensures every external token call in `registry/src/lib.rs` is reachable only
/// through a guarded entrypoint, and that each guarded entrypoint acquires
/// `ReentrancyGuard::enter(&env)?`.
#[test]
fn every_external_token_call_path_is_guarded() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest_dir.join("src/lib.rs"))
        .expect("registry/src/lib.rs must be readable");
    let prod_code = source
        .split("mod test {")
        .next()
        .expect("registry/src/lib.rs should contain production code before `mod test {`");

    // Every entrypoint that calls out to a token (directly or via apply_action).
    let guarded_entrypoints = [
        "execute_proposal",
        "register_contract",
        "register_contracts",
        "stake",
        "withdraw_stake",
    ];

    // Verify each guarded entrypoint acquires ReentrancyGuard::enter(&env)?.
    for fn_name in guarded_entrypoints {
        let marker = std::format!("pub fn {fn_name}(");
        let start = prod_code
            .find(&marker)
            .unwrap_or_else(|| panic!("entrypoint `{fn_name}` not found in registry/src/lib.rs"));
        let snippet = &prod_code[start..(start + 600).min(prod_code.len())];
        assert!(
            snippet.contains("ReentrancyGuard::enter(&env)?"),
            "entrypoint `{fn_name}` must acquire `ReentrancyGuard::enter(&env)?`"
        );
    }

    // Walk every `token::Client::new` occurrence in production code and confirm
    // its enclosing function is either one of the guarded entrypoints or
    // `apply_action` (which is called only from `execute_proposal` and itself).
    let mut allowed_enclosing: std::vec::Vec<&str> = guarded_entrypoints.to_vec();
    allowed_enclosing.push("apply_action");

    let mut current_fn = "<top-level>";
    let mut token_call_count = 0usize;
    for line in prod_code.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("pub fn ")
            .or_else(|| trimmed.strip_prefix("fn "))
        {
            if let Some((name, _)) = rest.split_once('(') {
                current_fn = name.trim();
            }
        }
        if trimmed.contains("token::Client::new") {
            token_call_count += 1;
            assert!(
                allowed_enclosing.contains(&current_fn),
                "found external token call `token::Client::new` inside unguarded function `{current_fn}`"
            );
        }
    }
    assert!(
        token_call_count >= 8,
        "expected at least 8 token::Client::new call sites in production code, found {token_call_count}"
    );

    // Confirm `apply_action` is only invoked from `execute_proposal` or recursively
    // from `apply_action` (for `ProposalAction::Batch`).
    let mut current_fn = "<top-level>";
    for line in prod_code.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("pub fn ")
            .or_else(|| trimmed.strip_prefix("fn "))
        {
            if let Some((name, _)) = rest.split_once('(') {
                current_fn = name.trim();
            }
        }
        if trimmed.contains("Self::apply_action(") {
            assert!(
                current_fn == "execute_proposal" || current_fn == "apply_action",
                "`Self::apply_action` called from unguarded function `{current_fn}`"
            );
        }
    }
}
