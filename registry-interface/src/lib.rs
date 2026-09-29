// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
#![no_std]
#![warn(missing_docs)]
//! Typed, read-only client for the Lumina Registry — for *contracts*, not
//! wallets.
//!
//! A Soroban contract that wants to ask "is this address listed, and is it
//! verified?" has two options today, and both are bad: hand-write
//! `env.invoke_contract(&registry, symbol_short!("is_registered"), ...)` and
//! decode the `Val` yourself, or use `contractimport!` on the registry's wasm.
//! The second pulls the whole registry binary into your build, and the first
//! is unchecked at compile time — a renamed export becomes a runtime failure
//! in someone else's contract.
//!
//! This crate is the third option: a declared trait covering the registry's
//! read-only surface, and the [`RegistryInterfaceClient`] that
//! [`soroban_sdk::contractclient`] generates from it.
//!
//! ```no_run
//! use lumina_registry_interface::RegistryInterfaceClient;
//! use soroban_sdk::{Address, Env};
//!
//! # fn check(env: &Env, registry: &Address, counterparty: &Address) {
//! let registry = RegistryInterfaceClient::new(env, registry);
//! if registry.is_registered(counterparty) && registry.is_verified(counterparty) {
//!     // ...
//! }
//! # }
//! ```
//!
//! ## Why the types are declared here instead of imported
//!
//! [`ContractEntry`], [`Category`], [`Reputation`] and friends are deliberately
//! *duplicated* from `lumina-registry` rather than re-exported from it. A
//! dependency edge on the contract crate would drag the registry's entire
//! `#[contractimpl]` — every exported entrypoint and its spec — into every
//! consumer's wasm, which is both a size problem and a link problem: two
//! `#[contractimpl]`s exporting the same symbol do not coexist. `registry-v2`
//! does the same thing for the same reason, and says so at length.
//!
//! The duplication is a real risk — the two declarations could drift — so it
//! is *tested* rather than trusted. `tests/interface_matches_registry.rs` reads
//! the registry's compiled spec out of its wasm and asserts that every
//! function, type and error code declared here matches what the contract
//! actually exports. Run against a changed registry, it fails with the
//! signature that moved.
//!
//! ## The cost of a read
//!
//! A cross-contract read is **not** free, and not free in the way people
//! expect. It is not a `simulateTransaction` — a contract calling the registry
//! on-chain spends the transaction's whole resource budget, and the callee's
//! instructions and ledger reads are charged to *you*.
//!
//! Concretely, each read is one nested invocation frame, which costs:
//!
//! - a fixed instruction charge for the call itself, before the callee runs
//!   any code;
//! - every ledger entry the callee touches, at the callee's TTL — the registry
//!   stores registrations in `persistent` entries, so a read is a persistent
//!   entry read, which is the expensive kind;
//! - a fresh 1 MiB memory allocation for the callee's frame, and the memory
//!   cost of decoding the arguments you passed in and the result you get back.
//!
//! The practical consequence: **the number of calls is what you pay for.** Two
//! `is_*` calls cost strictly more than one `get_contract_profile` that returns
//! both facts, and a loop over counterparties multiplies the fixed per-call
//! charge every iteration. The `examples/registry-consumer` crate measures this
//! on the real registry wasm rather than estimating it — see its `cost` module
//! and the "What a cross-contract read costs" section of the README.

use soroban_sdk::{contractclient, contracterror, contracttype, Address, Env, String, Vec};

