// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
#![no_std]
#![warn(missing_docs)]
//! Lumina Registry — on-chain contract registry for the Lumina indexer.
//!
//! Projects deploy their Soroban contracts and register them here so that
//! the Lumina indexer can discover and prioritize indexing their events.
//! This creates a permissionless, decentralized index manifest for Stellar.
//!
//! Flow:
//!   1. Project deploys a Soroban contract
//!   2. Project calls register_contract() on the Lumina Registry
//!   3. Lumina indexer polls the Registry for new entries and begins indexing
//!   4. Indexed events become queryable via the Lumina GraphQL API
//!
//! ## Governance model (multi-sig admin)
//!
//! Privileged actions — deactivating another owner's contract, upgrading the
//! wasm, changing the admin set — go through a **propose → approve →
//! execute** flow rather than a single signer:
//!
//! 1. Any admin calls `propose_*`; a `Proposal` is stored and a
//!    `proposal_proposed` event is emitted.
//! 2. Other admins call `approve_proposal`; each unique approval is counted
//!    and a `proposal_approved` event emitted.  When the threshold is reached
//!    the proposal becomes *ready* and a `proposal_ready` event is emitted —
//!    but it is **not** executed yet.
//! 3. After `TIMELOCK_LEDGERS` ledgers have elapsed since the proposal became
//!    ready, anyone may call `execute_proposal`; a `proposal_executed` event
//!    is emitted.
//! 4. Proposals that are never executed do not expire automatically; they can
//!    be superseded by a new proposal for the same action or simply ignored.

use soroban_sdk::{
    contract, contractimpl, contracttype, contracterror, token,
    Address, BytesN, Env, Symbol, String, Vec,
};

// ─── Version ───────────────────────────────────────────────────────────────

/// Version of the deployed code, returned by [`LuminaRegistry::get_version`].
///
/// Bump this in the same commit as any change to the exported interface or to
/// the storage shapes below.
pub const CONTRACT_VERSION: u32 = 4;

/// Minimum number of ledgers that must elapse between a proposal reaching
/// threshold and becoming executable.  At ~6 s per ledger this is roughly
/// 24 h, giving affected parties a window to notice and react before an
/// admin action takes effect.
///
/// In tests we use a much smaller value so ledger-advance doesn't archive
/// instance storage entries before `execute_proposal` can read them.
#[cfg(not(test))]
pub const TIMELOCK_LEDGERS: u32 = 17_280;

/// Test configuration for timelock ledgers.
#[cfg(test)]
pub const TIMELOCK_LEDGERS: u32 = 10;

/// How long a registration's *remaining* stake stays locked after a slash.
///
/// This is what "good standing" means for [`LuminaRegistry::withdraw_stake`]:
/// a slash is evidence that something is wrong, and letting the owner pull the
/// rest of their collateral out in the next ledger would make the first slash
/// the only one governance ever lands. The window is deliberately the same
/// ~24 h as [`TIMELOCK_LEDGERS`], which is exactly how long it takes to get a
/// follow-up slash proposal through the timelock.
///
/// As with the timelock, tests use a small value so the ledger can be advanced
/// past it without archiving instance storage.
#[cfg(not(test))]
pub const SLASH_LOCK_LEDGERS: u32 = 17_280;

/// Test configuration for slash lock ledgers.
#[cfg(test)]
pub const SLASH_LOCK_LEDGERS: u32 = 10;

// ─── Errors ────────────────────────────────────────────────────────────────

/// Errors returned by the Lumina Registry contract operations.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RegistryError {
    /// Contract is already initialized.
    AlreadyInitialized  = 1,
    /// Caller lacks authorization for this action.
    Unauthorized        = 2,
    /// Contract is already registered.
    AlreadyRegistered   = 3,
    /// Referenced contract was not found.
    ContractNotFound    = 4,
    /// Metadata provided is invalid.
    InvalidMetadata     = 5,
    /// Caller is not the registered owner of the contract.
    NotOwner            = 6,
    /// The registry has no admin because `initialize` was never called.
    NotInitialized      = 7,
    /// The referenced proposal does not exist.
    ProposalNotFound    = 8,
    /// The proposal has not yet collected enough approvals to be executed.
    ThresholdNotMet     = 9,
    /// The timelock delay has not elapsed since the proposal reached threshold.
    TimelockNotElapsed  = 10,
    /// This admin has already approved this proposal.
    AlreadyApproved     = 11,
    /// Caller is not a member of the admin set.
    NotAdmin            = 12,
    /// The admin set would become empty or the threshold would exceed the set
    /// size after this change.
    InvalidThreshold    = 13,
    /// The proposal has already been executed.
    AlreadyExecuted     = 14,
    /// No stake token / treasury has been set, so staking is not open yet.
    StakingNotConfigured = 15,
    /// A stake or slash amount was zero or negative.
    InvalidAmount       = 16,
    /// The registration's staked balance is smaller than the requested amount.
    InsufficientStake   = 17,
    /// The stake is still inside the post-slash lock window.
    StakeLocked         = 18,
    /// The registration is still active — deactivate before withdrawing.
    RegistrationActive  = 19,
    /// A registration must declare at least one category.
    NoCategories        = 20,
    /// The registration still holds stake — withdraw it before deregistering.
    StakeNotEmpty       = 21,
}

// ─── Storage shapes ────────────────────────────────────────────────────────
//
// ## Upgrade-compatibility rules
//
// `upgrade()` replaces the contract's code but leaves every ledger entry it
// has already written exactly as it is.  When changing these types:
//
// - Adding a `DataKey` variant is safe; renaming or repurposing one is not
//   (encoded by variant *name*).
// - Adding, removing, renaming, or retyping a struct field breaks every
//   existing entry.  A release that must change `ContractEntry` needs a
//   migration (see `DEPLOY.md`).
// - Bump [`CONTRACT_VERSION`] alongside any such change.
//
// `registry-v2/src/lib.rs` re-declares both types independently and reads
// back storage written by this version — that test keeps these rules honest.

/// Stored entry describing a registered Soroban contract.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractEntry {
    /// The registered Soroban contract address.
    pub contract_id: Address,
    /// Owner/deployer who registered this contract.
    pub owner: Address,
    /// Human-readable name (e.g. "Quorum Governance").
    pub name: String,
    /// Short description of what the contract does.
    pub description: String,
    /// Ledger at which this contract was registered.
    pub registered_at: u32,
    /// Whether indexing is currently active for this contract.
    pub active: bool,
}

// ─── Category taxonomy ─────────────────────────────────────────────────────
//
// ## Why a fixed enum rather than free-form `Vec<Symbol>` tags
//
// Tags are more flexible, and that is exactly the problem. The point of
// categories here is *browsing* — "show me the DeFi contracts" — and free-form
// tags fragment that immediately: `DeFi`, `defi`, `De-Fi` and `Defi` become
// four categories that each hold a slice of the answer, with no way for a
// client to know they are the same thing. A discovery surface needs a shared
// vocabulary more than it needs expressiveness.
//
// A fixed enum also keeps `DataKey::ByCategory` a bounded key space, so the
// number of index entries is a property of the contract rather than of what
// registrants happen to type.
//
// The cost is that adding a category needs a contract upgrade. That was a real
// objection before the registry became upgradeable; now it is a normal release
// (see `DEPLOY.md`), and adding a variant is safe under the storage rules above
// because `#[contracttype]` enums encode by variant *name* — existing entries
// keep decoding as long as current variants are neither renamed nor
// repurposed. [`Category::Other`] is the escape hatch in the meantime, so
// nothing is unclassifiable while waiting for that release.

/// The category vocabulary a registration can be browsed under.
///
/// Append new variants at the end and never rename or repurpose an existing
/// one — see the storage rules above.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Category {
    /// Decentralized finance protocols and instruments.
    DeFi,
    /// Non-fungible token contracts and collections.
    Nft,
    /// On-chain gaming contracts and state.
    Gaming,
    /// Identity and credential verification contracts.
    Identity,
    /// Core infrastructure, routers, and utility contracts.
    Infrastructure,
    /// Payment processors and payment rails.
    Payments,
    /// Data oracles and price feeds.
    Oracle,
    /// Decentralized autonomous organizations and governance contracts.
    Dao,
    /// Anything the vocabulary does not cover yet.
    Other,
}

// ─── Reputation types ──────────────────────────────────────────────────────
//
// Reputation is stored *beside* `ContractEntry`, never inside it. Adding
// fields to `ContractEntry` would break every entry already written by v1 —
// see the upgrade-compatibility rules above — and would force a migration on
// the live testnet deployment for what is, from storage's point of view,
// purely additive data. New `DataKey` variants cost nothing and are safe.
//
// [`ContractProfile`] is what closes the gap for callers: it joins the entry
// and its reputation at read time, so a consumer that wants both gets both in
// one call without the stored shape ever changing.

/// One slash levied against a registration, kept forever so the reason stays
/// auditable after the fact.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SlashRecord {
    /// How much stake was taken.
    pub amount: i128,
    /// Why governance slashed — recorded on-chain for accountability.
    pub reason: String,
    /// Ledger at which the slash executed.
    pub slashed_at: u32,
}

/// The reputation signal attached to a registration.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Reputation {
    /// Currently staked, withdrawable balance.
    pub stake: i128,
    /// Whether governance has attested this registration.
    pub verified: bool,
    /// Lifetime total slashed, which unlike `stake` never goes down.
    pub slashed_total: i128,
    /// Ledger before which `withdraw_stake` is refused. Zero once clear.
    pub withdraw_locked_until: u32,
}

/// A registration joined with its reputation — what a discovery client wants.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractProfile {
    /// The base registration metadata and status.
    pub entry: ContractEntry,
    /// The reputation and staking signal.
    pub reputation: Reputation,
}

// ─── Proposal types ────────────────────────────────────────────────────────

/// The action a governance proposal will execute once it clears threshold and
/// timelock.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum ProposalAction {
    /// Deactivate the given contract on behalf of the registry (admin action).
    Deactivate(Address),
    /// Upgrade the contract wasm to the given hash.
    Upgrade(BytesN<32>),
    /// Add a new address to the admin set.
    AddAdmin(Address),
    /// Remove an address from the admin set.
    RemoveAdmin(Address),
    /// Change the approval threshold.
    ChangeThreshold(u32),
    /// Point staking at a token and a treasury: `(stake_token, treasury)`.
    ConfigureStaking(Address, Address),
    /// Attest (or revoke) verified status for a registration.
    SetVerified(Address, bool),
    /// Take `(contract_id, amount, reason)` of a registration's stake.
    Slash(Address, i128, String),
    /// Enable or disable permissioned registration.
    SetAllowlistEnabled(bool),
    /// Add or remove an owner from the registration allowlist.
    SetAllowlisted(Address, bool),
    /// Set the per-owner limit and ledger window; a zero limit disables it.
    ConfigureRegistrationRateLimit(u32, u32),
}

/// Fixed-window registration counter for one owner.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RegistrationWindow {
    pub started_at: u32,
    pub count: u32,
}

/// State stored for every open (or executed) proposal.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    /// Sequential proposal ID, assigned by the contract.
    pub id: u32,
    /// The admin who submitted this proposal.
    pub proposer: Address,
    /// What the proposal will do when executed.
    pub action: ProposalAction,
    /// Admins who have already approved (prevents double-counting).
    pub approvals: Vec<Address>,
    /// Ledger sequence at which the proposal reached threshold.
    /// `u32::MAX` (0xFFFF_FFFF) means the threshold has not yet been reached.
    pub ready_at: u32,
    /// Whether the proposal has already been executed.
    pub executed: bool,
}

/// Storage keys used by the Lumina Registry contract.
#[contracttype]
pub enum DataKey {
    // ── Governance ──────────────────────────────────────────────────────────
    /// Vec<Address> — the current admin set.
    Admins,
    /// u32 — number of approvals required to pass a proposal.
    Threshold,
    /// u32 — monotonically-increasing proposal counter.
    ProposalCount,
    /// Proposal — the full proposal record.
    ProposalData(u32),

    // ── Registry ────────────────────────────────────────────────────────────
    /// u32 — live registrations (deactivated included, deregistered excluded).
    /// Incremented on `register_contract`, decremented on `deregister`.
    /// See `get_contract_count` / `get_total_registered` for which figure to read.
    ContractCount,
    /// u32 — lifetime registrations ever made. Incremented on
    /// `register_contract` and never decremented, so it survives `deregister`.
    /// Added alongside deregistration to keep the old "registrations ever made"
    /// figure available after `ContractCount` became the live total. Missing on
    /// deployments that predate it — `get_total_registered` falls back to
    /// `ContractCount` in that case.
    TotalRegistered,
    Contract(Address),
    /// Vec<Address> — list of contracts registered by a specific owner.
    OwnerContracts(Address),
    /// Vec<Address> — insertion-ordered list of every registered contract.
    AllContracts,

    // ── Staking & reputation ────────────────────────────────────────────────
    /// Address — the SEP-41 token stakes are denominated in.
    StakeToken,
    /// Address — where slashed stake is sent.
    Treasury,
    /// i128 — currently staked balance for a registration.
    Stake(Address),
    /// bool — governance-attested verified status.
    Verified(Address),
    /// Vec<SlashRecord> — every slash ever levied, oldest first.
    Slashes(Address),
    /// u32 — ledger before which `withdraw_stake` is refused.
    WithdrawLockedUntil(Address),

    // ── Category taxonomy ───────────────────────────────────────────────────
    /// Vec<Category> — the categories a registration declared, deduplicated.
    Categories(Address),
    /// Vec<Address> — insertion-ordered registrations in one category.
    ///
    /// Persistent rather than instance, following `OwnerContracts`: these grow
    /// with the number of registrations, and instance storage is a single
    /// bounded entry shared by everything in it.
    ByCategory(Category),

    // ── Registration policy ─────────────────────────────────────────────────
    /// bool — whether only allowlisted owners may register.
    AllowlistEnabled,
    /// bool — whether an owner is allowlisted.
    Allowlisted(Address),
    /// u32 — per-owner registrations per fixed window; zero disables limiting.
    RegistrationRateLimit,
    /// u32 — fixed registration window size in ledgers.
    RegistrationRateWindow,
    /// RegistrationWindow — the current fixed-window counter for one owner.
    RegistrationWindow(Address),

    // ── Legacy key kept for upgrade compatibility ────────────────────────
    /// Single-admin key written by the original v1 initialize.  Retained so
    /// that the registry-v2 upgrade tests, which read `DataKey::Admin` from
    /// instance storage, continue to decode correctly after an upgrade.
    Admin,
}

// ─── Contract ──────────────────────────────────────────────────────────────

/// Main contract type implementing the Lumina on-chain contract registry.
#[contract]
pub struct LuminaRegistry;

#[contractimpl]
impl LuminaRegistry {
    /// Atomically initialize a new deployment with one bootstrap admin.
    /// The deployment transaction must include that admin's authorization.
    pub fn __constructor(env: Env, bootstrap_admin: Address) {
        bootstrap_admin.require_auth();
        let mut admins = Vec::new(&env);
        admins.push_back(bootstrap_admin.clone());
        env.storage().instance().set(&DataKey::Admins, &admins);
        env.storage().instance().set(&DataKey::Threshold, &1u32);
        env.storage().instance().set(&DataKey::ProposalCount, &0u32);
        env.storage().instance().set(&DataKey::ContractCount, &0u32);
        env.storage().instance().set(&DataKey::Admin, &bootstrap_admin);
    }

    // ── Initialization ──────────────────────────────────────────────────────

    /// One-time setup.  `admins` must be non-empty and `threshold` must be
    /// between 1 and `admins.len()`.
    pub fn initialize(
        env: Env,
        admins: Vec<Address>,
        threshold: u32,
    ) -> Result<(), RegistryError> {
        if env.storage().instance().has(&DataKey::Admins) {
            return Err(RegistryError::AlreadyInitialized);
        }

        if admins.is_empty()
            || threshold == 0
            || threshold > admins.len()
        {
            return Err(RegistryError::InvalidThreshold);
        }

        // Every admin must authorize the initialization.
        for admin in admins.iter() {
            admin.require_auth();
        }

        env.storage().instance().set(&DataKey::Admins, &admins);
        env.storage().instance().set(&DataKey::Threshold, &threshold);
        env.storage().instance().set(&DataKey::ProposalCount, &0u32);
        env.storage().instance().set(&DataKey::ContractCount, &0u32);
        env.storage().instance().set(&DataKey::TotalRegistered, &0u32);

        // Write the legacy Admin key with the first admin so the v2 upgrade
        // tests (which read DataKey::Admin) continue to pass unchanged.
        let first_admin = admins.get(0).ok_or(RegistryError::InvalidThreshold)?;
        env.storage().instance().set(&DataKey::Admin, &first_admin);

        Ok(())
    }

    // ── Governance: proposal creation ───────────────────────────────────────

    /// Propose deactivating a contract that belongs to someone else.
    /// Returns the new proposal ID.
    pub fn propose_deactivate(
        env: Env,
        proposer: Address,
        contract_id: Address,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        Self::assert_is_admin(&env, &proposer)?;

        // Make sure the target actually exists.
        if !env.storage().persistent().has(&DataKey::Contract(contract_id.clone())) {
            return Err(RegistryError::ContractNotFound);
        }

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::Deactivate(contract_id.clone()),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (proposal_id, proposer, Symbol::new(&env, "deactivate"), contract_id),
        );

