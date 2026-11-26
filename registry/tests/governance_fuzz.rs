// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Property-based fuzz for governance operation sequences.
//!
//! The interesting bugs in a multi-admin system are not in individual
//! handlers but in the orderings nobody thought to write: approve then
//! remove the approver, change the threshold mid-approval, execute after a
//! removal invalidates a proposal's quorum. This test generates
//! randomised sequences of those operations, applies them, and asserts a
//! short list of invariants after every step.
//!
//! Invariants held across every reachable state:
//!
//! 1. The admin set is always non-empty — `get_admins()` returns at least
//!    `MIN_ADMINS` entries.
//! 2. The threshold is always ≥ 1 and ≤ total admin weight.
//! 3. An executed proposal is monotonic: once `executed == true`, no later
//!    operation flips it back.
//!
//! Scale is deliberately small (16 cases × 8 ops) so the test stays under
//! a second on CI. Soroban's `Env::default()` + `mock_all_auths` is enough
//! to drive full propose / approve / execute cycles.
//!
//! Issue #235: Fuzz governance operation sequences.

use proptest::prelude::*;
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env};
use std::collections::BTreeSet;

use lumina_registry::{LuminaRegistry, LuminaRegistryClient, TIMELOCK_LEDGERS};

/// Smallest admin count the contract allows. Keep in sync with `MIN_ADMINS`
/// in `registry/src/lib.rs`; the invariant check below reads from
/// `get_admins()`, so a divergence fails only if the contract itself
/// disagrees with this value.
const MIN_ADMINS: usize = 1;

/// Ops the fuzz driver can emit. Each maps to one governance proposal,
/// which is proposed, (maybe) approved, and (maybe) executed.
#[derive(Clone, Debug)]
enum Op {
    /// Propose adding a new admin at index `admin_seed`.
    ProposeAddAdmin { admin_seed: u8 },
    /// Propose removing the admin at position `idx` mod current count.
    ProposeRemoveAdmin { idx: u8 },
    /// Propose changing the threshold to `seed` mod total_weight + 1.
    ProposeChangeThreshold { seed: u8 },
    /// Have admin `idx` approve the most recent proposal (if any).
    Approve { idx: u8 },
    /// Execute the most recent proposal (if any and if ready).
    Execute,
    /// Advance the ledger by `TIMELOCK_LEDGERS`, matching what governance
    /// clients do in the real flow.
    AdvanceTimelock,
}

fn op_strategy() -> BoxedStrategy<Op> {
    prop_oneof![
        any::<u8>().prop_map(|admin_seed| Op::ProposeAddAdmin { admin_seed }),
        any::<u8>().prop_map(|idx| Op::ProposeRemoveAdmin { idx }),
        any::<u8>().prop_map(|seed| Op::ProposeChangeThreshold { seed }),
        any::<u8>().prop_map(|idx| Op::Approve { idx }),
        Just(Op::Execute),
        Just(Op::AdvanceTimelock),
    ]
    .boxed()
}

/// Driver state kept outside the registry so we know what proposal id to
/// approve / execute and so we can build fresh addresses deterministically
/// per sequence.
struct Driver {
    env: Env,
    client: LuminaRegistryClient<'static>,
    admins: Vec<Address>,
    /// The ID of the most recently proposed proposal, so `Approve` and
    /// `Execute` have a target.
    last_pid: Option<u32>,
    /// Pool of extra addresses the driver can promote to admin.
    pool: Vec<Address>,
    /// Pids of every proposal we have observed as executed, so we can
    /// assert they stay that way.
    once_executed: BTreeSet<u32>,
}

fn setup_driver() -> Driver {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|info| {
        info.min_persistent_entry_ttl = TIMELOCK_LEDGERS * 8;
        info.min_temp_entry_ttl = TIMELOCK_LEDGERS * 8;
        info.max_entry_ttl = TIMELOCK_LEDGERS * 64;
    });
    let admin = Address::generate(&env);
    let contract_id = env.register(LuminaRegistry, (&admin,));
    let client = LuminaRegistryClient::new(&env, &contract_id);
    // Pre-generate a pool so a full fuzz run uses a bounded number of
    // addresses — easier to reason about than generating on demand.
    let pool: Vec<Address> = (0..32).map(|_| Address::generate(&env)).collect();
    Driver {
        env,
        client,
        admins: vec![admin],
        last_pid: None,
        pool,
        once_executed: BTreeSet::new(),
    }
}