/// The read-only half of the Lumina Registry.
///
/// Every method here corresponds one-to-one to an export the registry contract
/// actually has, with the same name and the same arguments; nothing here
/// mutates state and nothing here requires authorization. A consumer that only
/// ever needs to *read* the registry should depend on this trait rather than
/// on the contract crate.
///
/// Methods are listed in the same order as the registry's own view section.
/// Two of them carry paging semantics that are easy to get wrong, and they are
/// called out on the methods themselves:
///
/// - `get_active_contracts`, `get_active_profiles`, `get_active_contract_ids`,
///   `get_active_contracts_page` and `get_active_profiles_page` treat `offset`
///   as a position in the *raw* index, not in the filtered result, so a page
///   can come back shorter than `limit` while more active entries follow.
///   The `_page` variants additionally return `has_more` so a caller can tell
///   "end of list" from "this page was short".
/// - `get_contracts_by_owner` includes deactivated entries, because an owner
///   listing is a management view, not a discovery one.
#[contractclient(name = "RegistryInterfaceClient")]
pub trait RegistryInterface {
    /// Which build of the registry is live at this address.
    fn get_version(env: Env) -> u32;

    /// The first admin address. Errors with `NotInitialized` before the
    /// registry has been set up.
    fn get_admin(env: Env) -> Result<Address, RegistryError>;

    /// The full current admin set. Errors with `NotInitialized` if empty.
    fn get_admins(env: Env) -> Result<Vec<Address>, RegistryError>;

    /// The number of approvals a proposal needs. Errors with `NotInitialized`
    /// before the registry has been set up.
    fn get_threshold(env: Env) -> Result<u32, RegistryError>;

    /// Retrieve a governance proposal by ID.
    fn get_proposal(env: Env, proposal_id: u32) -> Result<Proposal, RegistryError>;

    /// The categories a registration declared. Empty for a registration that
    /// predates the taxonomy, or for one that was never registered.
    fn get_categories(env: Env, contract_id: Address) -> Vec<Category>;

    /// Owner-set search tags for a registration. Empty for one that has none,
    /// or that was never registered.
    fn get_tags(env: Env, contract_id: Address) -> Vec<String>;

    /// One page of active registrations filed under `category`, in
    /// registration order.
    ///
    /// `offset` indexes the category's raw index rather than the filtered
    /// result, so a page can come back shorter than `limit` while more active
    /// registrations follow. See the trait docs.
    fn get_active_contracts_by_category(
        env: Env,
        category: Category,
        offset: u32,
        limit: u32,
    ) -> Vec<ContractEntry>;