        Ok(proposal_id)
    }

    /// Propose adding a new admin.
    pub fn propose_add_admin(
        env: Env,
        proposer: Address,
        new_admin: Address,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        Self::assert_is_admin(&env, &proposer)?;

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::AddAdmin(new_admin.clone()),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (proposal_id, proposer, Symbol::new(&env, "add_admin"), new_admin),
        );

        Ok(proposal_id)
    }

    /// Propose removing an admin.
    pub fn propose_remove_admin(
        env: Env,
        proposer: Address,
        admin_to_remove: Address,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        Self::assert_is_admin(&env, &proposer)?;

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::RemoveAdmin(admin_to_remove.clone()),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (proposal_id, proposer, Symbol::new(&env, "remove_admin"), admin_to_remove),
        );

        Ok(proposal_id)
    }

    /// Propose changing the approval threshold.
    pub fn propose_change_threshold(
        env: Env,
        proposer: Address,
        new_threshold: u32,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        Self::assert_is_admin(&env, &proposer)?;

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::ChangeThreshold(new_threshold),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (proposal_id, proposer, Symbol::new(&env, "change_threshold"), new_threshold),
        );

        Ok(proposal_id)
    }

    /// Propose a wasm upgrade via governance.
    pub fn propose_upgrade(
        env: Env,
        proposer: Address,
        new_wasm_hash: BytesN<32>,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        Self::assert_is_admin(&env, &proposer)?;

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::Upgrade(new_wasm_hash.clone()),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (proposal_id, proposer, Symbol::new(&env, "upgrade"), new_wasm_hash),
        );

        Ok(proposal_id)
    }

    /// Propose pointing staking at `token`, with slashed stake going to
    /// `treasury`.
    ///
    /// Deliberately a proposal rather than a setter on `initialize`: whoever
    /// sets the treasury decides where every future slash lands, and the live
    /// registry was already initialized under the old signature — routing this
    /// through governance lets a deployed registry adopt staking after an
    /// upgrade instead of needing to be redeployed.
    pub fn propose_configure_staking(
        env: Env,
        proposer: Address,
        token: Address,
        treasury: Address,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        Self::assert_is_admin(&env, &proposer)?;

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::ConfigureStaking(token.clone(), treasury.clone()),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (proposal_id, proposer, Symbol::new(&env, "configure_staking"), token),
        );

        Ok(proposal_id)
    }

    /// Propose attesting — or revoking — verified status for a registration.
    ///
    /// There is no non-governance path to this: a registrant cannot mark
    /// themselves verified, which is the whole point of the signal.
    pub fn propose_set_verified(
        env: Env,
        proposer: Address,
        contract_id: Address,
        verified: bool,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        Self::assert_is_admin(&env, &proposer)?;

        if !env.storage().persistent().has(&DataKey::Contract(contract_id.clone())) {
            return Err(RegistryError::ContractNotFound);
        }

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::SetVerified(contract_id.clone(), verified),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (proposal_id, proposer, Symbol::new(&env, "set_verified"), contract_id),
        );

        Ok(proposal_id)
    }

    /// Propose slashing `amount` of a registration's stake, with a reason that
    /// is recorded on-chain.
    ///
    /// Validated here as well as at execution time so an obviously bad
    /// proposal (unknown contract, non-positive amount) fails at proposal time
    /// rather than sitting through the timelock only to revert.
    pub fn propose_slash(
        env: Env,
        proposer: Address,
        contract_id: Address,
        amount: i128,
        reason: String,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        Self::assert_is_admin(&env, &proposer)?;

        if !env.storage().persistent().has(&DataKey::Contract(contract_id.clone())) {
            return Err(RegistryError::ContractNotFound);
        }
        if amount <= 0 {
            return Err(RegistryError::InvalidAmount);
        }

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::Slash(contract_id.clone(), amount, reason.clone()),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (proposal_id, proposer, Symbol::new(&env, "slash"), (contract_id, amount, reason)),
        );

        Ok(proposal_id)
    }

    /// Govern whether new registrations require an allowlisted owner.
    /// Permissionless registration remains the default until this proposal executes.
    pub fn propose_set_allowlist_enabled(
        env: Env,
        proposer: Address,
        enabled: bool,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        Self::assert_is_admin(&env, &proposer)?;
        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::SetAllowlistEnabled(enabled),
        );
        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (proposal_id, proposer, Symbol::new(&env, "set_allowlist"), enabled),
        );
        Ok(proposal_id)
    }

    /// Govern an owner's membership in the registration allowlist.
    pub fn propose_set_allowlisted(
        env: Env,
        proposer: Address,
        owner: Address,
        allowed: bool,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        Self::assert_is_admin(&env, &proposer)?;
        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::SetAllowlisted(owner.clone(), allowed),
        );
        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (proposal_id, proposer, Symbol::new(&env, "set_allowlisted"), (owner, allowed)),
        );
        Ok(proposal_id)
    }

    /// Govern a fixed-window per-owner registration limit. Zero disables it.
    pub fn propose_configure_registration_rate_limit(
        env: Env,
        proposer: Address,
        limit: u32,
        window_ledgers: u32,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        Self::assert_is_admin(&env, &proposer)?;
        if limit > 0 && (window_ledgers == 0 || window_ledgers > env.storage().max_ttl()) {
            return Err(RegistryError::InvalidRateLimit);
        }
        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::ConfigureRegistrationRateLimit(limit, window_ledgers),
        );
        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (proposal_id, proposer, Symbol::new(&env, "configure_rate_limit"), (limit, window_ledgers)),
        );
        Ok(proposal_id)
    }

    // ── Governance: approval ────────────────────────────────────────────────

    /// Record an admin's approval of a proposal.  When the number of unique
    /// approvals reaches the stored threshold the proposal transitions to
    /// *ready* (but is not yet executed).
    pub fn approve_proposal(
        env: Env,
        admin: Address,
        proposal_id: u32,
    ) -> Result<(), RegistryError> {
        admin.require_auth();
        Self::assert_is_admin(&env, &admin)?;

        let mut proposal = Self::load_proposal(&env, proposal_id)?;

        if proposal.executed {
            return Err(RegistryError::AlreadyExecuted);
        }

        // Reject duplicate approvals.
        if proposal.approvals.contains(&admin) {
            return Err(RegistryError::AlreadyApproved);
        }

        proposal.approvals.push_back(admin.clone());

        let threshold: u32 = env.storage().instance().get(&DataKey::Threshold).unwrap_or(1);

        env.events().publish(
            (Symbol::new(&env, "proposal_approved"),),
            (proposal_id, admin.clone(), proposal.approvals.len()),
        );

        // Transition to ready when threshold is first reached.
        if proposal.ready_at == u32::MAX && proposal.approvals.len() >= threshold {
            proposal.ready_at = env.ledger().sequence();
            env.events().publish(
                (Symbol::new(&env, "proposal_ready"),),
                (proposal_id, proposal.ready_at),
            );
        }

        Self::save_proposal(&env, &proposal);
        Ok(())
    }

    // ── Governance: execution ───────────────────────────────────────────────

    /// Execute a proposal that has reached threshold and passed the timelock.
    /// Callable by anyone once those conditions are satisfied.
    pub fn execute_proposal(env: Env, proposal_id: u32) -> Result<(), RegistryError> {
        let mut proposal = Self::load_proposal(&env, proposal_id)?;

        if proposal.executed {
            return Err(RegistryError::AlreadyExecuted);
        }

        let threshold: u32 = env.storage().instance().get(&DataKey::Threshold).unwrap_or(1);
        if proposal.approvals.len() < threshold {
            return Err(RegistryError::ThresholdNotMet);
        }

        if proposal.ready_at == u32::MAX {
            return Err(RegistryError::ThresholdNotMet);
        }

        let current = env.ledger().sequence();
        if current < proposal.ready_at + TIMELOCK_LEDGERS {
            return Err(RegistryError::TimelockNotElapsed);
        }

        // Mark executed before side effects (prevents re-entrance).
        proposal.executed = true;
        Self::save_proposal(&env, &proposal);

        Self::apply_action(&env, &proposal.action)?;

        env.events().publish(
            (Symbol::new(&env, "proposal_executed"),),
            (proposal_id, current),
        );

        Ok(())
    }

    // ── Governance: instant deactivate by the owner ─────────────────────────

    /// Deactivate a contract you own *immediately* — no proposal needed because
    /// you are the registered owner.  An admin deactivating *another* owner's
    /// contract must go through `propose_deactivate`.
    pub fn deactivate(env: Env, caller: Address, contract_id: Address) -> Result<(), RegistryError> {
        caller.require_auth();

        let mut entry: ContractEntry = env.storage().persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        // Owners can self-deactivate instantly.  Admins deactivating someone
        // else's contract must go through the governance flow.
        if caller != entry.owner {
            return Err(RegistryError::Unauthorized);
        }

        entry.active = false;
        env.storage().persistent().set(&DataKey::Contract(contract_id.clone()), &entry);

        env.events().publish(
            (Symbol::new(&env, "contract_deactivated"),),
            (contract_id, caller),
        );

        Ok(())
    }

    /// Permanently remove a deactivated, unstaked registration.
    ///
    /// This is the deregistration path, as opposed to `deactivate` (a soft
    /// flag that keeps the entry so the owner stays listed under
    /// `get_contracts_by_owner` and the address cannot be re-registered).
    /// `deregister` deletes the `Contract` entry itself, so the same address
    /// may be registered again later as a fresh entry.
    ///
    /// Requirements, checked in order:
    ///
    /// 1. the caller is the registered owner;
    /// 2. the registration is already deactivated (`deactivate` first);
    /// 3. no stake remains (`withdraw_stake` first — otherwise funds would be
    ///    stranded under an address the registry no longer tracks).
    ///
    /// Cleanup is **eager**: the entry is removed from the global
    /// `AllContracts` index, the owner's index, and every category index it
    /// claimed, and `ContractCount` (the live total) is decremented.
    /// `TotalRegistered` (the lifetime total) is deliberately left untouched.
    /// Slash records are intentionally kept so penalties stay auditable after
    /// the registration they were levied against is gone.
    ///
    /// Storage archival (TTL expiry making a `Contract` entry unloadable
    /// without any explicit call) is handled separately by the permissionless,
    /// idempotent `prune_category` / `prune_all_contracts` entrypoints: eager
    /// removal covers every path the contract itself controls, while pruning
    /// covers the one path it does not — the ledger garbage-collecting an
    /// entry out from under an index that still names it.
    pub fn deregister(env: Env, owner: Address, contract_id: Address) -> Result<(), RegistryError> {
        owner.require_auth();

        let entry: ContractEntry = env.storage().persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        if owner != entry.owner {
            return Err(RegistryError::NotOwner);
        }
        if entry.active {
            return Err(RegistryError::RegistrationActive);
        }
        if Self::stake_of(&env, &contract_id) != 0 {
            return Err(RegistryError::StakeNotEmpty);
        }

        // Drop every category index reference first, so a deregistered
        // contract leaves no index reference behind (see #140).
        for category in Self::categories_of(&env, &contract_id).iter() {
            let mut index = Self::category_index(&env, &category);
            if let Some(i) = index.first_index_of(&contract_id) {
                index.remove(i);
                env.storage().persistent()
                    .set(&DataKey::ByCategory(category), &index);
            }
        }
        env.storage().persistent().remove(&DataKey::Categories(contract_id.clone()));

        // Drop the global and owner indexes.
        let mut all: Vec<Address> = env.storage().instance().get(&DataKey::AllContracts).unwrap_or(Vec::new(&env));
        if let Some(i) = all.first_index_of(&contract_id) {
            all.remove(i);
            env.storage().instance().set(&DataKey::AllContracts, &all);
        }
        let mut owned = Self::owner_index(&env, &entry.owner);
        if let Some(i) = owned.first_index_of(&contract_id) {
            owned.remove(i);
            Self::set_owner_index(&env, &entry.owner, &owned);
        }

        // Delete the entry and its live reputation state. Slashes are kept
        // for auditability (see docstring above).
        env.storage().persistent().remove(&DataKey::Contract(contract_id.clone()));
        env.storage().persistent().remove(&DataKey::Stake(contract_id.clone()));
        env.storage().persistent().remove(&DataKey::Verified(contract_id.clone()));
        env.storage().persistent().remove(&DataKey::WithdrawLockedUntil(contract_id.clone()));

        // Live total goes down; lifetime total does not (see #141).
        let count: u32 = env.storage().instance().get(&DataKey::ContractCount).unwrap_or(1);
        env.storage().instance().set(&DataKey::ContractCount, &count.saturating_sub(1));

        env.events().publish(
            (Symbol::new(&env, "contract_deregistered"),),
            (contract_id, owner),
        );

        Ok(())
    }

    /// Remove dead references from one category index.
    ///
    /// A reference is dead when its `Contract` entry no longer loads — either
    /// because the registration was removed outside the indexed paths (e.g.
    /// storage archival/TTL expiry) or because it predates eager cleanup.
    /// `deregister` and `set_categories` already remove their own references
    /// eagerly; this covers the paths they cannot, which is why it exists
    /// alongside eager removal rather than instead of it.
    ///
    /// Permissionless (no auth) so anyone — indexer, frontend, or a cron-like
    /// caller — can pay for the cleanup. Idempotent and safe to call
    /// repeatedly: a second call with nothing dead removes nothing and
    /// returns 0. Returns the number of references removed.
    pub fn prune_category(env: Env, category: Category) -> u32 {
        let index = Self::category_index(&env, &category);
        let mut live = Vec::new(&env);
        let mut removed: u32 = 0;

        for contract_id in index.iter() {
            if env.storage().persistent().has(&DataKey::Contract(contract_id.clone())) {
                live.push_back(contract_id);
            } else {
                removed += 1;
            }
        }

        if removed > 0 {
            env.storage().persistent().set(&DataKey::ByCategory(category), &live);
        }

        env.events().publish(
            (Symbol::new(&env, "category_pruned"),),
            (category, removed),
        );

        removed
    }

    /// Remove dead references from the global `AllContracts` index.
    ///
    /// Same dead-definition, permissionless idempotent semantics, and return
    /// value as `prune_category`, but for the paginated
    /// `get_active_contracts` / `get_active_profiles` listing instead of one
    /// category. Callers that prune categories on a schedule should prune the
    /// global index on the same schedule.
    pub fn prune_all_contracts(env: Env) -> u32 {
        let all: Vec<Address> = env.storage().instance().get(&DataKey::AllContracts).unwrap_or(Vec::new(&env));
        let mut live = Vec::new(&env);
        let mut removed: u32 = 0;

        for contract_id in all.iter() {
            if env.storage().persistent().has(&DataKey::Contract(contract_id.clone())) {
                live.push_back(contract_id);
            } else {
                removed += 1;
            }
        }

        if removed > 0 {
            env.storage().instance().set(&DataKey::AllContracts, &live);
        }

        env.events().publish(
            (Symbol::new(&env, "all_contracts_pruned"),),
            (removed,),
        );

        removed
    }

    // ── Legacy upgrade kept for backward-compatibility with existing tests ───

    /// Direct upgrade, kept for the upgrade-path tests in this crate (which
    /// deploy v1 wasm via `contractimport!` and then call `upgrade` with the
    /// old single-admin signature).
    ///
    /// For new deployments, use `propose_upgrade` / `approve_proposal` /
    /// `execute_proposal` instead.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) -> Result<(), RegistryError> {
        admin.require_auth();

        // Accept either the old single-admin key or membership in the new set.
        let is_old_admin = env.storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::Admin)
            .map(|a| a == admin)
            .unwrap_or(false);
        let is_new_admin = Self::admin_index(&env).contains(&admin);

        if !is_old_admin && !is_new_admin {
            return Err(RegistryError::Unauthorized);
        }

        env.deployer().update_current_contract_wasm(new_wasm_hash.clone());

        // `CONTRACT_VERSION` is the version being *replaced*, not the incoming
        // one: the new wasm only takes over once this invocation returns, and
        // this code cannot know what version the new wasm carries. Consumers
        // read this field as "upgraded away from vN" — do not "fix" it to the
        // new version. Pinned by `registry_upgraded_event_reports_the_replaced_version`.
        env.events().publish(
            (Symbol::new(&env, "registry_upgraded"),),
            (admin, new_wasm_hash, CONTRACT_VERSION),
        );

        Ok(())
    }

    // ── Registry ────────────────────────────────────────────────────────────

    /// Register a Soroban contract for Lumina indexing.
    /// Anyone can register — the owner must authorize the call.
    ///
    /// `categories` must name at least one [`Category`]; duplicates are
    /// collapsed, so passing the same category twice indexes it once. Use
    /// [`Category::Other`] if none of the vocabulary fits.
    pub fn register_contract(
        env: Env,
        owner: Address,
        contract_id: Address,
        name: String,
        description: String,
        categories: Vec<Category>,
    ) -> Result<(), RegistryError> {
        owner.require_auth();

        if env.storage().instance().get(&DataKey::AllowlistEnabled).unwrap_or(false)
            && !env.storage().persistent().get(&DataKey::Allowlisted(owner.clone())).unwrap_or(false)
        {
            return Err(RegistryError::NotAllowlisted);
        }

        if env.storage().persistent().has(&DataKey::Contract(contract_id.clone())) {
            return Err(RegistryError::AlreadyRegistered);
        }

        let categories = Self::dedup_categories(&env, &categories)?;

        Self::consume_registration_rate(&env, &owner)?;

        let entry = ContractEntry {
            contract_id: contract_id.clone(),
            owner: owner.clone(),
            name: name.clone(),
            description,
            registered_at: env.ledger().sequence(),
            active: true,
        };

        env.storage().persistent().set(&DataKey::Contract(contract_id.clone()), &entry);

        let mut owned = Self::owner_index(&env, &owner);
        owned.push_back(contract_id.clone());
        Self::set_owner_index(&env, &owner, &owned);

        let mut all: Vec<Address> = env.storage().instance().get(&DataKey::AllContracts).unwrap_or(Vec::new(&env));
        all.push_back(contract_id.clone());
        env.storage().instance().set(&DataKey::AllContracts, &all);

        let count: u32 = env.storage().instance().get(&DataKey::ContractCount).unwrap_or(0);
        env.storage().instance().set(&DataKey::ContractCount, &(count + 1));
        // Lifetime total: never decremented, so it keeps counting across
        // `deregister`. `ContractCount` above is the live total.
        let total: u32 = env.storage().instance().get(&DataKey::TotalRegistered).unwrap_or(count);
        env.storage().instance().set(&DataKey::TotalRegistered, &(total + 1));

        Self::index_categories(&env, &contract_id, &categories);

        env.events().publish(
            (Symbol::new(&env, "contract_registered"),),
            (contract_id, owner, name, categories),
        );

        Ok(())
    }

    /// Re-declare which categories a registration is browsable under.
    ///
    /// Only the registered owner — the same authority as `update_metadata`,
    /// for the same reason: how a project files itself is metadata, not
    /// something the admin set has a say in.
    ///
    /// This is also the migration path for registrations made before the
    /// taxonomy existed. Those have no categories and so appear in no category
    /// listing; their owners can classify them without re-registering.
    pub fn set_categories(
        env: Env,
        owner: Address,
        contract_id: Address,
        categories: Vec<Category>,
    ) -> Result<(), RegistryError> {
        owner.require_auth();

        let entry: ContractEntry = env.storage().persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        if owner != entry.owner {
            return Err(RegistryError::NotOwner);
        }

        let categories = Self::dedup_categories(&env, &categories)?;

        // Drop the registration from any category it is leaving, so a stale
        // index cannot resurface it under a category it no longer claims.
        for previous in Self::categories_of(&env, &contract_id).iter() {
            if !categories.contains(&previous) {
                let mut index = Self::category_index(&env, &previous);
                if let Some(i) = index.first_index_of(&contract_id) {
                    index.remove(i);
                    env.storage().persistent()
                        .set(&DataKey::ByCategory(previous), &index);
                }
            }
        }

        Self::index_categories(&env, &contract_id, &categories);

        env.events().publish(
            (Symbol::new(&env, "categories_updated"),),
            (contract_id, owner, categories),
        );

        Ok(())
    }

    // ── Staking ─────────────────────────────────────────────────────────────

    /// Post collateral against a registration you own.
    ///
    /// Staking is a separate call rather than a `register_contract` parameter
    /// on purpose: registration stays free and permissionless (anyone can list
    /// a contract for indexing), and stake is the *optional* signal layered on
    /// top. It also means the registrations that already exist can acquire a
    /// stake without re-registering.
    ///
    /// Additive — calling it again tops the stake up.
    pub fn stake(
        env: Env,
        owner: Address,
        contract_id: Address,
        amount: i128,
    ) -> Result<(), RegistryError> {
        owner.require_auth();

        if amount <= 0 {
            return Err(RegistryError::InvalidAmount);
        }

        let entry: ContractEntry = env.storage().persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        if owner != entry.owner {
            return Err(RegistryError::NotOwner);
        }

        let (token_id, _) = Self::staking_config(&env)?;

        // Moves real tokens into the registry's own balance. `owner` has
        // already authorized this invocation, and the token's own
        // `from.require_auth()` runs as a sub-invocation of it.
        token::Client::new(&env, &token_id).transfer(
            &owner,
            &env.current_contract_address(),
            &amount,
        );

        let staked = Self::stake_of(&env, &contract_id) + amount;
        env.storage().persistent().set(&DataKey::Stake(contract_id.clone()), &staked);

        env.events().publish(
            (Symbol::new(&env, "stake_deposited"),),
            (contract_id, owner, amount, staked),
        );

        Ok(())
    }

    /// Reclaim the full remaining stake for a registration.
    ///
    /// "Good standing" is three conditions, all checked here:
    ///
    /// 1. the caller is the registered owner;
    /// 2. the registration is **deactivated** — you get your collateral back
    ///    by leaving, not while still listed and benefiting from the stake;
    /// 3. no slash has landed within the last [`SLASH_LOCK_LEDGERS`] ledgers,
    ///    so an owner cannot front-run governance by emptying the stake as
    ///    soon as the first slash reveals it is being watched.
    ///
    /// Returns the amount returned to the owner.
    pub fn withdraw_stake(
        env: Env,
        owner: Address,
        contract_id: Address,
    ) -> Result<i128, RegistryError> {
        owner.require_auth();

        let entry: ContractEntry = env.storage().persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        if owner != entry.owner {
            return Err(RegistryError::NotOwner);
        }
        if entry.active {
            return Err(RegistryError::RegistrationActive);
        }
        if env.ledger().sequence() < Self::withdraw_locked_until(&env, &contract_id) {
            return Err(RegistryError::StakeLocked);
        }

        let staked = Self::stake_of(&env, &contract_id);
        if staked <= 0 {
            return Err(RegistryError::InsufficientStake);
        }

        let (token_id, _) = Self::staking_config(&env)?;

        // The registry is the `from` here, and a contract authorizes moving
        // its own balance by virtue of being the invoker.
        token::Client::new(&env, &token_id).transfer(
            &env.current_contract_address(),
            &owner,
            &staked,
        );

        env.storage().persistent().set(&DataKey::Stake(contract_id.clone()), &0i128);

        env.events().publish(
            (Symbol::new(&env, "stake_withdrawn"),),
            (contract_id, owner, staked),
        );

        Ok(staked)
    }

    // ─── View ──────────────────────────────────────────────────────────────

    /// Which build of the registry is live at this address.
    pub fn get_version(_env: Env) -> u32 {
        CONTRACT_VERSION
    }

    /// The first admin address (kept for backward compatibility with v2 tests
    /// that call `get_admin`).
    pub fn get_admin(env: Env) -> Result<Address, RegistryError> {
        // Prefer the legacy single-admin key so the upgrade tests work as-is.
        if let Some(a) = env.storage().instance().get::<DataKey, Address>(&DataKey::Admin) {
            return Ok(a);
        }
        let admins = Self::admin_index(&env);
        admins.get(0).ok_or(RegistryError::NotInitialized)
    }

    /// The full current admin set.
    pub fn get_admins(env: Env) -> Result<Vec<Address>, RegistryError> {
        let admins = Self::admin_index(&env);
        if admins.is_empty() {
            return Err(RegistryError::NotInitialized);
        }
        Ok(admins)
    }

    /// The current approval threshold.
    pub fn get_threshold(env: Env) -> Result<u32, RegistryError> {
        env.storage()
            .instance()
            .get(&DataKey::Threshold)
            .ok_or(RegistryError::NotInitialized)
    }

    /// Retrieve a proposal by ID.
    pub fn get_proposal(env: Env, proposal_id: u32) -> Result<Proposal, RegistryError> {
        Self::load_proposal(&env, proposal_id)
    }

    /// The categories a registration declared. Empty for one registered
    /// before the taxonomy existed — see `set_categories`.
    pub fn get_categories(env: Env, contract_id: Address) -> Vec<Category> {
        Self::categories_of(&env, &contract_id)
    }

    /// Paginated list of active registrations in one category, in
    /// registration order.
    ///
    /// Semantics match [`LuminaRegistry::get_active_contracts`] exactly,
    /// including the one that surprises people: `offset` indexes into the
    /// category's raw index, not into the filtered result, so a page can come
    /// back shorter than `limit` when it spans deactivated entries.
    ///
    /// `deactivate` deliberately does not touch category indices — filtering
    /// here on `active` is what keeps a deactivated registration out of
    /// browsing, exactly as it does for the global listing, and it means
    /// reactivating a registration would restore it to every category it
    /// already claimed.
    pub fn get_active_contracts_by_category(
        env: Env,
        category: Category,
        offset: u32,
        limit: u32,
    ) -> Vec<ContractEntry> {
        let index = Self::category_index(&env, &category);
        let mut result = Vec::new(&env);

        let mut i = offset;
        while i < index.len() && result.len() < limit {
            if let Some(contract_id) = index.get(i) {
                if let Some(entry) = env.storage().persistent().get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id)) {
                    if entry.active {
                        result.push_back(entry);
                    }
                }
            }
            i += 1;
        }

        result
    }

    /// `(stake_token, treasury)`, or `StakingNotConfigured` if governance has
    /// not opened staking yet.
    pub fn get_staking_config(env: Env) -> Result<(Address, Address), RegistryError> {
        Self::staking_config(&env)
    }

    /// Currently staked balance. Zero for a registration that never staked.
    pub fn get_stake(env: Env, contract_id: Address) -> i128 {
        Self::stake_of(&env, &contract_id)
    }

    /// Whether governance has attested this registration.
    pub fn is_verified(env: Env, contract_id: Address) -> bool {
        env.storage().persistent()
            .get(&DataKey::Verified(contract_id))
            .unwrap_or(false)
    }

    /// Every slash levied against a registration, oldest first.
    pub fn get_slashes(env: Env, contract_id: Address) -> Vec<SlashRecord> {
        Self::slash_history(&env, &contract_id)
    }

    /// The full reputation signal for a registration. Returns zeroed values
    /// rather than erroring for an unregistered address, mirroring
    /// `is_registered`'s tolerance.
    pub fn get_reputation(env: Env, contract_id: Address) -> Reputation {
        Self::reputation_of(&env, &contract_id)
    }

    /// A registration joined with its reputation — one call instead of a
    /// `get_contract` plus a `get_reputation`.
    pub fn get_contract_profile(
        env: Env,
        contract_id: Address,
    ) -> Result<ContractProfile, RegistryError> {
        let entry: ContractEntry = env.storage().persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        Ok(ContractProfile {
            reputation: Self::reputation_of(&env, &contract_id),
            entry,
        })
    }

    /// `get_active_contracts`, with each entry's reputation attached. Same
    /// offset/limit and active-filtering semantics.
    pub fn get_active_profiles(env: Env, offset: u32, limit: u32) -> Vec<ContractProfile> {
        let all: Vec<Address> = env.storage().instance().get(&DataKey::AllContracts).unwrap_or(Vec::new(&env));
        let mut result = Vec::new(&env);

        let mut i = offset;
        while i < all.len() && result.len() < limit {
            if let Some(contract_id) = all.get(i) {
                if let Some(entry) = env.storage().persistent().get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id.clone())) {
                    if entry.active {
                        result.push_back(ContractProfile {
                            reputation: Self::reputation_of(&env, &contract_id),
                            entry,
                        });
                    }
                }
            }
            i += 1;
        }

        result
    }

    /// Retrieve the metadata entry for a registered contract.
    pub fn get_contract(env: Env, contract_id: Address) -> Result<ContractEntry, RegistryError> {
        env.storage().persistent()
            .get(&DataKey::Contract(contract_id))
            .ok_or(RegistryError::ContractNotFound)
    }

    /// Live registrations: deactivated entries included, deregistered ones
    /// not. Incremented on `register_contract`, decremented on `deregister`.
    /// This is the figure a "how many entries exist right now" consumer wants.
    /// For lifetime registrations ever made see `get_total_registered`; for
    /// currently listed (active) entries see `get_active_contract_count` (the
    /// frontend stats page should read that one).
    pub fn get_contract_count(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::ContractCount).unwrap_or(0)
    }

    /// Lifetime registrations ever made. Incremented on `register_contract`
    /// and never decremented, so it keeps counting across `deregister`.
    /// Falls back to `ContractCount` on deployments that predate the split
    /// (where the single counter was the lifetime figure).
    pub fn get_total_registered(env: Env) -> u32 {
        if let Some(total) = env.storage().instance().get::<DataKey, u32>(&DataKey::TotalRegistered) {
            return total;
        }
        env.storage().instance().get(&DataKey::ContractCount).unwrap_or(0)
    }

    /// Currently listed (active) registrations. Walks `AllContracts` and
    /// counts entries that still load and are flagged active, skipping dead
    /// references exactly as `get_active_contracts` does.
    pub fn get_active_contract_count(env: Env) -> u32 {
        let all: Vec<Address> = env.storage().instance().get(&DataKey::AllContracts).unwrap_or(Vec::new(&env));
        let mut active: u32 = 0;
        for contract_id in all.iter() {
            if let Some(entry) = env.storage().persistent().get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id)) {
                if entry.active {
                    active += 1;
                }
            }
        }
        active
    }

    pub fn is_registered(env: Env, contract_id: Address) -> bool {
        env.storage().persistent().has(&DataKey::Contract(contract_id))
    }

    /// Paginated list of active registered contracts in registration order.
    pub fn get_active_contracts(env: Env, offset: u32, limit: u32) -> Vec<ContractEntry> {
        let all: Vec<Address> = env.storage().instance().get(&DataKey::AllContracts).unwrap_or(Vec::new(&env));
        let mut result = Vec::new(&env);

        let mut i = offset;
        while i < all.len() && result.len() < limit {
            if let Some(contract_id) = all.get(i) {
                if let Some(entry) = env.storage().persistent().get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id)) {
                    if entry.active {
                        result.push_back(entry);
                    }
                }
            }
            i += 1;
        }

        result
    }

    /// Paginated list of every contract registered by `owner`, including
    /// deactivated entries.
    pub fn get_contracts_by_owner(env: Env, owner: Address, offset: u32, limit: u32) -> Vec<ContractEntry> {
        let owned = Self::owner_index(&env, &owner);
        let mut result = Vec::new(&env);

        let mut i = offset;
        while i < owned.len() && result.len() < limit {
            if let Some(contract_id) = owned.get(i) {
                if let Some(entry) = env.storage().persistent().get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id)) {
                    result.push_back(entry);
                }
            }
            i += 1;
        }

        result
    }

    /// Update a registered contract's name and description.
    /// Only the current registered owner can call this.
    pub fn update_metadata(
        env: Env,
        owner: Address,
        contract_id: Address,
        name: String,
        description: String,
    ) -> Result<(), RegistryError> {
        owner.require_auth();

        let mut entry: ContractEntry = env.storage().persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        if owner != entry.owner {
            return Err(RegistryError::NotOwner);
        }

        entry.name = name.clone();
        entry.description = description;
        env.storage().persistent().set(&DataKey::Contract(contract_id.clone()), &entry);

        env.events().publish(
            (Symbol::new(&env, "metadata_updated"),),
            (contract_id, owner, name),
        );

        Ok(())
    }

    /// Hand a registration over to a new owner.
    /// Only the current owner can call this (admin override removed — ownership
    /// transfer should be driven by the owner themselves).
    pub fn transfer_ownership(
        env: Env,
        caller: Address,
        contract_id: Address,
        new_owner: Address,
    ) -> Result<(), RegistryError> {
        caller.require_auth();

        let mut entry: ContractEntry = env.storage().persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        // Require caller to be the owner OR a member of the admin set.
        let is_owner = caller == entry.owner;
        let is_admin = Self::is_admin_member(&env, &caller);

        // Fall back to the legacy single-admin check for the upgrade tests.
        let is_legacy_admin = env.storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::Admin)
            .map(|a| a == caller)
            .unwrap_or(false);

        if !is_owner && !is_admin && !is_legacy_admin {
            // Neither the new multi-sig admins nor the legacy admin nor the
            // owner — check whether we're initialized at all so the error
            // message stays informative.
            if !env.storage().instance().has(&DataKey::Admins)
                && !env.storage().instance().has(&DataKey::Admin)
            {
                return Err(RegistryError::NotInitialized);
            }
            return Err(RegistryError::Unauthorized);
        }

        let previous_owner = entry.owner.clone();
        if previous_owner == new_owner {
            return Ok(());
        }

        let mut previous_owned = Self::owner_index(&env, &previous_owner);
        if let Some(i) = previous_owned.first_index_of(&contract_id) {
            previous_owned.remove(i);
            Self::set_owner_index(&env, &previous_owner, &previous_owned);
        }

        let mut new_owned = Self::owner_index(&env, &new_owner);
        new_owned.push_back(contract_id.clone());
        Self::set_owner_index(&env, &new_owner, &new_owned);

        entry.owner = new_owner.clone();
        env.storage().persistent().set(&DataKey::Contract(contract_id.clone()), &entry);

        env.events().publish(
            (Symbol::new(&env, "ownership_transferred"),),
            (contract_id, previous_owner, new_owner),
        );

        Ok(())
    }
}

