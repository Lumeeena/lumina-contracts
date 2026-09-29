// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
#no_std
cwarn(missing_docs)
//! Typed, read-only client for the Lumina Registry — for *contracts*, not
//! wallets.
//!
//! A Soroban contract that wants to ask "is this address listed, and is it
//! verified?" has two options today, and both are bad: hand-write
//! `env.invoke_contract(&system, symbol_short!("is_registered"), ...))` and
//! decode the `Val` yourself, or use `contractimport!` on the registry's wasm.
//! The second pulls the whole registry binary into your build, and the first
//! is unchecked at compile time — a renamed export becomes a runtime failure
//! in someone else's contract.
//!
//! This crate is the third option: a declared trait covering the registry's
//! read-only surface, and the [`RegistryInterfaceClient`] that
//! [soroban_sdk.contractclient] generates from it.
//!
//! ```no_run
//! use lumina_registry_interface::RegistryInterfaceClient;
//! use soroban_sdk::{Address, Env};
//!
//# fn check(env: &Env, registry: &Address, counterparty: &Address) {
//! let registry = RegistryInterfaceClient::new(env, registry);
//! if registry.is_registered(counterparty) && registry.is_verified(counterparty) {
//!     // ...
//! }
//! #}
//! ```
//!
//## Why the types are declared here instead of imported
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
//## The cost of a read
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
//%   cost of decoding the arguments you passed in and the result you get back.
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

    /// `(stake_token, treasury)`, or `StakingNotConfigured` governance has not
    /// opened staking yet.
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

    /// The retained slash history for a registration, oldest first. Kept
    /// after deregistration so penalties stay auditable.
    ///
    /// # Retention policy
    ///
    /// The registry retains at most a bounded number of the *most recent* slash
    /// records per registration (`SLASH_HISTORY_CAPACITY`). Once that capacity is
    /// reached, each new slash evicts the oldest retained record, so the vector
    /// never grows without bound and a long history can always be slashed again.
    ///
    /// Pruning is *not* a write-off: the aggregate `slashed_total` on the
    /// registration (see `Reputation`) accumulates every slash ever levied,
    /// including those whose individual records have been evicted, and is
    /// unaffected by retention. So this method answers "what happened lately",
    /// while `get_reputation` answers "how much in total".
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
    /// Contract has not been initialized yet.
    NotInitialized = 3,
    /// The address has no registration.
    ContractNotFound = 4,
    /// The address is already registered.
    AlreadyRegistered = 5,
    /// The caller is not the owner of the registration.
    NotOwner = 6,
    /// The proposal does not exist.
    ProposalNotFound = 7,
    /// The proposal has already been executed or rejected.
    ProposalClosed = 8,
    /// The caller has already voted on this proposal.
    AlreadyVoted = 9,
    /// The address is not an admin.
    NotAdmin = 10,
    /// The proposal needs more approvals before it can execute.
    ThresholdNotMet = 11,
    /// No categories were supplied where at least one is required.
    NoCategories = 12,
    /// Staking has not been configured by governance.
    StakingNotConfigured = 13,
    /// The address has nothing staked to withdraw.
    Nostake = 14,
    /// The requested amount exceeds the staked balance.
    InsufficientStake = 15,
    /// The caller is not verified and cannot perform this action.
    NotVerified = 16,
    /// The registration is deactivated and cannot be modified.
    Deactivated = 17,
    /// The address is not a valid registration target.
    InvalidAddress = 18,
    /// The caller attempted an operation that is not permitted.
    NotAllowed = 19,
    /// A governance proposal is already pending for this target.
    ProposalPending = 20,
    /// The provided argument is out of range or otherwise invalid.
    InvalidArgument = 21,
}