    /// One page of active registrations filed under **any** of `categories` —
    /// the union, deduplicated, in registration order.
    ///
    /// Errors with `NoCategories` if `categories` is empty. Paging semantics
    /// as for `get_active_contracts_by_category`.
    fn get_active_by_categories(
        env: Env,
        categories: Vec<Category>,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<ContractEntry>, RegistryError>;

    /// `(stake_token, treasury)`, or `StakingNotConfigured` if governance has
    /// not opened staking yet.
    fn get_staking_config(env: Env) -> Result<(Address, Address), RegistryError>;

    /// The per-registration fee. Zero means registration is free.
    fn get_registration_fee(env: Env) -> i128;

    /// Currently staked balance. Zero for a registration that never staked,
    /// and zero — not an error — for an address that was never registered.
    fn get_stake(env: Env, contract_id: Address) -> i128;

    /// Whether governance has attested this registration. False, not an error,
    /// for an address that was never registered.
    fn is_verified(env: Env, contract_id: Address) -> bool;

    /// Whether `contract_id` has a registration at all, active or not.
    ///
    /// This is the cheapest question to ask the registry: one `has` against one
    /// persistent entry, no decoding. Prefer it whenever the answer is a
    /// yes/no gate and the details are not needed.
    fn is_registered(env: Env, contract_id: Address) -> bool;

    /// Aggregate counters: lifetime, active and verified totals, plus the
    /// staked count and amount. Maintained on write, so the read is cheap
    /// apart from the per-registration stake scan.
    fn get_registry_stats(env: Env) -> RegistryStats;

    /// Every slash ever levied against a registration, oldest first. Kept
    /// after deregistration so penalties stay auditable.
    fn get_slashes(env: Env, contract_id: Address) -> Vec<SlashRecord>;

    /// The full reputation signal for a registration. Returns zeroed values
    /// rather than erroring for an unregistered address, matching
    /// `is_registered`'s tolerance.
    fn get_reputation(env: Env, contract_id: Address) -> Reputation;

    /// A registration joined with its reputation — one call instead of
    /// `get_contract` plus `get_reputation`. Errors with `ContractNotFound`
    /// for an address that is not registered.
    ///
    /// **This is the one to reach for when you want both "listed" and
    /// "verified".** The two facts cost one nested invocation here versus two
    /// via `is_registered` + `is_verified`, and the fixed per-call charge is
    /// the part that dominates a cheap read.
    fn get_contract_profile(env: Env, contract_id: Address) -> Result<ContractProfile, RegistryError>;

    /// `get_active_contracts` with each entry's reputation attached.
    fn get_active_profiles(env: Env, offset: u32, limit: u32) -> Vec<ContractProfile>;

    /// The stored metadata entry for a registered contract. Errors with
    /// `ContractNotFound` if there is no registration.
    fn get_contract(env: Env, contract_id: Address) -> Result<ContractEntry, RegistryError>;

    /// Live registrations: deactivated included, deregistered excluded.
    fn get_contract_count(env: Env) -> u32;

    /// Lifetime registrations ever made. Never decremented, so it keeps
    /// counting across deregistration.
    fn get_total_registered(env: Env) -> u32;

    /// Currently listed (active) registrations. This is the figure a stats
    /// page wants.
    fn get_active_contract_count(env: Env) -> u32;

    /// One page of active registrations in registration order.
    ///
    /// `offset` indexes the raw index, so a page can come back shorter than
    /// `limit` while more active registrations follow. See the trait docs.
    fn get_active_contracts(env: Env, offset: u32, limit: u32) -> Vec<ContractEntry>;

    /// As `get_active_contracts`, but only the addresses. Cheaper to decode
    /// and much smaller to return, for a consumer that does not read the
    /// metadata.
    fn get_active_contract_ids(env: Env, offset: u32, limit: u32) -> Vec<Address>;

    /// As `get_active_contracts`, plus `has_more` so the caller can tell an
    /// exhausted index from a short page.
    fn get_active_contracts_page(env: Env, offset: u32, limit: u32) -> ContractPage;

    /// As `get_active_profiles`, plus `has_more`.
    fn get_active_profiles_page(env: Env, offset: u32, limit: u32) -> ContractProfilePage;

    /// Every contract registered by `owner`, **including** deactivated ones.
    fn get_contracts_by_owner(env: Env, owner: Address, offset: u32, limit: u32) -> Vec<ContractEntry>;
}

/// Errors the registry's read-only surface can return.
///
/// Declared in full, with the same discriminants as `lumina_registry::RegistryError`,
/// not just the handful a read can actually produce. A client decodes a
/// contract error by matching on the enum it was generated against, so a
/// variant that is missing here turns a well-defined error into an opaque
/// decode failure. `tests/interface_matches_registry.rs` pins the whole list
/// against the contract's spec, so the two cannot drift.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RegistryError {
    /// Contract is already initialized.
    AlreadyInitialized = 1,
    /// Caller lacks authorization for this action.
    Unauthorized = 2,
    /// Contract is already registered.
    AlreadyRegistered = 3,
    /// Referenced contract was not found.
    ContractNotFound = 4,
    /// Metadata provided is invalid.
    InvalidMetadata = 5,
    /// Caller is not the registered owner of the contract.
    NotOwner = 6,
    /// The registry has no admin because `initialize` was never called.
    NotInitialized = 7,
    /// The referenced proposal does not exist.
    ProposalNotFound = 8,
    /// The proposal has not yet collected enough approvals to be executed.
    ThresholdNotMet = 9,
    /// The timelock delay has not elapsed since the proposal reached threshold.
    TimelockNotElapsed = 10,
    /// This admin has already approved this proposal.
    AlreadyApproved = 11,
    /// Caller is not a member of the admin set.
    NotAdmin = 12,
    /// The admin set would become empty or the threshold would exceed the set
    /// size after this change.
    InvalidThreshold = 13,
    /// The proposal has already been executed.
    AlreadyExecuted = 14,
    /// No stake token / treasury has been set, so staking is not open yet.
    StakingNotConfigured = 15,
    /// A stake or slash amount was zero or negative.
    InvalidAmount = 16,
    /// The registration's staked balance is smaller than the requested amount.
    InsufficientStake = 17,
    /// The stake is still inside the post-slash lock window.
    StakeLocked = 18,
    /// The registration is still active — deactivate before withdrawing.
    RegistrationActive = 19,
    /// A registration must declare at least one category.
    NoCategories = 20,
    /// The registration still holds stake — withdraw it before deregistering.
    StakeNotEmpty = 21,
    /// The registration rate limit configuration is invalid.
    InvalidRateLimit = 22,
    /// The owner is not allowlisted for registration.
    NotAllowlisted = 23,
    /// The registration rate limit has been exceeded.
    RegistrationRateLimited = 24,
    /// Registration fee was not paid.
    InsufficientFee = 25,
    /// Tag count or length exceeds bounds.
    InvalidTags = 26,
    /// Attestation label is empty, too long, or the registration already has
    /// the maximum number of attestations.
    InvalidAttestation = 27,
    /// The caller has no attestation to revoke on this registration.
    AttestationNotFound = 28,
    /// The contract's real token balance is lower than the sum of all tracked
    /// stakes, so the slash would transfer tokens the contract does not hold.
    ///
    /// Caused by accounting drift (fee-on-transfer token, direct drain, or a
    /// rounding bug).  Fee-on-transfer tokens are unsupported by design.
    ContractBalanceInsufficient = 29,
}

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