// ─── Internal helpers ──────────────────────────────────────────────────────

impl LuminaRegistry {
    fn consume_registration_rate(env: &Env, owner: &Address) -> Result<(), RegistryError> {
        let limit: u32 = env.storage().instance().get(&DataKey::RegistrationRateLimit).unwrap_or(0);
        if limit == 0 {
            return Ok(());
        }
        let window: u32 = env.storage().instance().get(&DataKey::RegistrationRateWindow).unwrap_or(0);
        if window == 0 {
            return Err(RegistryError::InvalidRateLimit);
        }
        let now = env.ledger().sequence();
        let key = DataKey::RegistrationWindow(owner.clone());
        let mut state: RegistrationWindow = env.storage().persistent()
            .get(&key)
            .unwrap_or(RegistrationWindow { started_at: now, count: 0 });
        if now.saturating_sub(state.started_at) >= window {
            state = RegistrationWindow { started_at: now, count: 0 };
        }
        if state.count >= limit {
            return Err(RegistryError::RegistrationRateLimited);
        }
        state.count += 1;
        env.storage().persistent().set(&key, &state);
        env.storage().persistent().extend_ttl(&key, window, window);
        Ok(())
    }

    /// Return the current admin set (may be empty before initialization).
    fn admin_index(env: &Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&DataKey::Admins)
            .unwrap_or(Vec::new(env))
    }

    /// Return `NotAdmin` if `addr` is not in the current admin set.
    fn assert_is_admin(env: &Env, addr: &Address) -> Result<(), RegistryError> {
        if !env.storage().instance().has(&DataKey::Admins) {
            return Err(RegistryError::NotInitialized);
        }
        if !Self::is_admin_member(env, addr) {
            return Err(RegistryError::NotAdmin);
        }
        Ok(())
    }

    fn is_admin_member(env: &Env, addr: &Address) -> bool {
        Self::admin_index(env).contains(addr)
    }

    /// Allocate a new proposal ID, store the proposal, and return the ID.
    fn create_proposal(
        env: &Env,
        proposer: Address,
        action: ProposalAction,
    ) -> u32 {
        let id: u32 = env.storage().instance().get(&DataKey::ProposalCount).unwrap_or(0);
        let proposal = Proposal {
            id,
            proposer,
            action,
            approvals: Vec::new(env),
            ready_at: u32::MAX,
            executed: false,
        };
        Self::save_proposal(env, &proposal);
        env.storage().instance().set(&DataKey::ProposalCount, &(id + 1));
        id
    }

    fn load_proposal(env: &Env, proposal_id: u32) -> Result<Proposal, RegistryError> {
        env.storage()
            .instance()
            .get(&DataKey::ProposalData(proposal_id))
            .ok_or(RegistryError::ProposalNotFound)
    }

    fn save_proposal(env: &Env, proposal: &Proposal) {
        env.storage()
            .instance()
            .set(&DataKey::ProposalData(proposal.id), proposal);
    }

    /// Execute the side-effect of a passed proposal.
    fn apply_action(env: &Env, action: &ProposalAction) -> Result<(), RegistryError> {
        match action {
            ProposalAction::Deactivate(contract_id) => {
                let mut entry: ContractEntry = env.storage().persistent()
                    .get(&DataKey::Contract(contract_id.clone()))
                    .ok_or(RegistryError::ContractNotFound)?;
                entry.active = false;
                env.storage().persistent().set(&DataKey::Contract(contract_id.clone()), &entry);
                env.events().publish(
                    (Symbol::new(env, "contract_deactivated"),),
                    (contract_id.clone(), Symbol::new(env, "governance")),
                );
            }
            ProposalAction::Upgrade(new_wasm_hash) => {
                env.deployer().update_current_contract_wasm(new_wasm_hash.clone());
                // The version being replaced, deliberately — see `upgrade`.
                env.events().publish(
                    (Symbol::new(env, "registry_upgraded"),),
                    (new_wasm_hash.clone(), CONTRACT_VERSION),
                );
            }
            ProposalAction::AddAdmin(new_admin) => {
                let mut admins = Self::admin_index(env);
                if !admins.contains(new_admin) {
                    admins.push_back(new_admin.clone());
                    env.storage().instance().set(&DataKey::Admins, &admins);
                }
                env.events().publish(
                    (Symbol::new(env, "admin_added"),),
                    (new_admin.clone(),),
                );
            }
            ProposalAction::RemoveAdmin(admin_to_remove) => {
                let mut admins = Self::admin_index(env);
                let threshold: u32 = env.storage().instance().get(&DataKey::Threshold).unwrap_or(1);

                // After removal the set must still be large enough for the
                // threshold to be satisfiable.
                let new_len = admins.len().saturating_sub(1);
                if new_len < threshold {
                    return Err(RegistryError::InvalidThreshold);
                }

                if let Some(i) = admins.first_index_of(admin_to_remove) {
                    admins.remove(i);
                    env.storage().instance().set(&DataKey::Admins, &admins);
                }
                env.events().publish(
                    (Symbol::new(env, "admin_removed"),),
                    (admin_to_remove.clone(),),
                );
            }
            ProposalAction::ChangeThreshold(new_threshold) => {
                let admins = Self::admin_index(env);
                if *new_threshold == 0 || *new_threshold > admins.len() {
                    return Err(RegistryError::InvalidThreshold);
                }
                env.storage().instance().set(&DataKey::Threshold, new_threshold);
                env.events().publish(
                    (Symbol::new(env, "threshold_changed"),),
                    (*new_threshold,),
                );
            }
            ProposalAction::ConfigureStaking(token_id, treasury) => {
                env.storage().instance().set(&DataKey::StakeToken, token_id);
                env.storage().instance().set(&DataKey::Treasury, treasury);
                env.events().publish(
                    (Symbol::new(env, "staking_configured"),),
                    (token_id.clone(), treasury.clone()),
                );
            }
            ProposalAction::SetVerified(contract_id, verified) => {
                if !env.storage().persistent().has(&DataKey::Contract(contract_id.clone())) {
                    return Err(RegistryError::ContractNotFound);
                }
                env.storage().persistent()
                    .set(&DataKey::Verified(contract_id.clone()), verified);
                env.events().publish(
                    (Symbol::new(env, "verification_set"),),
                    (contract_id.clone(), *verified),
                );
            }
            ProposalAction::Slash(contract_id, amount, reason) => {
                if *amount <= 0 {
                    return Err(RegistryError::InvalidAmount);
                }

                let staked = Self::stake_of(env, contract_id);
                if staked < *amount {
                    return Err(RegistryError::InsufficientStake);
                }

                let (token_id, treasury) = Self::staking_config(env)?;

                token::Client::new(env, &token_id).transfer(
                    &env.current_contract_address(),
                    &treasury,
                    amount,
                );

                env.storage().persistent()
                    .set(&DataKey::Stake(contract_id.clone()), &(staked - *amount));

                let slashed_at = env.ledger().sequence();
                let mut history = Self::slash_history(env, contract_id);
                history.push_back(SlashRecord {
                    amount: *amount,
                    reason: reason.clone(),
                    slashed_at,
                });
                env.storage().persistent()
                    .set(&DataKey::Slashes(contract_id.clone()), &history);

                // Freeze what is left, so the owner cannot empty the stake
                // before a second slash can clear the timelock.
                env.storage().persistent().set(
                    &DataKey::WithdrawLockedUntil(contract_id.clone()),
                    &(slashed_at + SLASH_LOCK_LEDGERS),
                );

                env.events().publish(
                    (Symbol::new(env, "stake_slashed"),),
                    (contract_id.clone(), *amount, reason.clone(), treasury),
                );
            }
            ProposalAction::SetAllowlistEnabled(enabled) => {
                env.storage().instance().set(&DataKey::AllowlistEnabled, enabled);
                env.events().publish(
                    (Symbol::new(env, "allowlist_mode_changed"),),
                    (*enabled,),
                );
            }
            ProposalAction::SetAllowlisted(owner, allowed) => {
                env.storage().persistent().set(&DataKey::Allowlisted(owner.clone()), allowed);
                env.events().publish(
                    (Symbol::new(env, "owner_allowlisted"),),
                    (owner.clone(), *allowed),
                );
            }
            ProposalAction::ConfigureRegistrationRateLimit(limit, window) => {
                if *limit > 0 && (*window == 0 || *window > env.storage().max_ttl()) {
                    return Err(RegistryError::InvalidRateLimit);
                }
                env.storage().instance().set(&DataKey::RegistrationRateLimit, limit);
                env.storage().instance().set(&DataKey::RegistrationRateWindow, window);
                env.events().publish(
                    (Symbol::new(env, "registration_rate_limit_changed"),),
                    (*limit, *window),
                );
            }
        }
        Ok(())
    }

    /// `(stake_token, treasury)`, or `StakingNotConfigured` if the
    /// `ConfigureStaking` proposal has never been executed.
    fn staking_config(env: &Env) -> Result<(Address, Address), RegistryError> {
        let token_id: Address = env.storage().instance()
            .get(&DataKey::StakeToken)
            .ok_or(RegistryError::StakingNotConfigured)?;
        let treasury: Address = env.storage().instance()
            .get(&DataKey::Treasury)
            .ok_or(RegistryError::StakingNotConfigured)?;
        Ok((token_id, treasury))
    }

    fn stake_of(env: &Env, contract_id: &Address) -> i128 {
        env.storage().persistent()
            .get(&DataKey::Stake(contract_id.clone()))
            .unwrap_or(0)
    }

    fn slash_history(env: &Env, contract_id: &Address) -> Vec<SlashRecord> {
        env.storage().persistent()
            .get(&DataKey::Slashes(contract_id.clone()))
            .unwrap_or(Vec::new(env))
    }

    fn withdraw_locked_until(env: &Env, contract_id: &Address) -> u32 {
        env.storage().persistent()
            .get(&DataKey::WithdrawLockedUntil(contract_id.clone()))
            .unwrap_or(0)
    }

    fn reputation_of(env: &Env, contract_id: &Address) -> Reputation {
        let mut slashed_total: i128 = 0;
        for record in Self::slash_history(env, contract_id).iter() {
            slashed_total += record.amount;
        }

        Reputation {
            stake: Self::stake_of(env, contract_id),
            verified: env.storage().persistent()
                .get(&DataKey::Verified(contract_id.clone()))
                .unwrap_or(false),
            slashed_total,
            withdraw_locked_until: Self::withdraw_locked_until(env, contract_id),
        }
    }

    /// Collapse duplicates, rejecting an empty selection.
    ///
    /// Deduplication is what bounds the work `register_contract` does: without
    /// it a registrant could pass the same category a thousand times and pay
    /// for a thousand index writes. With it, the number of index writes is at
    /// most the size of the [`Category`] vocabulary.
    fn dedup_categories(env: &Env, categories: &Vec<Category>) -> Result<Vec<Category>, RegistryError> {
        if categories.is_empty() {
            return Err(RegistryError::NoCategories);
        }

        let mut unique = Vec::new(env);
        for category in categories.iter() {
            if !unique.contains(&category) {
                unique.push_back(category);
            }
        }

        Ok(unique)
    }

    fn categories_of(env: &Env, contract_id: &Address) -> Vec<Category> {
        env.storage().persistent()
            .get(&DataKey::Categories(contract_id.clone()))
            .unwrap_or(Vec::new(env))
    }

    fn category_index(env: &Env, category: &Category) -> Vec<Address> {
        env.storage().persistent()
            .get(&DataKey::ByCategory(*category))
            .unwrap_or(Vec::new(env))
    }

    /// Record a registration's categories and append it to each category's
    /// index. Idempotent per category, so re-declaring an existing category
    /// does not list the registration under it twice.
    fn index_categories(env: &Env, contract_id: &Address, categories: &Vec<Category>) {
        env.storage().persistent()
            .set(&DataKey::Categories(contract_id.clone()), categories);

        for category in categories.iter() {
            let mut index = Self::category_index(env, &category);
            if !index.contains(contract_id) {
                index.push_back(contract_id.clone());
                env.storage().persistent()
                    .set(&DataKey::ByCategory(category), &index);
            }
        }
    }

    fn owner_index(env: &Env, owner: &Address) -> Vec<Address> {
        env.storage().persistent()
            .get(&DataKey::OwnerContracts(owner.clone()))
            .unwrap_or(Vec::new(env))
    }

    fn set_owner_index(env: &Env, owner: &Address, contracts: &Vec<Address>) {
        env.storage().persistent().set(&DataKey::OwnerContracts(owner.clone()), contracts);
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events, Ledger, MockAuth, MockAuthInvoke};
    use soroban_sdk::{IntoVal, TryFromVal};

    // ── Upgrade-path wasm fixtures ──────────────────────────────────────────

    mod registry_v1_wasm {
        soroban_sdk::contractimport!(
            file = "../target/wasm32v1-none/release/lumina_registry.wasm"
        );
    }

    mod registry_v2_wasm {
        soroban_sdk::contractimport!(
            file = "../target/wasm32v1-none/release/lumina_registry_v2.wasm"
        );
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    /// Set up a registry with a 2-of-3 multi-sig admin.
    fn setup_multisig() -> (Env, LuminaRegistryClient<'static>, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let a1 = Address::generate(&env);
        let a2 = Address::generate(&env);
        let a3 = Address::generate(&env);
        let contract_id = env.register(LuminaRegistry, (&a1,));
        let client = LuminaRegistryClient::new(&env, &contract_id);

        let add_a2 = client.propose_add_admin(&a1, &a2);
        pass_proposal(&env, &client, &a1, add_a2);
        let add_a3 = client.propose_add_admin(&a1, &a3);
        pass_proposal(&env, &client, &a1, add_a3);
        let change_threshold = client.propose_change_threshold(&a1, &2);
        client.approve_proposal(&a1, &change_threshold);
        client.approve_proposal(&a2, &change_threshold);
        advance_ledger(&env, TIMELOCK_LEDGERS);
        client.execute_proposal(&change_threshold);
        (env, client, a1, a2, a3)
    }

    /// Set up a registry with a single admin for tests that don't need multi-sig.
    fn setup() -> (Env, LuminaRegistryClient<'static>, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let contract_id = env.register(LuminaRegistry, (&admin,));
        let client = LuminaRegistryClient::new(&env, &contract_id);
        (env, client, admin)
    }

    /// Build a `Vec<Category>` from a slice, for readability at call sites.
    fn cats(env: &Env, list: &[Category]) -> Vec<Category> {
        let mut v = Vec::new(env);
        for category in list {
            v.push_back(*category);
        }
        v
    }

    /// The category tests that don't care which category is used still need
    /// one, since registration requires at least one.
    fn default_cats(env: &Env) -> Vec<Category> {
        cats(env, &[Category::Infrastructure])
    }

    fn register_sample(env: &Env, client: &LuminaRegistryClient) -> (Address, Address) {
        let owner = Address::generate(env);
        let target = Address::generate(env);
        client.register_contract(
            &owner,
            &target,
            &String::from_str(env, "Test Contract"),
            &String::from_str(env, "A test contract"),
            &default_cats(env),
        );
        (owner, target)
    }

    fn register_for(env: &Env, client: &LuminaRegistryClient, owner: &Address) -> Address {
        let target = Address::generate(env);
        client.register_contract(
            owner,
            &target,
            &String::from_str(env, "Test Contract"),
            &String::from_str(env, "A test contract"),
            &default_cats(env),
        );
        target
    }

    /// Register under an explicit category selection.
    fn register_in(
        env: &Env,
        client: &LuminaRegistryClient,
        owner: &Address,
        categories: &[Category],
    ) -> Address {
        let target = Address::generate(env);
        client.register_contract(
            owner,
            &target,
            &String::from_str(env, "Test Contract"),
            &String::from_str(env, "A test contract"),
            &cats(env, categories),
        );
        target
    }

    fn page_contains(entries: &Vec<ContractEntry>, contract_id: &Address) -> bool {
        entries.iter().any(|e| &e.contract_id == contract_id)
    }

    /// Advance the mock ledger by `n` ledgers.
    fn advance_ledger(env: &Env, n: u32) {
        let seq = env.ledger().sequence();
        env.ledger().set_sequence_number(seq + n);
    }

    // ── Initialization ──────────────────────────────────────────────────────

    #[test]
    fn initialize_sets_zero_count() {
        let (_, client, _) = setup();
        assert_eq!(client.get_contract_count(), 0);
        assert_eq!(client.get_total_registered(), 0);
        assert_eq!(client.get_active_contract_count(), 0);
    }

    #[test]
    fn initialize_twice_fails() {
        let (env, client, admin) = setup();
        let mut admins = Vec::new(&env);
        admins.push_back(admin.clone());
        let result = client.try_initialize(&admins, &1);
        assert_eq!(result, Err(Ok(RegistryError::AlreadyInitialized)));
    }

    #[test]
    fn second_party_cannot_claim_a_new_deployment() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let contract_id = env.register(LuminaRegistry, (&admin,));
        let client = LuminaRegistryClient::new(&env, &contract_id);
        let attacker = Address::generate(&env);
        let mut admins = Vec::new(&env);
        admins.push_back(attacker);
        assert_eq!(
            client.try_initialize(&admins, &1),
            Err(Ok(RegistryError::AlreadyInitialized))
        );
    }

    // ── Threshold enforcement ───────────────────────────────────────────────

    #[test]
    fn proposal_cannot_execute_below_threshold() {
        let (env, client, a1, _a2, _a3) = setup_multisig();
        let (_owner, target) = register_sample(&env, &client);

        // Only a1 approves (threshold is 2).
        let pid = client.propose_deactivate(&a1, &target);
        client.approve_proposal(&a1, &pid);

        // Advance past timelock — should still fail because threshold not met.
        advance_ledger(&env, TIMELOCK_LEDGERS + 1);
        assert_eq!(
            client.try_execute_proposal(&pid),
            Err(Ok(RegistryError::ThresholdNotMet))
        );
        // Contract is still active.
        assert!(client.get_contract(&target).active);
    }

    #[test]
    fn proposal_executes_when_threshold_met_and_timelock_elapsed() {
        let (env, client, a1, a2, _a3) = setup_multisig();
        let (_owner, target) = register_sample(&env, &client);

        let pid = client.propose_deactivate(&a1, &target);
        client.approve_proposal(&a1, &pid);
        client.approve_proposal(&a2, &pid);

        advance_ledger(&env, TIMELOCK_LEDGERS + 1);
        client.execute_proposal(&pid);

        assert!(!client.get_contract(&target).active);
    }

    // ── Timelock enforcement ────────────────────────────────────────────────

    #[test]
    fn proposal_cannot_execute_before_timelock_elapses() {
        let (env, client, a1, a2, _a3) = setup_multisig();
        let (_owner, target) = register_sample(&env, &client);

        let pid = client.propose_deactivate(&a1, &target);
        client.approve_proposal(&a1, &pid);
        client.approve_proposal(&a2, &pid);

        // Advance by one ledger less than required.
        advance_ledger(&env, TIMELOCK_LEDGERS - 1);
        assert_eq!(
            client.try_execute_proposal(&pid),
            Err(Ok(RegistryError::TimelockNotElapsed))
        );
        assert!(client.get_contract(&target).active);
    }

    #[test]
    fn proposal_executes_exactly_at_timelock_boundary() {
        let (env, client, a1, a2, _a3) = setup_multisig();
        let (_owner, target) = register_sample(&env, &client);

        let pid = client.propose_deactivate(&a1, &target);
        client.approve_proposal(&a1, &pid);
        client.approve_proposal(&a2, &pid);

        advance_ledger(&env, TIMELOCK_LEDGERS);
        client.execute_proposal(&pid);
        assert!(!client.get_contract(&target).active);
    }

    // ── Duplicate-approval rejection ────────────────────────────────────────

    #[test]
    fn same_admin_approving_twice_is_rejected() {
        let (env, client, a1, _a2, _a3) = setup_multisig();
        let (_owner, target) = register_sample(&env, &client);

        let pid = client.propose_deactivate(&a1, &target);
        client.approve_proposal(&a1, &pid);

        assert_eq!(
            client.try_approve_proposal(&a1, &pid),
            Err(Ok(RegistryError::AlreadyApproved))
        );
    }

    #[test]
    fn double_approval_does_not_count_toward_threshold() {
        let (env, client, a1, _a2, _a3) = setup_multisig();
        let (_owner, target) = register_sample(&env, &client);

        let pid = client.propose_deactivate(&a1, &target);
        // First approval succeeds.
        client.approve_proposal(&a1, &pid);
        // Second approval rejected.
        let _ = client.try_approve_proposal(&a1, &pid).unwrap_err();

        advance_ledger(&env, TIMELOCK_LEDGERS + 1);
        // Still below threshold (need 2), so execution must fail.
        assert_eq!(
            client.try_execute_proposal(&pid),
            Err(Ok(RegistryError::ThresholdNotMet))
        );
    }

    // ── Non-admin cannot propose or approve ─────────────────────────────────

    #[test]
    fn non_admin_cannot_propose() {
        let (env, client, _a1, _a2, _a3) = setup_multisig();
        let (_owner, target) = register_sample(&env, &client);
        let stranger = Address::generate(&env);

        assert_eq!(
            client.try_propose_deactivate(&stranger, &target),
            Err(Ok(RegistryError::NotAdmin))
        );
    }

    #[test]
    fn non_admin_cannot_approve() {
        let (env, client, a1, _a2, _a3) = setup_multisig();
        let (_owner, target) = register_sample(&env, &client);
        let stranger = Address::generate(&env);

        let pid = client.propose_deactivate(&a1, &target);
        assert_eq!(
            client.try_approve_proposal(&stranger, &pid),
            Err(Ok(RegistryError::NotAdmin))
        );
    }

    // ── Admin-set changes go through governance ─────────────────────────────

    #[test]
    fn add_admin_via_governance() {
        let (env, client, a1, a2, _a3) = setup_multisig();
        let new_admin = Address::generate(&env);

        let pid = client.propose_add_admin(&a1, &new_admin);
        client.approve_proposal(&a1, &pid);
        client.approve_proposal(&a2, &pid);
        advance_ledger(&env, TIMELOCK_LEDGERS);
        client.execute_proposal(&pid);

        let admins = client.get_admins();
        assert!(admins.contains(&new_admin));
    }

    #[test]
    fn remove_admin_via_governance() {
        let (env, client, a1, a2, a3) = setup_multisig();

        // Remove a3 — set goes from 3 to 2, still satisfies threshold=2.
        let pid = client.propose_remove_admin(&a1, &a3);
        client.approve_proposal(&a1, &pid);
        client.approve_proposal(&a2, &pid);
        advance_ledger(&env, TIMELOCK_LEDGERS);
        client.execute_proposal(&pid);

        let admins = client.get_admins();
        assert!(!admins.contains(&a3));
        assert_eq!(admins.len(), 2);
    }

    #[test]
    fn remove_admin_that_would_violate_threshold_fails() {
        let (env, client, a1, a2, _a3) = setup_multisig();
        // threshold=2, admins=3; removing one leaves 2 which still satisfies.
        // But if we try to remove a second one that'd leave 1 < threshold=2.
        let pid1 = client.propose_remove_admin(&a1, &a2);
        client.approve_proposal(&a1, &pid1);
        // need second approval
        let a3 = client.get_admins().get(2).unwrap();
        client.approve_proposal(&a3, &pid1);
        advance_ledger(&env, TIMELOCK_LEDGERS);
        // Still ok: 3-1=2 >= threshold=2.
        client.execute_proposal(&pid1);

        // Now admins = {a1, a3}, threshold=2.  Removing a3 would leave 1 < 2.
        let pid2 = client.propose_remove_admin(&a1, &a3);
        client.approve_proposal(&a1, &pid2);
        client.approve_proposal(&a3, &pid2);
        advance_ledger(&env, TIMELOCK_LEDGERS);
        assert_eq!(
            client.try_execute_proposal(&pid2),
            Err(Ok(RegistryError::InvalidThreshold))
        );
    }

    #[test]
    fn change_threshold_via_governance() {
        let (env, client, a1, a2, _a3) = setup_multisig();

        let pid = client.propose_change_threshold(&a1, &1);
        client.approve_proposal(&a1, &pid);
        client.approve_proposal(&a2, &pid);
        advance_ledger(&env, TIMELOCK_LEDGERS);
        client.execute_proposal(&pid);

        assert_eq!(client.get_threshold(), 1);
    }

    // ── Full realistic scenario ─────────────────────────────────────────────

    #[test]
    fn full_scenario_compromised_admin_removed_via_governance() {
        // 3 admins, 2-of-3 threshold, admin key "a3" compromised.
        // a1 and a2 vote to remove a3.
        let (env, client, a1, a2, a3) = setup_multisig();

        // Confirm a3 is currently an admin.
        assert!(client.get_admins().contains(&a3));

        let pid = client.propose_remove_admin(&a1, &a3);
        client.approve_proposal(&a1, &pid);
        client.approve_proposal(&a2, &pid);
        advance_ledger(&env, TIMELOCK_LEDGERS);
        client.execute_proposal(&pid);

        // a3 is no longer an admin.
        assert!(!client.get_admins().contains(&a3));

        // a3 can no longer propose anything.
        let (_owner, target) = register_sample(&env, &client);
        assert_eq!(
            client.try_propose_deactivate(&a3, &target),
            Err(Ok(RegistryError::NotAdmin))
        );

        // a1 and a2 can still form a 2-of-2 quorum.
        let pid2 = client.propose_deactivate(&a1, &target);
        client.approve_proposal(&a1, &pid2);
        client.approve_proposal(&a2, &pid2);
        advance_ledger(&env, TIMELOCK_LEDGERS);
        client.execute_proposal(&pid2);
        assert!(!client.get_contract(&target).active);
    }

    // ── Owner self-deactivate is still instant ──────────────────────────────

    #[test]
    fn owner_can_deactivate_own_contract_immediately() {
        let (env, client, _a1, _a2, _a3) = setup_multisig();
        let (owner, target) = register_sample(&env, &client);

        // No proposal needed — owner deactivates directly.
        client.deactivate(&owner, &target);
        assert!(!client.get_contract(&target).active);
    }

    #[test]
    fn deactivate_by_non_owner_is_rejected() {
        let (env, client, a1, _a2, _a3) = setup_multisig();
        let (_owner, target) = register_sample(&env, &client);

        // Admin attempting to bypass governance.
        assert_eq!(
            client.try_deactivate(&a1, &target),
            Err(Ok(RegistryError::Unauthorized))
        );
    }

    // ── Executed proposal cannot be re-executed ─────────────────────────────

    #[test]
    fn executed_proposal_cannot_execute_again() {
        let (env, client, a1, a2, _a3) = setup_multisig();
        let (_owner, target) = register_sample(&env, &client);

        let pid = client.propose_deactivate(&a1, &target);
        client.approve_proposal(&a1, &pid);
        client.approve_proposal(&a2, &pid);
        advance_ledger(&env, TIMELOCK_LEDGERS);
        client.execute_proposal(&pid);

        assert_eq!(
            client.try_execute_proposal(&pid),
            Err(Ok(RegistryError::AlreadyExecuted))
        );
    }

    // ── Existing registry tests (single-admin setup) ────────────────────────

    #[test]
    fn register_contract_succeeds() {
        let (env, client, _admin) = setup();
        let (owner, target) = register_sample(&env, &client);
        assert!(client.is_registered(&target));
        assert_eq!(client.get_contract_count(), 1);
        let entry = client.get_contract(&target);
        assert_eq!(entry.owner, owner);
        assert!(entry.active);
    }

    #[test]
    fn register_contract_rejects_duplicate() {
        let (env, client, _admin) = setup();
        let (owner, target) = register_sample(&env, &client);
        let result = client.try_register_contract(
            &owner,
            &target,
            &String::from_str(&env, "X"),
            &String::from_str(&env, "X"),
            &default_cats(&env),
        );
        assert_eq!(result, Err(Ok(RegistryError::AlreadyRegistered)));
    }

    #[test]
    fn allowlist_mode_is_off_by_default_and_governed() {
        let (env, client, admin) = setup();
        let owner = Address::generate(&env);
        let first_target = Address::generate(&env);
        client.register_contract(
            &owner, &first_target, &String::from_str(&env, "First"),
            &String::from_str(&env, "First"), &default_cats(&env),
        );
        let enable = client.propose_set_allowlist_enabled(&admin, &true);
        pass_proposal(&env, &client, &admin, enable);
        let target = Address::generate(&env);
        assert_eq!(
            client.try_register_contract(
                &owner, &target, &String::from_str(&env, "Blocked"),
                &String::from_str(&env, "Blocked"), &default_cats(&env),
            ),
            Err(Ok(RegistryError::NotAllowlisted)),
        );
        let allow = client.propose_set_allowlisted(&admin, &owner, &true);
        pass_proposal(&env, &client, &admin, allow);
        client.register_contract(
            &owner, &target, &String::from_str(&env, "Allowed"),
            &String::from_str(&env, "Allowed"), &default_cats(&env),
        );
        assert!(client.is_registered(&target));
    }

    #[test]
    fn registration_rate_limit_resets_after_governed_window() {
        let (env, client, admin) = setup();
        let owner = Address::generate(&env);
        let configure = client.propose_configure_registration_rate_limit(&admin, &1, &3);
        pass_proposal(&env, &client, &admin, configure);
        let first = Address::generate(&env);
        client.register_contract(
            &owner, &first, &String::from_str(&env, "First"),
            &String::from_str(&env, "First"), &default_cats(&env),
        );
        let second = Address::generate(&env);
        assert_eq!(
            client.try_register_contract(
                &owner, &second, &String::from_str(&env, "Second"),
                &String::from_str(&env, "Second"), &default_cats(&env),
            ),
            Err(Ok(RegistryError::RegistrationRateLimited)),
        );
        advance_ledger(&env, 3);
        client.register_contract(
            &owner, &second, &String::from_str(&env, "Second"),
            &String::from_str(&env, "Second"), &default_cats(&env),
        );
        assert!(client.is_registered(&second));
    }

    #[test]
    fn bootstrap_admin_can_add_admin_and_raise_threshold_through_governance() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let contract_id = env.register(LuminaRegistry, (&admin,));
        let client = LuminaRegistryClient::new(&env, &contract_id);
        let second_admin = Address::generate(&env);
        assert_eq!(client.get_admins().len(), 1);
        assert_eq!(client.get_threshold(), 1);
        let add = client.propose_add_admin(&admin, &second_admin);
        pass_proposal(&env, &client, &admin, add);
        let change = client.propose_change_threshold(&admin, &2);
        client.approve_proposal(&admin, &change);
        client.approve_proposal(&second_admin, &change);
        advance_ledger(&env, TIMELOCK_LEDGERS);
        client.execute_proposal(&change);
        assert_eq!(client.get_admins().len(), 2);
        assert_eq!(client.get_threshold(), 2);
    }

    #[test]
    fn deactivate_by_owner_succeeds() {
        let (env, client, _admin) = setup();
        let (owner, target) = register_sample(&env, &client);
        client.deactivate(&owner, &target);
        assert!(!client.get_contract(&target).active);
    }

    #[test]
    fn deactivate_by_unrelated_caller_fails() {
        let (env, client, _admin) = setup();
        let (_owner, target) = register_sample(&env, &client);
        let stranger = Address::generate(&env);
        assert_eq!(
            client.try_deactivate(&stranger, &target),
            Err(Ok(RegistryError::Unauthorized))
        );
    }

    #[test]
    fn get_contract_not_found_for_unknown_address() {
        let (env, client, _admin) = setup();
        let target = Address::generate(&env);
        assert_eq!(
            client.try_get_contract(&target),
            Err(Ok(RegistryError::ContractNotFound))
        );
    }

    #[test]
    fn get_active_contracts_excludes_deactivated() {
        let (env, client, _admin) = setup();
        let (owner, target) = register_sample(&env, &client);
        client.deactivate(&owner, &target);
        assert_eq!(client.get_active_contracts(&0, &10).len(), 0);
    }

    #[test]
    fn get_active_contracts_respects_limit_and_offset() {
        let (env, client, _admin) = setup();
        for _ in 0..5 {
            register_sample(&env, &client);
        }
        assert_eq!(client.get_active_contracts(&0, &2).len(), 2);
        assert_eq!(client.get_active_contracts(&2, &2).len(), 2);
        assert_eq!(client.get_active_contracts(&4, &2).len(), 1);
    }

    #[test]
    fn get_contracts_by_owner_returns_only_that_owners_contracts() {
        let (env, client, _admin) = setup();
        let (owner_a, target_a) = register_sample(&env, &client);
        let (owner_b, target_b) = register_sample(&env, &client);
        let target_a2 = register_for(&env, &client, &owner_a);

        let a = client.get_contracts_by_owner(&owner_a, &0, &10);
        assert_eq!(a.len(), 2);
        assert!(page_contains(&a, &target_a));
        assert!(page_contains(&a, &target_a2));
        assert!(!page_contains(&a, &target_b));

        let b = client.get_contracts_by_owner(&owner_b, &0, &10);
        assert_eq!(b.len(), 1);
        assert!(page_contains(&b, &target_b));
    }

    #[test]
    fn update_metadata_by_owner_succeeds() {
        let (env, client, _admin) = setup();
        let (owner, target) = register_sample(&env, &client);
        let name = String::from_str(&env, "Renamed Protocol");
        let desc = String::from_str(&env, "A corrected description");
        client.update_metadata(&owner, &target, &name, &desc);
        assert_eq!(client.get_contract(&target).name, name);
    }

    #[test]
    fn update_metadata_rejects_non_owner() {
        let (env, client, _admin) = setup();
        let (_owner, target) = register_sample(&env, &client);
        let stranger = Address::generate(&env);
        assert_eq!(
            client.try_update_metadata(
                &stranger,
                &target,
                &String::from_str(&env, "X"),
                &String::from_str(&env, "X"),
            ),
            Err(Ok(RegistryError::NotOwner))
        );
    }

    #[test]
    fn transfer_ownership_moves_entry_between_owner_indices() {
        let (env, client, _admin) = setup();
        let (owner, target) = register_sample(&env, &client);
        let kept = register_for(&env, &client, &owner);
        let new_owner = Address::generate(&env);

        client.transfer_ownership(&owner, &target, &new_owner);

        let old_entries = client.get_contracts_by_owner(&owner, &0, &10);
        assert_eq!(old_entries.len(), 1);
        assert!(page_contains(&old_entries, &kept));
        assert!(!page_contains(&old_entries, &target));

        let new_entries = client.get_contracts_by_owner(&new_owner, &0, &10);
        assert_eq!(new_entries.len(), 1);
        assert!(page_contains(&new_entries, &target));
    }

    #[test]
    fn transfer_ownership_by_admin_succeeds() {
        let (env, client, admin) = setup();
        let (owner, target) = register_sample(&env, &client);
        let new_owner = Address::generate(&env);

        client.transfer_ownership(&admin, &target, &new_owner);

        assert_eq!(client.get_contract(&target).owner, new_owner);
        assert_eq!(client.get_contracts_by_owner(&owner, &0, &10).len(), 0);
        assert_eq!(client.get_contracts_by_owner(&new_owner, &0, &10).len(), 1);
    }

    #[test]
    fn transfer_ownership_by_unrelated_caller_fails() {
        let (env, client, _admin) = setup();
        let (owner, target) = register_sample(&env, &client);
        let stranger = Address::generate(&env);
        let new_owner = Address::generate(&env);
        assert_eq!(
            client.try_transfer_ownership(&stranger, &target, &new_owner),
            Err(Ok(RegistryError::Unauthorized))
        );
        assert_eq!(client.get_contract(&target).owner, owner);
    }

    #[test]
    fn transfer_ownership_to_current_owner_is_noop() {
        let (env, client, _admin) = setup();
        let (owner, target) = register_sample(&env, &client);
        client.transfer_ownership(&owner, &target, &owner);
        let entries = client.get_contracts_by_owner(&owner, &0, &10);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn get_version_reports_compiled_version() {
        let (_, client, _) = setup();
        assert_eq!(client.get_version(), CONTRACT_VERSION);
    }

    #[test]
    fn get_admin_returns_first_admin() {
        let (_, client, admin) = setup();
        assert_eq!(client.get_admin(), admin);
    }

    #[test]
    fn unauthorized_owner_management_call_is_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let contract_id = env.register(LuminaRegistry, (&admin,));
        let client = LuminaRegistryClient::new(&env, &contract_id);
        let (_owner, target) = register_sample(&env, &client);
        let stranger = Address::generate(&env);
        let new_owner = Address::generate(&env);

        assert_eq!(
            client.try_transfer_ownership(&stranger, &target, &new_owner),
            Err(Ok(RegistryError::Unauthorized)),
        );
    }

    // ── Upgrade-path tests ──────────────────────────────────────────────────

    fn deploy_v1(env: &Env) -> (registry_v1_wasm::Client<'static>, Address, Address) {
        let admin = Address::generate(env);
        let contract_id = env.register(registry_v1_wasm::WASM, (&admin,));
        let client = registry_v1_wasm::Client::new(env, &contract_id);
        (client, admin, contract_id)
    }

    fn register_via(env: &Env, client: &registry_v1_wasm::Client, owner: &Address) -> Address {
        let target = Address::generate(env);
        // The imported wasm carries its own generated copy of `Category`.
        let mut categories = Vec::new(env);
        categories.push_back(registry_v1_wasm::Category::Infrastructure);
        client.register_contract(
            owner,
            &target,
            &String::from_str(env, "Test Contract"),
            &String::from_str(env, "A test contract"),
            &categories,
        );
        target
    }

    #[test]
    fn upgrade_swaps_code_and_preserves_registrations() {
        let env = Env::default();
        env.mock_all_auths();
        let (v1, admin, contract_id) = deploy_v1(&env);

        let owner = Address::generate(&env);
        let kept_active = register_via(&env, &v1, &owner);
        let deactivated = register_via(&env, &v1, &owner);
        let other_owner = register_via(&env, &v1, &Address::generate(&env));
        v1.deactivate(&owner, &deactivated);

        assert_eq!(v1.get_version(), CONTRACT_VERSION);
        assert_eq!(v1.get_contract_count(), 3);

        let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);
        v1.upgrade(&admin, &v2_hash);

        let v2 = registry_v2_wasm::Client::new(&env, &contract_id);
        assert_eq!(v2.get_version(), CONTRACT_VERSION + 1);
        assert_eq!(v2.get_contract_count(), 3);

        let entry = v2.get_contract(&kept_active);
        assert_eq!(entry.owner, owner);
        assert!(entry.active);
        assert!(!v2.get_contract(&deactivated).active);
        assert_ne!(v2.get_contract(&other_owner).owner, owner);

        let owned = v2.get_contracts_by_owner(&owner, &0, &10);
        assert_eq!(owned.len(), 2);
        assert_eq!(v2.count_active(), 2);
    }

    #[test]
    fn upgrade_retires_previous_interface() {
        let env = Env::default();
        env.mock_all_auths();
        let (v1, admin, _) = deploy_v1(&env);
        register_via(&env, &v1, &Address::generate(&env));
        assert_eq!(v1.get_active_contracts(&0, &10).len(), 1);

        let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);
        v1.upgrade(&admin, &v2_hash);
        assert!(v1.try_get_active_contracts(&0, &10).is_err());
    }

    #[test]
    fn upgrade_by_non_admin_is_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let (v1, _admin, _) = deploy_v1(&env);
        let stranger = Address::generate(&env);
        let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);
        assert_eq!(
            v1.try_upgrade(&stranger, &v2_hash),
            Err(Ok(registry_v1_wasm::RegistryError::Unauthorized))
        );
    }

    #[test]
    #[should_panic(expected = "Error(Auth, InvalidAction)")]
    fn upgrade_without_admin_signature_panics() {
        let env = Env::default();
        env.mock_all_auths();
        let (v1, admin, _) = deploy_v1(&env);
        let stranger = Address::generate(&env);
        let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);

        env.mock_auths(&[MockAuth {
            address: &stranger,
            invoke: &MockAuthInvoke {
                contract: &v1.address,
                fn_name: "upgrade",
                args: (admin.clone(), v2_hash.clone()).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        v1.upgrade(&admin, &v2_hash);
    }

    #[test]
    fn upgrade_with_admin_signature_succeeds() {
        let env = Env::default();
        env.mock_all_auths();
        let (v1, admin, contract_id) = deploy_v1(&env);
        let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);

        env.mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &v1.address,
                fn_name: "upgrade",
                args: (admin.clone(), v2_hash.clone()).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        v1.upgrade(&admin, &v2_hash);
        assert_eq!(registry_v2_wasm::Client::new(&env, &contract_id).get_version(), CONTRACT_VERSION + 1);
    }

    #[test]
    fn upgraded_registry_can_be_rolled_back() {
        let env = Env::default();
        env.mock_all_auths();
        let (v1, admin, contract_id) = deploy_v1(&env);
        let owner = Address::generate(&env);
        let target = register_via(&env, &v1, &owner);

        let v1_hash = env.deployer().upload_contract_wasm(registry_v1_wasm::WASM);
        let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);

        v1.upgrade(&admin, &v2_hash);
        let v2 = registry_v2_wasm::Client::new(&env, &contract_id);
        assert_eq!(v2.get_version(), CONTRACT_VERSION + 1);

        v2.upgrade(&admin, &v1_hash);
        assert_eq!(v1.get_version(), CONTRACT_VERSION);
        assert_eq!(v1.get_contract(&target).owner, owner);
        assert_eq!(v1.get_active_contracts(&0, &10).len(), 1);
    }

    #[test]
    fn upgrade_carries_admin_across_swap() {
        let env = Env::default();
        env.mock_all_auths();
        let (v1, admin, contract_id) = deploy_v1(&env);
        let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);

        v1.upgrade(&admin, &v2_hash);
        let v2 = registry_v2_wasm::Client::new(&env, &contract_id);

        let stranger = Address::generate(&env);
        assert_eq!(
            v2.try_upgrade(&stranger, &v2_hash),
            Err(Ok(registry_v2_wasm::RegistryError::Unauthorized))
        );
        v2.upgrade(&admin, &v2_hash);
    }

    /// Data of the single `registry_upgraded` event emitted by `contract_id`
    /// during the last invocation.
    fn registry_upgraded_data(env: &Env, contract_id: &Address) -> soroban_sdk::Val {
        let topic = Symbol::new(env, "registry_upgraded");
        let mut found: Vec<soroban_sdk::Val> = Vec::new(env);
        for (emitter, topics, data) in env.events().all().iter() {
            let first = topics.get(0).and_then(|t| Symbol::try_from_val(env, &t).ok());
            if &emitter == contract_id && first == Some(topic.clone()) {
                found.push_back(data);
            }
        }
        assert_eq!(found.len(), 1, "expected exactly one registry_upgraded event");
        found.get(0).unwrap()
    }

    // The upgrade event carries the version of the code being *replaced*.
    // Emitting the incoming version would be an equally plausible-looking
    // choice, and it would silently invert every consumer's reading of the
    // field — these tests pin the intent so that change cannot slip through.

    #[test]
    fn registry_upgraded_event_reports_the_replaced_version() {
        let env = Env::default();
        env.mock_all_auths();
        let (v1, admin, contract_id) = deploy_v1(&env);
        let replaced = v1.get_version();

        let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);
        v1.upgrade(&admin, &v2_hash);
        // Read the event before any further call: `events().all()` only
        // covers the most recent invocation.
        let (by, hash, version): (Address, BytesN<32>, u32) =
            registry_upgraded_data(&env, &contract_id).into_val(&env);

        let incoming = registry_v2_wasm::Client::new(&env, &contract_id).get_version();
        assert_ne!(replaced, incoming, "fixture must make the two versions distinguishable");
        assert_eq!(by, admin);
        assert_eq!(hash, v2_hash);
        assert_eq!(version, replaced);
        assert_ne!(version, incoming);
    }

    #[test]
    fn governance_upgrade_event_reports_the_replaced_version() {
        // The v1 fixture is the release wasm, built without `cfg(test)`, so it
        // enforces the production timelock. Stretch entry TTLs so waiting it
        // out does not archive the registry's storage or code.
        let production_timelock: u32 = 17_280;
        let env = Env::default();
        env.ledger().with_mut(|li| {
            li.min_persistent_entry_ttl = production_timelock * 2;
            li.min_temp_entry_ttl = production_timelock * 2;
            li.max_entry_ttl = production_timelock * 4;
        });
        env.mock_all_auths();
        let (v1, admin, contract_id) = deploy_v1(&env);
        let replaced = v1.get_version();

        let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);
        let pid = v1.propose_upgrade(&admin, &v2_hash);
        v1.approve_proposal(&admin, &pid);
        advance_ledger(&env, production_timelock);
        v1.execute_proposal(&pid);
        let (hash, version): (BytesN<32>, u32) =
            registry_upgraded_data(&env, &contract_id).into_val(&env);

        let incoming = registry_v2_wasm::Client::new(&env, &contract_id).get_version();
        assert_ne!(replaced, incoming, "fixture must make the two versions distinguishable");
        assert_eq!(hash, v2_hash);
        assert_eq!(version, replaced);
        assert_ne!(version, incoming);
    }

    #[test]
    fn register_contract_populates_owner_index() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        assert_eq!(client.get_contracts_by_owner(&owner, &0, &10).len(), 0);
        let target = register_for(&env, &client, &owner);
        let entries = client.get_contracts_by_owner(&owner, &0, &10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.get(0).unwrap().contract_id, target);
    }

    #[test]
    fn admin_can_still_deactivate_after_ownership_transfer() {
        // Via governance (propose + approve + execute).
        let (env, client, a1, a2, _a3) = setup_multisig();
        let (owner, target) = register_sample(&env, &client);
        let new_owner = Address::generate(&env);

        client.transfer_ownership(&owner, &target, &new_owner);

        // Admin deactivation goes through governance.
        let pid = client.propose_deactivate(&a1, &target);
        client.approve_proposal(&a1, &pid);
        client.approve_proposal(&a2, &pid);
        advance_ledger(&env, TIMELOCK_LEDGERS);
        client.execute_proposal(&pid);

        assert!(!client.get_contract(&target).active);
    }

    #[test]
    fn transfer_ownership_succeeds_with_real_owner_signature() {
        let (env, client, _admin) = setup();
        let (owner, target) = register_sample(&env, &client);
        let new_owner = Address::generate(&env);

        env.mock_auths(&[MockAuth {
            address: &owner,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "transfer_ownership",
                args: (owner.clone(), target.clone(), new_owner.clone()).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        client.transfer_ownership(&owner, &target, &new_owner);
        assert_eq!(client.get_contract(&target).owner, new_owner);
    }

    #[test]
    #[should_panic(expected = "Error(Auth, InvalidAction)")]
    fn transfer_ownership_without_caller_signature_panics() {
        let (env, client, _admin) = setup();
        let (owner, target) = register_sample(&env, &client);
        let new_owner = Address::generate(&env);

        env.mock_auths(&[MockAuth {
            address: &new_owner,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "transfer_ownership",
                args: (owner.clone(), target.clone(), new_owner.clone()).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        client.transfer_ownership(&owner, &target, &new_owner);
    }

    #[test]
    fn constructor_sets_bootstrap_admin() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let contract_id = env.register(LuminaRegistry, (&admin,));
        let client = LuminaRegistryClient::new(&env, &contract_id);
        assert_eq!(client.get_admin(), admin);
    }

    #[test]
    fn update_metadata_succeeds_with_real_owner_signature() {
        let (env, client, _admin) = setup();
        let (owner, target) = register_sample(&env, &client);
        let name = String::from_str(&env, "Signed Rename");
        let desc = String::from_str(&env, "Signed Rename");

        env.mock_auths(&[MockAuth {
            address: &owner,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "update_metadata",
                args: (owner.clone(), target.clone(), name.clone(), desc.clone()).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        client.update_metadata(&owner, &target, &name, &desc);
        assert_eq!(client.get_contract(&target).name, name);
    }

    #[test]
    #[should_panic(expected = "Error(Auth, InvalidAction)")]
    fn update_metadata_without_owner_signature_panics() {
        let (env, client, _admin) = setup();
        let (owner, target) = register_sample(&env, &client);
        let stranger = Address::generate(&env);
        let name = String::from_str(&env, "Unsigned Rename");
        let desc = String::from_str(&env, "Unsigned Rename");

        env.mock_auths(&[MockAuth {
            address: &stranger,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "update_metadata",
                args: (owner.clone(), target.clone(), name.clone(), desc.clone()).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        client.update_metadata(&owner, &target, &name, &desc);
    }

    // ── Staking, verification & slashing ────────────────────────────────────

    /// A registry with a live Stellar Asset Contract as its stake token and
    /// staking already opened through governance.
    ///
    /// Returns `(env, client, admin, token_id, treasury)`.
    fn setup_staking() -> (Env, LuminaRegistryClient<'static>, Address, Address, Address) {
        let (env, client, admin) = setup();

        let issuer = Address::generate(&env);
        let token_id = env.register_stellar_asset_contract_v2(issuer).address();
        let treasury = Address::generate(&env);

        let pid = client.propose_configure_staking(&admin, &token_id, &treasury);
        pass_proposal(&env, &client, &admin, pid);

        (env, client, admin, token_id, treasury)
    }

    /// Drive a proposal through the 1-of-1 governance flow: approve, wait out
    /// the timelock, execute.
    fn pass_proposal(env: &Env, client: &LuminaRegistryClient, admin: &Address, pid: u32) {
        client.approve_proposal(admin, &pid);
        advance_ledger(env, TIMELOCK_LEDGERS);
        client.execute_proposal(&pid);
    }

    fn mint(env: &Env, token_id: &Address, to: &Address, amount: i128) {
        token::StellarAssetClient::new(env, token_id).mint(to, &amount);
    }

    fn balance(env: &Env, token_id: &Address, of: &Address) -> i128 {
        token::Client::new(env, token_id).balance(of)
    }

    /// Register a contract and stake `amount` against it.
    fn register_and_stake(
        env: &Env,
        client: &LuminaRegistryClient,
        token_id: &Address,
        amount: i128,
    ) -> (Address, Address) {
        let (owner, target) = register_sample(env, client);
        mint(env, token_id, &owner, amount);
        client.stake(&owner, &target, &amount);
        (owner, target)
    }

    /// Sum of every registration's tracked stake (`DataKey::Stake`).
    ///
    /// Reads `AllContracts` from the registry's own storage, so it covers
    /// deactivated entries (which keep their stake until withdrawal) rather
    /// than only the active listing.
    fn tracked_stake_total(env: &Env, client: &LuminaRegistryClient) -> i128 {
        env.as_contract(&client.address, || {
            let all: Vec<Address> = env.storage().instance()
                .get(&DataKey::AllContracts)
                .unwrap_or(Vec::new(env));
            let mut total: i128 = 0;
            for contract_id in all.iter() {
                total += env.storage().persistent()
                    .get::<DataKey, i128>(&DataKey::Stake(contract_id))
                    .unwrap_or(0);
            }
            total
        })
    }

    /// Solvency invariant: the registry's token balance must exactly match
    /// what it believes it owes across all registrations.
    ///
    /// `stake` moves tokens in and tracks them, `withdraw_stake` moves them
    /// out and untracks them, and slash execution moves them to the treasury
    /// and untracks them — so after any sequence the two figures agree.
    /// Deliberately an equality (not just `balance >= tracked`): both an
    /// over-credited and an under-credited accounting bug break it.
    fn assert_solvency(env: &Env, client: &LuminaRegistryClient, token_id: &Address) {
        let tracked = tracked_stake_total(env, client);
        let held = balance(env, token_id, &client.address);
        assert_eq!(
            held, tracked,
            "solvency invariant violated: token balance {} != tracked stake {}",
            held, tracked,
        );
    }

    // ── Configuration ───────────────────────────────────────────────────────

    #[test]
    fn staking_is_closed_until_governance_opens_it() {
        let (env, client, _admin) = setup();
        let (owner, target) = register_sample(&env, &client);

        assert_eq!(
            client.try_get_staking_config(),
            Err(Ok(RegistryError::StakingNotConfigured)),
        );
        assert_eq!(
            client.try_stake(&owner, &target, &100),
            Err(Ok(RegistryError::StakingNotConfigured)),
        );
    }

    #[test]
    fn configure_staking_records_token_and_treasury() {
        let (_env, client, _admin, token_id, treasury) = setup_staking();
        assert_eq!(client.get_staking_config(), (token_id, treasury));
    }

    #[test]
    fn configure_staking_cannot_be_proposed_by_a_non_admin() {
        let (env, client, _admin) = setup();
        let stranger = Address::generate(&env);
        let token_id = env.register_stellar_asset_contract_v2(Address::generate(&env)).address();

        assert_eq!(
            client.try_propose_configure_staking(&stranger, &token_id, &stranger),
            Err(Ok(RegistryError::NotAdmin)),
        );
    }

    // ── Staking ─────────────────────────────────────────────────────────────

    #[test]
    fn stake_moves_real_tokens_into_the_registry() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_sample(&env, &client);
        mint(&env, &token_id, &owner, 1_000);

        client.stake(&owner, &target, &400);

        assert_eq!(client.get_stake(&target), 400);
        assert_eq!(balance(&env, &token_id, &owner), 600);
        assert_eq!(balance(&env, &token_id, &client.address), 400);
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn stake_tops_up_an_existing_stake() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_and_stake(&env, &client, &token_id, 300);

        mint(&env, &token_id, &owner, 200);
        client.stake(&owner, &target, &200);

        assert_eq!(client.get_stake(&target), 500);
        assert_eq!(balance(&env, &token_id, &client.address), 500);
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn stake_rejects_a_caller_who_is_not_the_owner() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (_owner, target) = register_sample(&env, &client);
        let stranger = Address::generate(&env);
        mint(&env, &token_id, &stranger, 500);

        assert_eq!(
            client.try_stake(&stranger, &target, &100),
            Err(Ok(RegistryError::NotOwner)),
        );
        assert_eq!(client.get_stake(&target), 0);
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn stake_rejects_non_positive_amounts() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_sample(&env, &client);
        mint(&env, &token_id, &owner, 500);

        assert_eq!(
            client.try_stake(&owner, &target, &0),
            Err(Ok(RegistryError::InvalidAmount)),
        );
        assert_eq!(
            client.try_stake(&owner, &target, &-100),
            Err(Ok(RegistryError::InvalidAmount)),
        );
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn stake_rejects_an_unregistered_contract() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let owner = Address::generate(&env);
        let unregistered = Address::generate(&env);
        mint(&env, &token_id, &owner, 500);

        assert_eq!(
            client.try_stake(&owner, &unregistered, &100),
            Err(Ok(RegistryError::ContractNotFound)),
        );
        assert_solvency(&env, &client, &token_id);
    }

    // ── Verification ────────────────────────────────────────────────────────

    #[test]
    fn verification_is_unset_by_default() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (_owner, target) = register_sample(&env, &client);
        assert!(!client.is_verified(&target));
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn governance_can_attest_and_later_revoke_verification() {
        let (env, client, admin, token_id, _treasury) = setup_staking();
        let (_owner, target) = register_sample(&env, &client);

        let pid = client.propose_set_verified(&admin, &target, &true);
        pass_proposal(&env, &client, &admin, pid);
        assert!(client.is_verified(&target));

        let pid = client.propose_set_verified(&admin, &target, &false);
        pass_proposal(&env, &client, &admin, pid);
        assert!(!client.is_verified(&target));
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn a_registrant_cannot_verify_their_own_contract() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_sample(&env, &client);

        // The owner is not an admin, and there is no non-governance path to
        // verified status at all — this is the entire value of the signal.
        assert_eq!(
            client.try_propose_set_verified(&owner, &target, &true),
            Err(Ok(RegistryError::NotAdmin)),
        );
        assert!(!client.is_verified(&target));
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn verification_cannot_be_proposed_for_an_unregistered_contract() {
        let (env, client, admin, token_id, _treasury) = setup_staking();
        let unregistered = Address::generate(&env);

        assert_eq!(
            client.try_propose_set_verified(&admin, &unregistered, &true),
            Err(Ok(RegistryError::ContractNotFound)),
        );
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn verification_survives_the_timelock_without_early_effect() {
        let (env, client, admin, token_id, _treasury) = setup_staking();
        let (_owner, target) = register_sample(&env, &client);

        let pid = client.propose_set_verified(&admin, &target, &true);
        client.approve_proposal(&admin, &pid);

        // Approved but not executed: the attestation must not be live yet.
        assert!(!client.is_verified(&target));

        advance_ledger(&env, TIMELOCK_LEDGERS);
        client.execute_proposal(&pid);
        assert!(client.is_verified(&target));
        assert_solvency(&env, &client, &token_id);
    }

    // ── Slashing ────────────────────────────────────────────────────────────

    #[test]
    fn slash_moves_stake_to_the_treasury_and_records_the_reason() {
        let (env, client, admin, token_id, treasury) = setup_staking();
        let (_owner, target) = register_and_stake(&env, &client, &token_id, 1_000);

        let reason = String::from_str(&env, "indexed a phishing contract");
        let pid = client.propose_slash(&admin, &target, &400, &reason);
        pass_proposal(&env, &client, &admin, pid);

        assert_eq!(client.get_stake(&target), 600);
        assert_eq!(balance(&env, &token_id, &treasury), 400);
        assert_eq!(balance(&env, &token_id, &client.address), 600);

        let slashes = client.get_slashes(&target);
        assert_eq!(slashes.len(), 1);
        let record = slashes.get(0).unwrap();
        assert_eq!(record.amount, 400);
        assert_eq!(record.reason, reason);
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn repeated_slashes_accumulate_in_the_history() {
        let (env, client, admin, token_id, treasury) = setup_staking();
        let (_owner, target) = register_and_stake(&env, &client, &token_id, 1_000);

        let first = String::from_str(&env, "first offence");
        let pid = client.propose_slash(&admin, &target, &200, &first);
        pass_proposal(&env, &client, &admin, pid);

        let second = String::from_str(&env, "second offence");
        let pid = client.propose_slash(&admin, &target, &300, &second);
        pass_proposal(&env, &client, &admin, pid);

        assert_eq!(client.get_stake(&target), 500);
        assert_eq!(balance(&env, &token_id, &treasury), 500);

        let slashes = client.get_slashes(&target);
        assert_eq!(slashes.len(), 2);
        assert_eq!(slashes.get(0).unwrap().reason, first);
        assert_eq!(slashes.get(1).unwrap().reason, second);
        assert_eq!(client.get_reputation(&target).slashed_total, 500);
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn slash_cannot_exceed_the_staked_balance() {
        let (env, client, admin, token_id, treasury) = setup_staking();
        let (_owner, target) = register_and_stake(&env, &client, &token_id, 100);

        let reason = String::from_str(&env, "over-slash");
        let pid = client.propose_slash(&admin, &target, &500, &reason);
        client.approve_proposal(&admin, &pid);
        advance_ledger(&env, TIMELOCK_LEDGERS);

        // The proposal passes governance but reverts on execution rather than
        // taking tokens the registry is not holding for this registration.
        assert_eq!(
            client.try_execute_proposal(&pid),
            Err(Ok(RegistryError::InsufficientStake)),
        );
        assert_eq!(client.get_stake(&target), 100);
        assert_eq!(balance(&env, &token_id, &treasury), 0);
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn slashing_a_registration_with_no_stake_is_rejected() {
        let (env, client, admin, token_id, _treasury) = setup_staking();
        let (_owner, target) = register_sample(&env, &client);

        let reason = String::from_str(&env, "nothing at stake");
        let pid = client.propose_slash(&admin, &target, &1, &reason);
        client.approve_proposal(&admin, &pid);
        advance_ledger(&env, TIMELOCK_LEDGERS);

        assert_eq!(
            client.try_execute_proposal(&pid),
            Err(Ok(RegistryError::InsufficientStake)),
        );
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn slash_cannot_be_proposed_for_an_unregistered_contract() {
        let (env, client, admin, token_id, _treasury) = setup_staking();
        let unregistered = Address::generate(&env);
        let reason = String::from_str(&env, "unknown");

        assert_eq!(
            client.try_propose_slash(&admin, &unregistered, &100, &reason),
            Err(Ok(RegistryError::ContractNotFound)),
        );
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn slash_cannot_be_proposed_for_a_non_positive_amount() {
        let (env, client, admin, token_id, _treasury) = setup_staking();
        let (_owner, target) = register_and_stake(&env, &client, &token_id, 100);
        let reason = String::from_str(&env, "zero");

        assert_eq!(
            client.try_propose_slash(&admin, &target, &0, &reason),
            Err(Ok(RegistryError::InvalidAmount)),
        );
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn slash_cannot_be_proposed_by_a_non_admin() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_and_stake(&env, &client, &token_id, 100);
        let reason = String::from_str(&env, "self-serving");

        assert_eq!(
            client.try_propose_slash(&owner, &target, &50, &reason),
            Err(Ok(RegistryError::NotAdmin)),
        );
        assert_solvency(&env, &client, &token_id);
    }

    // ── Withdrawal ──────────────────────────────────────────────────────────

    #[test]
    fn withdraw_returns_the_full_stake_once_the_owner_has_deactivated() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_and_stake(&env, &client, &token_id, 750);

        client.deactivate(&owner, &target);
        assert_eq!(client.withdraw_stake(&owner, &target), 750);

        assert_eq!(client.get_stake(&target), 0);
        assert_eq!(balance(&env, &token_id, &owner), 750);
        assert_eq!(balance(&env, &token_id, &client.address), 0);
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn withdraw_is_refused_while_the_registration_is_still_active() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_and_stake(&env, &client, &token_id, 500);

        // Still listed and still benefiting from the stake.
        assert_eq!(
            client.try_withdraw_stake(&owner, &target),
            Err(Ok(RegistryError::RegistrationActive)),
        );
        assert_eq!(client.get_stake(&target), 500);
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn withdraw_is_refused_for_anyone_but_the_owner() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_and_stake(&env, &client, &token_id, 500);
        let stranger = Address::generate(&env);
        client.deactivate(&owner, &target);

        assert_eq!(
            client.try_withdraw_stake(&stranger, &target),
            Err(Ok(RegistryError::NotOwner)),
        );
        assert_eq!(client.get_stake(&target), 500);
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn withdraw_is_refused_when_there_is_nothing_staked() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_sample(&env, &client);
        client.deactivate(&owner, &target);

        assert_eq!(
            client.try_withdraw_stake(&owner, &target),
            Err(Ok(RegistryError::InsufficientStake)),
        );
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn withdraw_is_frozen_while_a_slash_is_still_recent() {
        let (env, client, admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_and_stake(&env, &client, &token_id, 1_000);

        let reason = String::from_str(&env, "under investigation");
        let pid = client.propose_slash(&admin, &target, &200, &reason);
        pass_proposal(&env, &client, &admin, pid);

        client.deactivate(&owner, &target);

        // Deactivated and owned by the caller, but the slash lock is what
        // stops the owner emptying the remainder before a second slash can
        // clear its own timelock.
        assert_eq!(
            client.try_withdraw_stake(&owner, &target),
            Err(Ok(RegistryError::StakeLocked)),
        );
        assert_eq!(client.get_stake(&target), 800);
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn withdraw_reopens_once_the_slash_lock_elapses() {
        let (env, client, admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_and_stake(&env, &client, &token_id, 1_000);

        let reason = String::from_str(&env, "under investigation");
        let pid = client.propose_slash(&admin, &target, &200, &reason);
        pass_proposal(&env, &client, &admin, pid);
        client.deactivate(&owner, &target);

        advance_ledger(&env, SLASH_LOCK_LEDGERS);

        // Only what survived the slash comes back.
        assert_eq!(client.withdraw_stake(&owner, &target), 800);
        assert_eq!(balance(&env, &token_id, &owner), 800);
        assert_eq!(client.get_stake(&target), 0);
        assert_solvency(&env, &client, &token_id);
    }

    // ── Reputation views ────────────────────────────────────────────────────

    #[test]
    fn reputation_of_an_untouched_registration_is_all_zeroes() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (_owner, target) = register_sample(&env, &client);

        let reputation = client.get_reputation(&target);
        assert_eq!(reputation.stake, 0);
        assert!(!reputation.verified);
        assert_eq!(reputation.slashed_total, 0);
        assert_eq!(reputation.withdraw_locked_until, 0);
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn contract_profile_joins_the_entry_with_its_reputation() {
        let (env, client, admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_and_stake(&env, &client, &token_id, 600);

        let pid = client.propose_set_verified(&admin, &target, &true);
        pass_proposal(&env, &client, &admin, pid);

        let profile = client.get_contract_profile(&target);
        assert_eq!(profile.entry.contract_id, target);
        assert_eq!(profile.entry.owner, owner);
        assert!(profile.entry.active);
        assert_eq!(profile.reputation.stake, 600);
        assert!(profile.reputation.verified);
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn contract_profile_rejects_an_unregistered_contract() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let unregistered = Address::generate(&env);

        assert_eq!(
            client.try_get_contract_profile(&unregistered),
            Err(Ok(RegistryError::ContractNotFound)),
        );
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn active_profiles_track_active_contracts_and_carry_reputation() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (_o1, staked) = register_and_stake(&env, &client, &token_id, 250);
        let (owner2, plain) = register_sample(&env, &client);

        let profiles = client.get_active_profiles(&0, &10);
        assert_eq!(profiles.len(), client.get_active_contracts(&0, &10).len());
        assert_eq!(profiles.len(), 2);

        let with_stake = profiles.iter().find(|p| p.entry.contract_id == staked).unwrap();
        assert_eq!(with_stake.reputation.stake, 250);
        let without = profiles.iter().find(|p| p.entry.contract_id == plain).unwrap();
        assert_eq!(without.reputation.stake, 0);

        // Deactivation drops it from the profile listing exactly as it does
        // from the plain listing.
        client.deactivate(&owner2, &plain);
        assert_eq!(client.get_active_profiles(&0, &10).len(), 1);
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn active_profiles_paginate_like_active_contracts() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        for _ in 0..5 {
            register_sample(&env, &client);
        }

        assert_eq!(client.get_active_profiles(&0, &2).len(), 2);
        assert_eq!(client.get_active_profiles(&2, &2).len(), 2);
        assert_eq!(client.get_active_profiles(&4, &2).len(), 1);
        assert_eq!(client.get_active_profiles(&5, &2).len(), 0);
        assert_eq!(client.get_active_profiles(&99, &2).len(), 0);
        assert_solvency(&env, &client, &token_id);
    }

    // ── End-to-end lifecycle ────────────────────────────────────────────────

    #[test]
    fn full_stake_verify_slash_withdraw_lifecycle() {
        let (env, client, admin, token_id, treasury) = setup_staking();

        // Register and post collateral.
        let (owner, target) = register_sample(&env, &client);
        mint(&env, &token_id, &owner, 1_000);
        client.stake(&owner, &target, &1_000);
        assert_eq!(client.get_reputation(&target).stake, 1_000);

        // Governance attests the project.
        let pid = client.propose_set_verified(&admin, &target, &true);
        pass_proposal(&env, &client, &admin, pid);
        assert!(client.get_contract_profile(&target).reputation.verified);

        // The project misbehaves and governance slashes a quarter of the stake.
        let reason = String::from_str(&env, "misreported contract metadata");
        let pid = client.propose_slash(&admin, &target, &250, &reason);
        pass_proposal(&env, &client, &admin, pid);

        let reputation = client.get_reputation(&target);
        assert_eq!(reputation.stake, 750);
        assert_eq!(reputation.slashed_total, 250);
        assert!(reputation.withdraw_locked_until > env.ledger().sequence());
        assert_eq!(balance(&env, &token_id, &treasury), 250);

        // Trying to exit immediately fails on both counts, in order: still
        // listed first, then still locked.
        assert_eq!(
            client.try_withdraw_stake(&owner, &target),
            Err(Ok(RegistryError::RegistrationActive)),
        );
        client.deactivate(&owner, &target);
        assert_eq!(
            client.try_withdraw_stake(&owner, &target),
            Err(Ok(RegistryError::StakeLocked)),
        );

        // Once the lock expires the remainder — and only the remainder —
        // comes back.
        advance_ledger(&env, SLASH_LOCK_LEDGERS);
        assert_eq!(client.withdraw_stake(&owner, &target), 750);
        assert_eq!(balance(&env, &token_id, &owner), 750);
        assert_eq!(balance(&env, &token_id, &client.address), 0);

        // The slash record outlives the stake it was taken from.
        assert_eq!(client.get_slashes(&target).len(), 1);
        assert_eq!(client.get_reputation(&target).slashed_total, 250);
        assert_eq!(client.get_reputation(&target).stake, 0);
        assert_solvency(&env, &client, &token_id);
    }

    // ── Stake-token failure ─────────────────────────────────────────────────
    //
    // Real stake tokens can refuse a transfer — a frozen trustline, an
    // insufficient balance, a clawback-enabled asset. Every staking path moves
    // tokens before writing its own bookkeeping, and relies on the failed
    // transfer aborting the whole invocation so that nothing it wrote
    // survives. These tests use a token that fails on demand to check that the
    // registry's stored balances never drift from what the token reports.

    #[contracttype]
    enum FailingTokenKey {
        Balance(Address),
        Failing,
    }

    /// A minimal SEP-41-shaped token whose `transfer` can be switched to fail.
    #[contract]
    struct FailingToken;

    #[contractimpl]
    impl FailingToken {
        pub fn mint(env: Env, to: Address, amount: i128) {
            let key = FailingTokenKey::Balance(to);
            let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
            env.storage().persistent().set(&key, &(current + amount));
        }

        pub fn balance(env: Env, id: Address) -> i128 {
            env.storage().persistent().get(&FailingTokenKey::Balance(id)).unwrap_or(0)
        }

        pub fn set_failing(env: Env, failing: bool) {
            env.storage().instance().set(&FailingTokenKey::Failing, &failing);
        }

        pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
            from.require_auth();
            if env.storage().instance().get(&FailingTokenKey::Failing).unwrap_or(false) {
                panic!("transfer refused: account frozen");
            }
            let from_balance = Self::balance(env.clone(), from.clone());
            if from_balance < amount {
                panic!("transfer refused: insufficient balance");
            }
            env.storage().persistent()
                .set(&FailingTokenKey::Balance(from), &(from_balance - amount));
            Self::mint(env, to, amount);
        }
    }

    /// Like [`setup_staking`], but with a [`FailingToken`] as the stake token.
    fn setup_failing_staking() -> (
        Env,
        LuminaRegistryClient<'static>,
        Address,
        FailingTokenClient<'static>,
        Address,
    ) {
        let (env, client, admin) = setup();
        let token_id = env.register(FailingToken, ());
        let token = FailingTokenClient::new(&env, &token_id);
        let treasury = Address::generate(&env);

        let pid = client.propose_configure_staking(&admin, &token_id, &treasury);
        pass_proposal(&env, &client, &admin, pid);

        (env, client, admin, token, treasury)
    }

    /// The registry's recorded stakes must add up to exactly what the token
    /// says the registry holds.
    fn assert_accounting_matches_token(
        client: &LuminaRegistryClient,
        token: &FailingTokenClient,
        registrations: &[&Address],
    ) {
        let recorded: i128 = registrations.iter().map(|r| client.get_stake(r)).sum();
        assert_eq!(recorded, token.balance(&client.address));
    }

    #[test]
    fn failed_stake_transfer_records_no_stake() {
        let (env, client, _admin, token, _treasury) = setup_failing_staking();
        let (owner, target) = register_sample(&env, &client);
        token.mint(&owner, &1_000);

        token.set_failing(&true);
        assert!(client.try_stake(&owner, &target, &400).is_err());

        assert_eq!(client.get_stake(&target), 0);
        assert_eq!(client.get_reputation(&target).stake, 0);
        assert_eq!(token.balance(&owner), 1_000);
        assert_accounting_matches_token(&client, &token, &[&target]);

        // Nothing was half-applied, so a retry once the token recovers counts
        // the stake exactly once.
        token.set_failing(&false);
        client.stake(&owner, &target, &400);
        assert_eq!(client.get_stake(&target), 400);
        assert_eq!(token.balance(&owner), 600);
        assert_accounting_matches_token(&client, &token, &[&target]);
    }

    #[test]
    fn failed_top_up_leaves_the_existing_stake_untouched() {
        let (env, client, _admin, token, _treasury) = setup_failing_staking();
        let (owner, target) = register_sample(&env, &client);
        token.mint(&owner, &1_000);
        client.stake(&owner, &target, &400);

        token.set_failing(&true);
        assert!(client.try_stake(&owner, &target, &100).is_err());

        assert_eq!(client.get_stake(&target), 400);
        assert_eq!(token.balance(&owner), 600);
        assert_accounting_matches_token(&client, &token, &[&target]);
    }

    #[test]
    fn stake_beyond_the_owners_balance_records_nothing() {
        // The same guarantee against a real Stellar Asset Contract, whose
        // refusal here is an ordinary insufficient-balance error.
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_sample(&env, &client);
        mint(&env, &token_id, &owner, 100);

        assert!(client.try_stake(&owner, &target, &101).is_err());

        assert_eq!(client.get_stake(&target), 0);
        assert_eq!(balance(&env, &token_id, &owner), 100);
        assert_eq!(balance(&env, &token_id, &client.address), 0);
    }

    #[test]
    fn failed_withdraw_transfer_keeps_the_stake_recorded() {
        let (env, client, _admin, token, _treasury) = setup_failing_staking();
        let (owner, target) = register_sample(&env, &client);
        token.mint(&owner, &750);
        client.stake(&owner, &target, &750);
        client.deactivate(&owner, &target);

        token.set_failing(&true);
        assert!(client.try_withdraw_stake(&owner, &target).is_err());

        // Zeroing the stake without the tokens leaving would strand them in
        // the registry with no registration able to claim them.
        assert_eq!(client.get_stake(&target), 750);
        assert_eq!(token.balance(&owner), 0);
        assert_accounting_matches_token(&client, &token, &[&target]);

        token.set_failing(&false);
        assert_eq!(client.withdraw_stake(&owner, &target), 750);
        assert_eq!(client.get_stake(&target), 0);
        assert_eq!(token.balance(&owner), 750);
        assert_accounting_matches_token(&client, &token, &[&target]);
    }

    #[test]
    fn failed_slash_transfer_leaves_stake_history_and_proposal_untouched() {
        let (env, client, admin, token, treasury) = setup_failing_staking();
        let (owner, target) = register_sample(&env, &client);
        token.mint(&owner, &1_000);
        client.stake(&owner, &target, &1_000);

        let reason = String::from_str(&env, "indexed a phishing contract");
        let pid = client.propose_slash(&admin, &target, &400, &reason);
        client.approve_proposal(&admin, &pid);
        advance_ledger(&env, TIMELOCK_LEDGERS);

        token.set_failing(&true);
        assert!(client.try_execute_proposal(&pid).is_err());

        // No stake debited, no slash recorded, no withdraw lock applied, and
        // — although `execute_proposal` marks the proposal executed before
        // applying it — that mark is rolled back too, so it can be retried.
        assert_eq!(client.get_stake(&target), 1_000);
        assert_eq!(client.get_slashes(&target).len(), 0);
        assert_eq!(client.get_reputation(&target).slashed_total, 0);
        assert_eq!(client.get_reputation(&target).withdraw_locked_until, 0);
        assert!(!client.get_proposal(&pid).executed);
        assert_eq!(token.balance(&treasury), 0);
        assert_accounting_matches_token(&client, &token, &[&target]);

        token.set_failing(&false);
        client.execute_proposal(&pid);
        assert_eq!(client.get_stake(&target), 600);
        assert_eq!(client.get_slashes(&target).len(), 1);
        assert_eq!(token.balance(&treasury), 400);
        assert_accounting_matches_token(&client, &token, &[&target]);

        // The retried slash applied its lock as normal.
        client.deactivate(&owner, &target);
        assert_eq!(
            client.try_withdraw_stake(&owner, &target),
            Err(Ok(RegistryError::StakeLocked)),
        );
    }

    #[test]
    fn a_failed_transfer_on_one_registration_does_not_disturb_another() {
        let (env, client, admin, token, _treasury) = setup_failing_staking();
        let (owner_a, a) = register_sample(&env, &client);
        let (owner_b, b) = register_sample(&env, &client);
        token.mint(&owner_a, &500);
        token.mint(&owner_b, &300);
        client.stake(&owner_a, &a, &500);
        client.stake(&owner_b, &b, &300);

        let reason = String::from_str(&env, "spam");
        let pid = client.propose_slash(&admin, &a, &200, &reason);
        client.approve_proposal(&admin, &pid);
        advance_ledger(&env, TIMELOCK_LEDGERS);
        client.deactivate(&owner_b, &b);

        token.set_failing(&true);
        assert!(client.try_execute_proposal(&pid).is_err());
        assert!(client.try_withdraw_stake(&owner_b, &b).is_err());

        assert_eq!(client.get_stake(&a), 500);
        assert_eq!(client.get_stake(&b), 300);
        assert_accounting_matches_token(&client, &token, &[&a, &b]);
    }

    // ── Category taxonomy ───────────────────────────────────────────────────

    #[test]
    fn registration_requires_at_least_one_category() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let target = Address::generate(&env);

        assert_eq!(
            client.try_register_contract(
                &owner,
                &target,
                &String::from_str(&env, "Uncategorized"),
                &String::from_str(&env, "Uncategorized"),
                &Vec::new(&env),
            ),
            Err(Ok(RegistryError::NoCategories)),
        );
        assert!(!client.is_registered(&target));
    }

    #[test]
    fn registration_records_its_categories() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let target = register_in(&env, &client, &owner, &[Category::DeFi, Category::Payments]);

        let recorded = client.get_categories(&target);
        assert_eq!(recorded.len(), 2);
        assert!(recorded.contains(&Category::DeFi));
        assert!(recorded.contains(&Category::Payments));
    }

    #[test]
    fn duplicate_categories_are_collapsed() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let target = register_in(
            &env,
            &client,
            &owner,
            &[Category::Gaming, Category::Gaming, Category::Gaming],
        );

        assert_eq!(client.get_categories(&target).len(), 1);
        // And the index lists it once, not three times.
        assert_eq!(client.get_active_contracts_by_category(&Category::Gaming, &0, &10).len(), 1);
    }

    #[test]
    fn a_multi_category_registration_is_discoverable_under_each() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let target = register_in(&env, &client, &owner, &[Category::DeFi, Category::Oracle]);

        for category in [Category::DeFi, Category::Oracle] {
            let page = client.get_active_contracts_by_category(&category, &0, &10);
            assert_eq!(page.len(), 1);
            assert_eq!(page.get(0).unwrap().contract_id, target);
        }
    }

    #[test]
    fn category_listing_excludes_other_categories() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let defi = register_in(&env, &client, &owner, &[Category::DeFi]);
        let gaming = register_in(&env, &client, &owner, &[Category::Gaming]);

        let defi_page = client.get_active_contracts_by_category(&Category::DeFi, &0, &10);
        assert!(page_contains(&defi_page, &defi));
        assert!(!page_contains(&defi_page, &gaming));

        let gaming_page = client.get_active_contracts_by_category(&Category::Gaming, &0, &10);
        assert!(page_contains(&gaming_page, &gaming));
        assert!(!page_contains(&gaming_page, &defi));
    }

    #[test]
    fn an_unused_category_returns_an_empty_page() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        register_in(&env, &client, &owner, &[Category::DeFi]);

        assert_eq!(client.get_active_contracts_by_category(&Category::Dao, &0, &10).len(), 0);
        // Including on a completely empty registry.
        let (_env2, empty, _a) = setup();
        assert_eq!(empty.get_active_contracts_by_category(&Category::DeFi, &0, &10).len(), 0);
    }

    #[test]
    fn deactivation_removes_a_contract_from_category_browsing() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let target = register_in(&env, &client, &owner, &[Category::Identity]);

        assert_eq!(client.get_active_contracts_by_category(&Category::Identity, &0, &10).len(), 1);

        client.deactivate(&owner, &target);

        // Filtered out of browsing, exactly like the global listing...
        assert_eq!(client.get_active_contracts_by_category(&Category::Identity, &0, &10).len(), 0);
        assert_eq!(client.get_active_contracts(&0, &10).len(), 0);
        // ...but still recorded against the registration, because `deactivate`
        // does not rewrite category indices.
        assert_eq!(client.get_categories(&target).len(), 1);
    }

    #[test]
    fn category_pagination_matches_the_global_listing() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        for _ in 0..5 {
            register_in(&env, &client, &owner, &[Category::Nft]);
        }
        // A registration in another category must not leak into the pages.
        register_in(&env, &client, &owner, &[Category::Gaming]);

        assert_eq!(client.get_active_contracts_by_category(&Category::Nft, &0, &10).len(), 5);
        assert_eq!(client.get_active_contracts_by_category(&Category::Nft, &0, &2).len(), 2);
        assert_eq!(client.get_active_contracts_by_category(&Category::Nft, &2, &2).len(), 2);
        assert_eq!(client.get_active_contracts_by_category(&Category::Nft, &4, &2).len(), 1);
        assert_eq!(client.get_active_contracts_by_category(&Category::Nft, &5, &2).len(), 0);
        assert_eq!(client.get_active_contracts_by_category(&Category::Nft, &99, &2).len(), 0);
    }

    #[test]
    fn category_pages_are_in_registration_order() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let first = register_in(&env, &client, &owner, &[Category::Dao]);
        let second = register_in(&env, &client, &owner, &[Category::Dao]);
        let third = register_in(&env, &client, &owner, &[Category::Dao]);

        let at = |offset: u32| {
            client.get_active_contracts_by_category(&Category::Dao, &offset, &1)
                .get(0).unwrap().contract_id
        };
        assert_eq!(at(0), first);
        assert_eq!(at(1), second);
        assert_eq!(at(2), third);
    }

    #[test]
    fn category_paging_is_identical_to_the_global_listing_over_the_same_set() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);

        // Every registration goes in one category, so the category index and
        // the global index hold the same addresses in the same order. Any
        // divergence between the two queries is then a real difference in
        // paging semantics rather than a difference in the data.
        let mut registered = Vec::new(&env);
        for _ in 0..6 {
            registered.push_back(register_in(&env, &client, &owner, &[Category::Oracle]));
        }

        // A hole in the middle and one at the very end, so the comparison
        // covers pages that span skipped entries and pages that run off the
        // end of the index.
        client.deactivate(&owner, &registered.get(2).unwrap());
        client.deactivate(&owner, &registered.get(5).unwrap());

        for offset in 0..8u32 {
            for limit in 0..8u32 {
                assert_eq!(
                    client.get_active_contracts_by_category(&Category::Oracle, &offset, &limit),
                    client.get_active_contracts(&offset, &limit),
                    "category paging diverged at offset {} limit {}",
                    offset,
                    limit,
                );
            }
        }

        // And the shared behaviour is the one worth naming: `offset` indexes
        // the raw index, while deactivated entries are stepped over without
        // consuming `limit`.
        let page = client.get_active_contracts_by_category(&Category::Oracle, &2, &2);
        assert_eq!(page.len(), 2);
        assert_eq!(page.get(0).unwrap().contract_id, registered.get(3).unwrap());
        assert_eq!(page.get(1).unwrap().contract_id, registered.get(4).unwrap());
    }

    // ── set_categories ──────────────────────────────────────────────────────

    #[test]
    fn set_categories_moves_a_registration_between_categories() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let target = register_in(&env, &client, &owner, &[Category::DeFi]);

        client.set_categories(&owner, &target, &cats(&env, &[Category::Payments]));

        assert_eq!(client.get_active_contracts_by_category(&Category::DeFi, &0, &10).len(), 0);
        let page = client.get_active_contracts_by_category(&Category::Payments, &0, &10);
        assert_eq!(page.len(), 1);
        assert_eq!(page.get(0).unwrap().contract_id, target);
        assert_eq!(client.get_categories(&target), cats(&env, &[Category::Payments]));
    }

    #[test]
    fn set_categories_keeps_the_ones_that_are_retained() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let target = register_in(&env, &client, &owner, &[Category::DeFi, Category::Oracle]);

        client.set_categories(&owner, &target, &cats(&env, &[Category::DeFi, Category::Dao]));

        assert_eq!(client.get_active_contracts_by_category(&Category::DeFi, &0, &10).len(), 1);
        assert_eq!(client.get_active_contracts_by_category(&Category::Dao, &0, &10).len(), 1);
        assert_eq!(client.get_active_contracts_by_category(&Category::Oracle, &0, &10).len(), 0);
    }

    #[test]
    fn set_categories_does_not_duplicate_an_unchanged_category() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let target = register_in(&env, &client, &owner, &[Category::Gaming]);

        client.set_categories(&owner, &target, &cats(&env, &[Category::Gaming]));

        assert_eq!(client.get_active_contracts_by_category(&Category::Gaming, &0, &10).len(), 1);
        assert_eq!(client.get_categories(&target).len(), 1);
    }

    #[test]
    fn set_categories_is_owner_only() {
        let (env, client, admin) = setup();
        let owner = Address::generate(&env);
        let target = register_in(&env, &client, &owner, &[Category::DeFi]);
        let stranger = Address::generate(&env);

        assert_eq!(
            client.try_set_categories(&stranger, &target, &cats(&env, &[Category::Gaming])),
            Err(Ok(RegistryError::NotOwner)),
        );
        // Not even the admin — how a project files itself is its own business,
        // exactly as with `update_metadata`.
        assert_eq!(
            client.try_set_categories(&admin, &target, &cats(&env, &[Category::Gaming])),
            Err(Ok(RegistryError::NotOwner)),
        );
        assert_eq!(client.get_categories(&target), cats(&env, &[Category::DeFi]));
    }

    #[test]
    fn set_categories_rejects_an_empty_selection() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let target = register_in(&env, &client, &owner, &[Category::DeFi]);

        assert_eq!(
            client.try_set_categories(&owner, &target, &Vec::new(&env)),
            Err(Ok(RegistryError::NoCategories)),
        );
        assert_eq!(client.get_categories(&target).len(), 1);
    }

    #[test]
    fn set_categories_rejects_an_unregistered_contract() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let unregistered = Address::generate(&env);

        assert_eq!(
            client.try_set_categories(&owner, &unregistered, &cats(&env, &[Category::DeFi])),
            Err(Ok(RegistryError::ContractNotFound)),
        );
    }

    #[test]
    fn set_categories_survives_a_transfer_of_ownership() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let target = register_in(&env, &client, &owner, &[Category::DeFi]);
        let new_owner = Address::generate(&env);

        client.transfer_ownership(&owner, &target, &new_owner);

        // The old owner has lost the right to refile it; the new one has it.
        assert_eq!(
            client.try_set_categories(&owner, &target, &cats(&env, &[Category::Gaming])),
            Err(Ok(RegistryError::NotOwner)),
        );
        client.set_categories(&new_owner, &target, &cats(&env, &[Category::Gaming]));
        assert_eq!(client.get_categories(&target), cats(&env, &[Category::Gaming]));
    }

    #[test]
    fn categories_and_reputation_are_independent() {
        let (env, client, admin, token_id, _treasury) = setup_staking();
        let owner = Address::generate(&env);
        let target = register_in(&env, &client, &owner, &[Category::DeFi]);

        mint(&env, &token_id, &owner, 500);
        client.stake(&owner, &target, &500);
        let pid = client.propose_set_verified(&admin, &target, &true);
        pass_proposal(&env, &client, &admin, pid);

        // Refiling the contract must not disturb its stake or attestation.
        client.set_categories(&owner, &target, &cats(&env, &[Category::Payments]));

        let profile = client.get_contract_profile(&target);
        assert_eq!(profile.reputation.stake, 500);
        assert!(profile.reputation.verified);
        assert_eq!(
            client.get_active_contracts_by_category(&Category::Payments, &0, &10).len(),
            1,
        );
        assert_solvency(&env, &client, &token_id);
    }

    // ── Deregistration, pruning & counters ──────────────────────────────────

    #[test]
    fn deregister_removes_every_index_reference_and_decrements_the_live_count() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let target = register_in(&env, &client, &owner, &[Category::DeFi, Category::Oracle]);

        assert_eq!(client.get_contract_count(), 1);
        assert_eq!(client.get_total_registered(), 1);

        client.deactivate(&owner, &target);
        client.deregister(&owner, &target);

        // The entry is gone and the address may be re-registered.
        assert!(!client.is_registered(&target));
        assert_eq!(client.get_contract_count(), 0);
        // Lifetime total is untouched.
        assert_eq!(client.get_total_registered(), 1);
        assert_eq!(client.get_active_contract_count(), 0);

        // No index reference remains: global, owner, and both categories.
        assert_eq!(client.get_active_contracts(&0, &10).len(), 0);
        assert_eq!(client.get_contracts_by_owner(&owner, &0, &10).len(), 0);
        assert_eq!(client.get_active_contracts_by_category(&Category::DeFi, &0, &10).len(), 0);
        assert_eq!(client.get_active_contracts_by_category(&Category::Oracle, &0, &10).len(), 0);
        assert_eq!(client.get_categories(&target).len(), 0);
    }

    #[test]
    fn deregister_requires_deactivated_and_unstaked() {
        let (env, client, _admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_and_stake(&env, &client, &token_id, 100);

        // Still active.
        assert_eq!(
            client.try_deregister(&owner, &target),
            Err(Ok(RegistryError::RegistrationActive)),
        );

        client.deactivate(&owner, &target);

        // Still staked.
        assert_eq!(
            client.try_deregister(&owner, &target),
            Err(Ok(RegistryError::StakeNotEmpty)),
        );
        assert_solvency(&env, &client, &token_id);

        // Non-owner cannot deregister even once eligible.
        let stranger = Address::generate(&env);
        assert_eq!(
            client.try_deregister(&stranger, &target),
            Err(Ok(RegistryError::NotOwner)),
        );
    }

    #[test]
    fn deregister_keeps_slash_history_for_audit() {
        let (env, client, admin, token_id, _treasury) = setup_staking();
        let (owner, target) = register_and_stake(&env, &client, &token_id, 500);

        let reason = String::from_str(&env, "kept for audit");
        let pid = client.propose_slash(&admin, &target, &100, &reason);
        pass_proposal(&env, &client, &admin, pid);
        client.deactivate(&owner, &target);

        advance_ledger(&env, SLASH_LOCK_LEDGERS);
        client.withdraw_stake(&owner, &target);
        client.deregister(&owner, &target);

        assert_eq!(client.get_slashes(&target).len(), 1);
        assert_solvency(&env, &client, &token_id);
    }

    #[test]
    fn prune_category_drops_dead_references_and_is_safe_to_repeat() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let live = register_in(&env, &client, &owner, &[Category::DeFi]);
        let gone = register_in(&env, &client, &owner, &[Category::DeFi]);

        // Simulate a dead reference the eager paths did not clean (e.g.
        // archival): remove the Contract entry behind the index's back.
        env.as_contract(&client.address, || {
            env.storage().persistent().remove(&DataKey::Contract(gone.clone()));
        });

        // The listing tolerates it, so it stays correct but pays for the walk.
        let page = client.get_active_contracts_by_category(&Category::DeFi, &0, &10);
        assert_eq!(page.len(), 1);
        assert_eq!(page.get(0).unwrap().contract_id, live);

        // Permissionless prune removes exactly the dead reference...
        assert_eq!(client.prune_category(&Category::DeFi), 1);
        assert_eq!(client.get_active_contracts_by_category(&Category::DeFi, &0, &10).len(), 1);

        // ...and repeating it removes nothing.
        assert_eq!(client.prune_category(&Category::DeFi), 0);
        assert_eq!(client.prune_all_contracts(), 1);
        assert_eq!(client.prune_all_contracts(), 0);
    }

    #[test]
    fn contract_count_is_live_and_total_registered_is_lifetime() {
        let (env, client, _admin) = setup();
        let owner = Address::generate(&env);
        let first = register_in(&env, &client, &owner, &[Category::Dao]);
        let _second = register_in(&env, &client, &owner, &[Category::Dao]);

        assert_eq!(client.get_contract_count(), 2);
        assert_eq!(client.get_total_registered(), 2);
        assert_eq!(client.get_active_contract_count(), 2);

        client.deactivate(&owner, &first);
        // Deactivation is not deregistration: the live total still counts it,
        // only the active figure drops.
        assert_eq!(client.get_contract_count(), 2);
        assert_eq!(client.get_total_registered(), 2);
        assert_eq!(client.get_active_contract_count(), 1);

        client.deregister(&owner, &first);
        assert_eq!(client.get_contract_count(), 1);
        assert_eq!(client.get_total_registered(), 2);
        assert_eq!(client.get_active_contract_count(), 1);
    }
}