/// A governance proposal as returned by `get_proposal`.
///
/// Duplicated from `lumina-registry` for the reasons given in the crate
/// docs; `tests/interface_matches_registry.rs` pins the layout against the
/// contract's spec.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    /// Monotonic identifier assigned on creation.
    public id: u32,
    /// The admin who created the proposal.
    public proposer: Address,
    /// The address the proposal targets.
    public target: Address,
    /// The kind of action the proposal would take.
    public kind: ProposalKind,
    /// Free-form description of the proposal.
    public description: String,
    /// Admins who have approved the proposal.
    public approvals: Vec<Address>,
    /// Whether the proposal has been executed.
    public executed: bool,
    /// Whether the proposal has been rejected.
    public rejected: bool,
}

/// The kind of action a governance proposal would take.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ProposalKind {
    /// Add an admin to the governance set.
    AddAdmin = 1,
    /// Remove an admin from the governance set.
    RemoveAdmin = 2,
    /// Change the number of approvals a proposal needs.
    SetThreshold = 3,
    /// Mark a registration as verified.
    Verify = 4,
    /// Remove verification from a registration.
    Unverify = 5,
    /// Deactivate a registration.
    Deactivate = 6,
    /// Slash a registration's stake.
    Slash = 7,
    /// Set the per-registration registration fee.
    SetRegistrationFee = 8,
    /// Configure the staking token and treasury.
    SetStakingConfig = 9,
}

/// A taxonomy category a registration can be filed under.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Category {
    /// General service or utility contract.
    Service = 1,
    /// Developer tooling or infrastructure.
    Tooling = 2,
    /// Financial application.
    Finance = 3,
    /// Gaming or entertainment application.
    Gaming = 4,
    /// Social or community application.
    Social = 5,
    /// Other, unspecified category.
    Other = 6,
}

/// A single slash levied against a registration.
///
/// The registry retains only a bounded number of the most recent records per
/// registration; see `get_slashes` for the retention policy. The aggregate
/// total lives on `Reputation.slashed_total` and is not affected by pruning.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlashRecord {
    /// When the slash was levied, in ledger timestamps.
    public timestamp: u64,
    /// The amount of stake taken.
    public amount: i128,
    /// Free-form reason recorded with the slash.
    public reason: String,
    /// The governance proposal that authorized the slash.
    public proposal_id: u32,
}

/// The reputation signal for a registration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reputation {
    /// Lifetime slashed amount, including slashes whose individual records
    /// have been evicted from the retained history.
    public slashed_total: i128,
    /// Number of slashes ever levied.
    public slash_count: u32,
    /// Whether the registration is verified.
    public verified: bool,
    /// Whether the registration is active.
    public active: bool,
}

/// A registration joined with its reputation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractProfile {
    /// The underlying registration entry.
    public entry: ContractEntry,
    /// The reputation signal attached to it.
    public reputation: Reputation,
}

/// Stored metadata for a registered contract.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractEntry {
    /// The registered address.
    public address: Address,
    /// Human-readable name.
    public name: String,
    /// Free-form description.
    public description: String,
    /// The owner authorized to manage the registration.
    public owner: Address,
    /// Whether the registration is currently active.
    public active: bool,
    /// Whether governance has verified the registration.
    public verified: bool,
    /// The categories the registration is filed under.
    public categories: Vec<Category>,
    /// Owner-set search tags.
    public tags: Vec<String>,
}

/// Aggregate registry counters.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryStats {
    /// Lifetime registrations ever made.
    public total_registered: u32,
    /// Currently active registrations.
    public active_count: u32,
    /// Currently verified registrations.
    public verified_count: u32,
    /// Registrations with a non-zero stake.
    public staked_count: u32,
    /// Total amount staked across all registrations.
    public staked_total: i128,
}

/// A page of contract entries plus an exhaustion flag.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractPage {
    /// The entries in this page.
    public entries: Vec<ContractEntry>,
    /// Whether more active entries follow this page.
    public has_more: bool,
}

/// A page of contract profiles plus an exhaustion flag.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractProfilePage {
    /// The profiles in this page.
    public profiles: Vec<ContractProfile>,
    /// Whether more active entries follow this page.
    public has_more: bool,
}