/// Byte-compatible with `lumina_registry::Category`.
///
/// Append-only: adding a variant needs a registry upgrade, and existing
/// variants are never renamed or repurposed. [`Category::Other`] is the escape
/// hatch in the meantime.
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

/// Byte-compatible with `lumina_registry::SlashRecord`.
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

/// Byte-compatible with `lumina_registry::Reputation`.
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

/// Byte-compatible with `lumina_registry::ContractProfile`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractProfile {
    /// The base registration metadata and status.
    pub entry: ContractEntry,
    /// The reputation and staking signal.
    pub reputation: Reputation,
}

/// Byte-compatible with `lumina_registry::ContractPage`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractPage {
    /// The contracts in this page.
    pub entries: Vec<ContractEntry>,
    /// True if more results are available after this page.
    pub has_more: bool,
}

/// Byte-compatible with `lumina_registry::ContractProfilePage`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractProfilePage {
    /// The profiles in this page.
    pub entries: Vec<ContractProfile>,
    /// True if more results are available after this page.
    pub has_more: bool,
}

/// Byte-compatible with `lumina_registry::RegistryStats`.
#[contracttype]
#[derive(Clone, Debug)]
pub struct RegistryStats {
    /// Total number of registrations ever made.
    pub total_registered: u32,
    /// Number of currently active registrations.
    pub active_count: u32,
    /// Number of verified registrations.
    pub verified_count: u32,
    /// Number of registrations with non-zero stake.
    pub staked_count: u32,
    /// Total staked amount across all registrations.
    pub total_staked: i128,
}

/// Byte-compatible with `lumina_registry::Proposal`.
///
/// Part of the read-only surface so a consumer can inspect what governance is
/// currently attempting — a contract that gates on registry state may reasonably
/// want to refuse while a `deactivate` proposal against it is in flight.
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
    /// `u32::MAX` means the threshold has not yet been reached.
    pub ready_at: u32,
    /// Whether the proposal has already been executed.
    pub executed: bool,
}

/// Byte-compatible with `lumina_registry::ProposalAction`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum ProposalAction {
    /// Deactivate the given contract on behalf of the registry (admin action).
    Deactivate(Address),
    /// Upgrade the contract wasm to the given hash.
    Upgrade(soroban_sdk::BytesN<32>),
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
    /// Set the registration fee in the stake token; zero disables it.
    SetRegistrationFee(i128),
}