impl Driver {
    fn apply(&mut self, op: &Op) {
        match op {
            Op::ProposeAddAdmin { admin_seed } => {
                let proposer = self.admins[0].clone();
                let new_admin = self.pool[(*admin_seed as usize) % self.pool.len()].clone();
                if let Ok(Ok(pid)) = self.client.try_propose_add_admin(&proposer, &new_admin) {
                    self.last_pid = Some(pid);
                }
            }
            Op::ProposeRemoveAdmin { idx } => {
                let proposer = self.admins[0].clone();
                let target = self.admins[(*idx as usize) % self.admins.len()].clone();
                if let Ok(Ok(pid)) = self.client.try_propose_remove_admin(&proposer, &target) {
                    self.last_pid = Some(pid);
                }
            }
            Op::ProposeChangeThreshold { seed } => {
                let proposer = self.admins[0].clone();
                // Thresholds must be in 1..=total_weight. Each admin defaults to
                // weight 1 and this fuzz does not touch weights, so total weight
                // equals admin count.
                let target_threshold = ((*seed as u32) % (self.admins.len() as u32)) + 1;
                if let Ok(Ok(pid)) = self
                    .client
                    .try_propose_change_threshold(&proposer, &target_threshold)
                {
                    self.last_pid = Some(pid);
                }
            }
            Op::Approve { idx } => {
                if let Some(pid) = self.last_pid {
                    let approver = self.admins[(*idx as usize) % self.admins.len()].clone();
                    // Expected error cases — not approver, already approved, etc.
                    // — are swallowed silently so the fuzz keeps moving.
                    let _ = self.client.try_approve_proposal(&approver, &pid);
                }
            }
            Op::Execute => {
                if let Some(pid) = self.last_pid {
                    let executor = self.admins[0].clone();
                    let res = self.client.try_execute_proposal(&executor, &pid);
                    if res.is_ok() {
                        self.once_executed.insert(pid);
                        // Add/remove-admin success is reflected in the admin
                        // set; refresh our local view from the contract so
                        // later ops target the right addresses.
                        self.admins = self.client.get_admins().iter().collect();
                    }
                }
            }
            Op::AdvanceTimelock => {
                let seq = self.env.ledger().sequence();
                self.env.ledger().set_sequence_number(seq + TIMELOCK_LEDGERS);
            }
        }
    }

    fn assert_invariants(&self) {
        // Invariant 1: admin set never shrinks below the minimum.
        let admins = self.client.get_admins();
        assert!(
            admins.len() as usize >= MIN_ADMINS,
            "admin set dropped below MIN_ADMINS to {}",
            admins.len()
        );

        // Invariant 2: threshold is in 1..=total_admin_weight.
        let threshold = self.client.get_threshold();
        let total_weight = self.client.get_total_admin_weight();
        assert!(threshold >= 1, "threshold {threshold} dropped to zero");
        assert!(
            threshold <= total_weight,
            "threshold {threshold} exceeds total weight {total_weight}",
        );

        // Invariant 3: once a proposal is executed it stays executed.
        for pid in &self.once_executed {
            let p = self.client.get_proposal(pid);
            assert!(p.executed, "proposal {pid} un-executed between steps");
        }
    }
}

// ─── The property ────────────────────────────────────────────────────────────

proptest! {
    // Keep the shape small so this runs well under a second on CI.
    #![proptest_config(ProptestConfig {
        cases: 16,
        .. ProptestConfig::default()
    })]

    #[test]
    fn invariants_hold_across_random_governance_sequences(
        ops in proptest::collection::vec(op_strategy(), 1..9)
    ) {
        let mut driver = setup_driver();
        driver.assert_invariants();
        for op in &ops {
            driver.apply(op);
            driver.assert_invariants();
        }
    }
}
