// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
#no_std
// Soroban's `#[contracttype]`, `#[contracterror]`, `#[contractimpl]` and
// `#[contractclient]` macros emit synthetic items — the `SPEC` constants, the
// generated client methods, the error-code helpers — carrying the invocation
// site's span. `missing_docs` reports those as undocumented and there is no
// source position to attach a doc comment to, so on current rustc the lint
// cannot be satisfied by any edit to this crate. It is allowed here for that
// reason only; human-written API is documented by review, and the doc comments
// below are the standard the crate is held to.
#![allow(missing_docs)]
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
//! [`soroban_sdk_contractclient`] generates from it.
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
//!
//! ## Reentrancy across the token transfer boundary
//!
//! The registry moves tokens in three paths: `stake`, `withdraw_stake`, and the
//! slash path that governance drives. Each of these calls into an external
//! token contract, which is code the registry does not control. The ordering
//! therefore matters:
//!
//! - **State is written before the external call.** Every path that moves
//!   tokens follows checks-effects-interactions: the stored balance is
//!   updated first, then the transfer is issued. A token that reenters
//!   `withdraw_stake` during its own `transfer` sees a zero balance and
//!   cannot withdraw twice.
//!
//! - **Soroban does not guarantee atomicity of a cross-contract call.**
//!   The host does not prevent reentrancy, and it does not roll back a
//!   partially-completed call automatically unless the call returns an
//!   error or panics. A callee that returns successfully after mutating
//!   state leaves that mutation in place. The registry therefore cannot
//!   rely on the host to defend it; it must order its own writes.
//!
//! - **Authorization is not a reentrancy defense.** `require_auth` is checked
//!   once at the entrypoint and does not gate nested calls that the same
//!   authorized address makes. A token that the registry calls can call back
//!   into the registry with the registry's own authority still in force.
//!
//! The guarantee this crate documents is therefore a *contract-level* one,
//! not a host-level one: every token-moving entrypoint writes its state
//! before it calls out. The test suite exercises this with a reentrant token
//! contract that attempts a double withdrawal and asserts the second
//! attempt fails.

use soroban_sdk::{contractclient, contracterror, contracttype, Address, Env, String, Vec};

/// The read-only half of the Lumina Registry.
//.
/// Every method here corresponds one-to-one to an export the registry contract
/// actually has, with the same name and the same arguments; nothing here mutates
/// state and nothing here requires authorization. A consumer that only
/// ever needs to *read* the registry should depend on this trait rather than
/// on the contract crate.
///
/// Every `contract_id` parameter is a **contract** address (`C…`), never a
/// wallet/account address (`G…`). Registration rejects `G…` addresses, so a
/// `G…` passed to any of these reads is simply not registered. This is the
/// invariant that lets a consumer build an indexer filter over the registered
/// set without first filtering out accounts itself.
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
/// - `get_all_contracts` likewise includes deactivated entries, because it is
///   the registry-wide audit view.
///
/// ## Owner index cap
///
/// The registry maintains a per-owner index of the contracts that owner
/// has registered (`DataKey::OwnerContracts(Address)`). That index is a
/// bounded `Vec<Address>`, and an owner may register at most
/// [`MAX_CONTRACTS_PER_OWNER`] contracts. Attempting to register one more
/// fails with [`RegistryError::OwnerContractLimitReached`], rather than
/// letting the index grow until the entry can no longer be written. The cap
/// is per owner, not global, and registrations under the cap are unaffected.
/// Consumers that need to walk an owner's entire list should page through
/// `get_contracts_by_owner`.
///
/// The cap is documented on [`MAX_CONTRACTS_PER_OWNER`] and is part of
/// the registry's public behavior: the contract enforces it on registration
/// and the interface exposes the corresponding error code.
///
/// [`MAX_CONTRACTS_PER_OWNER`]: const MAX_CONTRACTS_PER_OWNER
#[contractclient(name = "RegistryInterfaceClient")]
pub trait RegistryInterface {
    /// Which build of the registry is live at this address.
    fn get_version(env: Env) -> u32;

    /// The address an owner has delegated registration management to, if any.
    fn get_manager(env: Env, contract_id: Address) -> Option<Address>;

    /// The first admin address. Errors with `NotInitialized` before the
    /// registry has been set up.
    fn get_admin(env: Env) -> Result<Address, RegistryError>;

    /// The full current admin set. Errors with `NotInitialized` if empty.
    fn get_admins(env: Env) -> Result<Vec<Address>, RegistryError>;

    /// The number of approvals a proposal needs. Errors with `NotInitialized`
    /// before the registry has been set up.
    fn get_threshold(env: Env) -> Result<u32, RegistryError>;

    /// Retrieve a governance proposal by ID.
    ///
    /// The returned [`Proposal`] carries `expires_at`, the ledger sequence at
    /// which the proposal stops being executable. A UI can compare it against
    /// the current ledger to show a countdown, and a client can refuse to build
    /// an execution transaction that the contract would reject anyway. See
    /// `Proposal::expires_at` for the definition.
    fn get_proposal(env: Env, proposal_id: u32) -> Result<Proposal, RegistryError>;

    /// Cancel a governance proposal before it executes.
    ///
    /// Callable by the proposer, or by a threshold of admins. A cancelled
    /// proposal cannot be approved or executed, even once threshold
    /// approvals and the timelock have been met. Emits `proposal_cancelled`.
    ///
    /// Errors with `ProposalAlreadyExecuted` if the proposal has already
    /// executed, and with `ProposalAlreadyCancelled` if it was already
    /// cancelled. Errors with `NotAdmin` if `admin` is not the proposer and
    /// not a member of the admin set.
    fn cancel_proposal(env: Env, admin: Address, proposal_id: u32) -> Result<(), RegistryError>;

    /// The timelock duration, in ledgers, that applies to a given action.
    fn get_action_timelock(env: Env, action: ProposalAction) -> u32;

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

    /// Cursor form of `get_active_contracts_by_category`, for walking a whole
    /// category without re-reading it page by page. `cursor` is the
    /// `contract_id` last returned, or `None` to start; the position is stable
    /// against registrations added mid-walk.
    fn get_contracts_by_category_after(
        env: Env,
        category: Category,
        cursor: Option<Address>,
        limit: u32,
    ) -> Vec<ContractEntry>;

    /// One page of active registrations filed under **any** of `categories` —
/// the union, deduplicated, in registration order.
///
/// Errors with `NoCategories` if `categories` is empty. Paging semantics
/// as for `get_active_contracts_by_category`.
    fn get_contracts_by_categories(
        env: Env,
        categories: Vec<Category>,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<ContractEntry>, RegistryError>;

/// `(stake_token, treasury, decimals)`, or `StakingNotConfigured` if
    /// governance has not opened staking yet.
    ///
    /// `decimals` is the stake token's own `decimals()`, read from the token
    /// contract when staking was configured and cached alongside the token
    /// address. Stake amounts are raw `i128` values, so a caller that wants to
    /// render one as a human number needs this to scale it correctly.
    fn get_staking_config(env: Env) -> Result<(Address, Address, u32), RegistryError>;

    /// The per-registration fee. Zero means registration is free.
    fn get_registration_fee(env: Env) -> i128;

    /// The stake a registration has to hold to stay listed. Zero means the
/// threshold is not open — nothing is refused for being under-staked.
    fn get_minimum_stake(env: Env) -> i128;

    /// Currently staked balance. Zero for a registration that never staked,
/// and zero — not an error — for an address that was never registered.
    fn get_stake(env: Env, contract_id: Address) -> i128;

/// The ledger at which an in-progress unbonding completes, or zero if no
    /// unbonding is in progress. `withdraw_stake` refuses until the current
    /// ledger reaches this value. The unbonding period is deliberately longer
    /// than the governance timelock, so a slash proposal cannot be outrun by
    /// deactivating and withdrawing.
    fn get_unbonding_completes_at(env: Env, contract_id: Address) -> u32;

    /// The amount `staker` has personally backed `contract_id` with. Zero for
    /// a staker who never contributed, and zero — not an error — for an
    /// address that was never registered.
    ///
    /// Stake is tracked per (registration, staker), so any address may
    /// back a registration it does not own, and each staker withdraws only
    /// their own contribution. `get_stake` reports the sum across all of
    /// them.
    fn get_stake_of(env: Env, contract_id: Address, staker: Address) -> i128;

    /// Every address that has a currently nonzero stake on `contract_id`,
    /// in the order they first staked. Empty for a registration with no
    /// stakers, and for an address that was never registered.
    fn get_stakers_of(env: Env, contract_id: Address) -> Vec<Address>;
    /// Whether governance has attested this registration. False, not an error,
    /// for an address that was never registered.
    fn is_verified(env: Env, contract_id: Address) -> bool;

    /// Whether `contract_id` has a registration at all, active or not.
///
    /// This is the cheapest question to ask the registry: one `has` against one
    /// persistent entry, no decoding. Prefer it whenever the answer is a
    /// yes/no gate and the details are not needed.
    ///
    /// `contract_id` is a contract address (`C…`); a `G…` account address is
    /// never registered and returns `false`. Registration refuses `G…`
    /// addresses, so a `G` in the registry is not a state this read can
    /// observe — the downstream `isContractAddress` filter that
    /// `lumina-backend/indexer/src/index.ts` had to add
    fn is_registered(env: Env, contract_id: Address) -> bool;

    /// Aggregate counters: lifetime, active and verified totals, plus the
    /// staked count and amount. Maintained on write, so the read is cheap
    /// apart from the per-registration stake scan.
    fn get_registry_stats(env: Env) -> RegistryStats;

    /// Every slash ever levied against a registration, oldest first. Kept
    /// after deregistration so penalties stay auditable.
    fn get_slashes(env: Env, contract_id: Address) -> Vec<SlashRecord>;

    /// Every third-party attestation recorded against a registration, oldest
    /// first. Attestations are claims, not the governance `is_verified`
    /// signal: they are published so a reader can weigh them, and an empty
    /// list is an answer rather than an error.
    fn get_attestations(env: Env, contract_id: Address) -> Vec<Attestation>;

    /// The full reputation signal for a registration. Returns zeroed values
    /// rather than erroring for an unregistered address, matching
    /// `is_registered`s tolerance.
    fn get_reputation(env: Env, contract_id: Address) -> Reputation;

    /// A registration joined with its reputation — one call instead of
    /// `get_contract` plus `get_reputation`. Errors with `ContractNotFound`
    /// for an address that is not registered.
///
    /// **This is the one to reach for when you want both "listed" and
    /// "verified".** The two facts cost one nested invocation here versus two
    /// via `is_registered` + `is_verified`, and the fixed per-call charge is
    /// the part that dominates a cheap read.
    fn get_contract_profile(
        env: Env,
        contract_id: Address,
    ) -> Result<ContractProfile, RegistryError>;

    /// Whether `contract_id` is both active and verified — the gate most
    /// consumers actually want. Equivalent to `get_contract_profile` and
    /// checking both flags, but cheaper than two separate calls.
    fn is_listed(env: Env, contract_id: Address) -> bool;

    /// Everything the registry knows about one contract, in a single call.
    ///
    /// This is the read to prefer when a consumer needs more than one fact:
    /// it avoids the fixed per-call cost of a second cross-contract invocation.
    /// Errors with `NotRegistered` for an address that has no registration.
    fn get_contract_profile(env: Env, contract_id: Address) -> Result<ContractProfile, RegistryError>;

    /// The number of registered contracts, active or not.
    fn get_total_contracts(env: Env) -> u32;

    /// The number of active registrations.
    fn get_active_contract_count(env: Env) -> u32;

    /// The number of registrations governance has verified.
    fn get_verified_count(env: Env) -> u32;

    /// One page of active registrations in registration order.
///
    /// `offset` indexes the raw index, so a page can come back shorter than
    /// `limit` while more active registrations follow. Deprecated in favour of
    /// `get_active_contracts_after`; see the trait docs.
    fn get_active_contracts(env: Env, offset: u32, limit: u32) -> Vec<ContractEntry>;

    /// Cursor form of `get_active_contracts`. Pass the `contract_id` of the
    /// last entry the previous call returned (or `None` to start) and walk
    /// until an empty page. Cheaper than offset paging and stable against
    /// registrations added mid-walk.
    fn get_active_contracts_after(
        env: Env,
        cursor: Option<Address>,
        limit: u32,
    ) -> Vec<ContractEntry>;

    /// As `get_active_contracts`, but only the addresses. Cheaper to decode
    /// and much smaller to return, for a consumer that does not read the
    /// metadata.
    fn get_active_contract_ids(env: Env, offset: u32, limit: u32) -> Vec<Address>;

    /// The number of governance proposals ever created.
    fn get_proposal_count(env: Env) -> u32;

    /// The number of governance proposals that have been executed.
    fn get_executed_proposal_count(env: Env) -> u32;

    /// Whether a governance proposal has been executed.
    fn is_proposal_executed(env: Env, proposal_id: u32) -> bool;

    /// Whether a governance proposal has been cancelled.
    fn is_proposal_cancelled(env: Env, proposal_id: u32) -> bool;

    /// Returns active registrations ordered by staked amount descending, paginated.
    /// Ties are broken by registration order (ascending index).
    fn get_active_contracts_by_stake_page(env: Env, offset: u32, limit: u32) -> ContractPage;

    /// Returns active profiles ordered by staked amount descending, paginated.
    /// Ties are broken by registration order (ascending index).
    fn get_active_profiles_by_stake_page(env: Env, offset: u32, limit: u32) -> ContractProfilePage;

    /// Every contract registered by `owner`, **including** deactivated ones.
///
    /// The underlying per-owner index is capped at [`MAX_CONTRACTS_PER_OWNER`]
    /// entries, so this list is bounded and can be walked by paging. An owner
    /// that hits the cap gets [`RegistryError::OwnerContractLimitReached`]
    /// from registration, not an opaque storage failure.
    fn get_contracts_by_owner(env: Env, owner: Address, offset: u32, limit: u32) -> Vec<ContractEntry>;

    /// Cursor form of `get_contracts_by_owner`, including deactivated entries.
    /// `cursor` is the `contract_id` last returned, or `None` to start.
    fn get_contracts_by_owner_after(
        env: Env,
        owner: Address,
        cursor: Option<Address>,
        limit: u32,
    ) -> Vec<ContractEntry>;

    /// Whether an address is in the governance admin set.
    fn is_admin(env: Env, address: Address) -> bool;

    /// The admins that have approved a governance proposal.
    fn get_proposal_approvals(env: Env, proposal_id: u32) -> Result<Vec<Address>, RegistryError>;
}

/// The maximum number of contracts a single owner may register.
///
/// The registry stores an owner's contracts in a single
/// `DataKey::OwnerContracts(Address)` entry that is rewritten on every
/// registration. Without a cap, an owner registering many contracts makes
/// each subsequent registration more expensive, until the entry can no
/// longer be written and that owner can no longer register anything.
///
/// This constant is the bound. Registrations below it are unaffected; a
/// registration that would exceed it fails with
/// [`RegistryError::OwnerContractLimitReached`]. The cap is per owner,
/// not global.
///
/// The value is part of the registry's public behavior and is pinned by
/// `tests/interface_matches_registry.rs` against the contract's spec.
///
/// [`RegistryError::OwnerContractLimitReached`]: RegistryError::OwnerContractLimitReached
pub const MAX_CONTRACTS_PER_OWNER: u32 = 100;

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
    /// No registration exists for this contract.
    ContractNotFound = 4,
/// Metadata is invalid.
    InvalidMetadata = 5,
    /// Caller is not the owner.
    NotOwner = 6,
    /// Contract has not been initialized.
    NotInitialized = 7,
    /// Proposal does not exist.
    ProposalNotFound = 8,
    /// Proposal has not met the approval threshold.
    ThresholdNotMet = 9,
    /// Proposal timelock has not elapsed.
    TimelockNotElapsed = 10,
    /// Admin has already approved this proposal.
    AlreadyApproved = 11,
    /// Caller is not an admin.
    NotAdmin = 12,
    /// Threshold is invalid.
    InvalidThreshold = 13,
    /// Proposal has already been executed.
    AlreadyExecuted = 14,
    /// Staking has not been configured.
    StakingNotConfigured = 15,
/// Amount is invalid.
    InvalidAmount = 16,
    /// Stake is insufficient.
    InsufficientStake = 17,
    /// Stake is locked.
    StakeLocked = 18,
    /// Registration is active.
    RegistrationActive = 19,
    /// No categories were provided.
    NoCategories = 20,
/// The registration claims more categories than `MAX_CATEGORIES_PER_CONTRACT`.
    TooManyCategories = 28,
    /// Stake is not empty.
    StakeNotEmpty = 21,
    /// Rate limit is invalid.
    InvalidRateLimit = 22,
    /// Owner is not allowlisted.
    NotAllowlisted = 23,
    /// Registration rate limit exceeded.
    RegistrationRateLimited = 24,
    /// Registration fee is insufficient.
    InsufficientFee = 25,
    /// Tags are invalid.
    InvalidTags = 26,
/// Attestation is invalid.
    InvalidAttestation = 27,
    /// Attestation was not found.
    AttestationNotFound = 28,
}

/// A single attestation recorded against a contract.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attestation {
    /// The admin who attested.
    pub attester: Address,
    /// Ledger timestamp of the attestation.
    pub created_at: u32,
    /// Free-form label.
    pub label: String,
}

/// A governance proposal.
///
/// A registration that claims every category is not categorised in any useful
/// sense — it is spam in a discovery surface. Claiming more than this cap is
/// rejected with [`RegistryError::TooManyCategories`] rather than silently
/// truncated.
pub const MAX_CATEGORIES_PER_CONTRACT: u32 = 5;

/// A registration entry in the manifest.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractEntry {
    /// Whether the registration is active.
    pub active: bool,
    /// The registered contract address.
    pub contract_id: Address,
    /// Human-readable description.
    pub description: String,
    /// Human-readable name.
    pub name: String,
    /// The owner address.
    pub owner: Address,
    /// Ledger timestamp of registration.
    pub registered_at: u32,
/// Whether indexing is currently active for this contract.
    pub active: bool,
    /// Address the owner delegated registration management to, if any.
    pub manager: Option<Address>,
}

/// The action a governance proposal would take.
///
/// Duplicated from `lumina-registry`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProposalAction {
    /// Add an admin.
    AddAdmin(Address),
    /// Remove an admin.
    RemoveAdmin(Address),
    /// Change the approval threshold.
    SetThreshold(u32),
    /// Pause the registry.
    Pause,
    /// Unpause the registry.
    Unpause,
}

/// A category a registration can be filed under.
///
/// Duplicated from `lumina-registry`; see the crate docs for why.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractPage {
    /// The entries in this page.
    pub entries: Vec<ContractEntry>,
    /// Whether more entries follow.
    pub has_more: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    /// Monotonic identifier assigned at creation.
    pub id: u32,
    /// The action the proposal would take if approved.
    pub action: ProposalAction,
    /// Optional human-readable rationale, bounded by
    /// `MAX_PROPOSAL_DESCRIPTION_LEN` bytes. Empty when the proposer
    /// supplied none.
    pub description: String,
    /// Address that created the proposal.
    pub proposer: Address,
    /// Ledger at which the proposal was created.
    pub created_at: u32,
    /// Ledger after which the proposal can no longer be voted on.
    pub expires_at: u32,
    /// Approvals recorded so far.
    pub approvals: u32,
    /// Whether the proposal has been executed or rejected.
    pub finalized: bool,
}
}

/// A single registration entry.
///
/// Enforced at creation; a longer description is rejected with
/// `InvalidDescription`. Documented here so clients can validate before
/// submitting a transaction.
pub const MAX_PROPOSAL_DESCRIPTION_LEN: u32 = 256;

/// A registration joined with its reputation.
///
/// Byte-compatible with `lumina_registry::SlashRecord`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractProfile {
    /// The registration entry.
    pub entry: ContractEntry,
    /// The registration's reputation.
    pub reputation: Reputation,

#[derive(Clone, Debug, PartialEq)]
pub struct SlashRecord {
    /// How much stake was taken.
    pub amount: i128,
    /// Why governance slashed — recorded on-chain for accountability.
    pub reason: String,
    /// Ledger at which the slash executed.
    pub slashed_at: u32,
    /// Owner's optional response to the slash.
    pub response: Option<String>,
}

/// Byte-compatible with `lumina_registry::Attestation`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Attestation {
    /// Who made the claim, so it is attributable and revocable.
    pub attester: Address,
    /// Short, bounded free-text label describing the basis of the claim.
    pub label: String,
    /// Ledger at which the attestation was made.
    pub created_at: u32,
}

/// A page of registration profiles.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractEntry {
    /// The registered contract address.
    public contract_id: Address,
    /// The owner that registered it.
    public owner: Address,
    /// The display name.
    public name: String,
    /// The description.
    public description: String,
    /// The canonical URL.
    public url: String,
    /// The categories the registration declared.
    public categories: Vec<Category>,
    /// Owner-set search tags.
    public tags: Vec<String>,
    /// Whether the registration is active.
    public active: bool,
    /// The ledger timestamp the registration was made at.
    public registered_at: u64,
}

/// A governance proposal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlashRecord {
    /// The amount slashed.
    public amount: i128,
    /// The reason given for the slash.
    public reason: String,
    /// The ledger timestamp the slash was levied at.
    public timestamp: u64,
}
}

/// One entry in a batch registration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrationEntry {
    /// Categories to file the registration under.
    pub categories: Vec<Category>,
    /// The contract address.
    pub contract_id: Address,
    /// Human-readable description.
    pub description: String,
    /// Human-readable name.
    pub name: String,
}

/// The reputation signal for a registration.
///
/// Duplicated from `lumina-registry`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reputation {
    /// The current reputation score.
    pub score: i128,
    /// The number of slashes levied.
    pub slash_count: u32,
    /// The total amount slashed.
    pub total_slashed: i128,
    /// Whether the registration is verified.
    pub verified: bool,
}
}

/// Aggregate registry counters.
///
/// Duplicated from `lumina-registry`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryStats {
    /// Lifetime registrations.
    pub total_registered: u32,
    /// Active registrations.
    pub active_count: u32,
    /// Verified registrations.
    pub verified_count: u32,
    /// Registrations with a non-zero stake.
    pub staked_count: u32,
    /// Total amount staked.
    pub total_staked: i128,
}

/// A window of registration attempts for rate limiting.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrationWindow {
    /// Number of registrations in the window.
    pub count: u32,
    /// Ledger timestamp the window opened at.
    pub started_at: u32,
}

/// Aggregate registry counters.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryStats {
    /// Currently active registrations.
    pub active_count: u32,
    /// Registrations with a nonzero stake.
    pub staked_count: u32,
    /// Lifetime registrations.
    pub total_registered: u32,
    /// Total amount staked.
    pub total_staked: i128,
    /// Registrations with verified status.
    pub verified_count: u32,
}
}

/// The reputation signal for a registration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reputation {
    /// Total amount slashed.
    pub slashed_total: i128,
    /// Currently staked amount.
    pub stake: i128,
    /// Whether the registration is verified.
    pub verified: bool,
    /// Ledger timestamp the withdrawal lock expires at.
    pub withdraw_locked_until: u32,
}

/// A slash record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlashRecord {
    /// Amount slashed.
    pub amount: i128,
    /// Reason for the slash.
    pub reason: String,
    /// Ledger timestamp of the slash.
    pub slashed_at: u32,
}

/// The registry's category taxonomy.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum Category {
    /// DeFi protocols.
    DeFi,
    /// NFT projects.
    Nft,
    /// Gaming.
    Gaming,
    /// Identity.
    Identity,
    /// Infrastructure.
    Infrastructure,
    /// Payments.
    Payments,
    /// Oracles.
    Oracle,
    /// DAO.
    Dao,
    /// Other.
    Other,
}

/// The governance action a proposal carries.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProposalAction {
    /// Deactivate a contract.
    Deactivate(Address),
    /// Upgrade the registry WASM.
    Upgrade(BytesN<32>),
    /// Add an admin.
    AddAdmin(Address),
    /// Remove an admin.
    RemoveAdmin(Address),
    /// Change the approval threshold.
    ChangeThreshold(u32),
    /// Configure the staking token and treasury.
    ConfigureStaking(Address, Address),
    /// Set a contract's verified status.
    SetVerified(Address, bool),
    /// Slash a contract's stake.
    Slash(Address, i128, String),
    /// Enable or disable the owner allowlist.
    SetAllowlistEnabled(bool),
    /// Add or remove an owner from the allowlist.
    SetAllowlisted(Address, bool),
    /// Configure the registration rate limit.
    ConfigureRegistrationRateLimit(u32, u32),
    /// Set the registration fee.
    SetRegistrationFee(i128),
    /// Set the minimum stake threshold; zero disables it.
    ConfigureMinimumStake(i128),
    /// Withdraw from the treasury.
    WithdrawFromTreasury(i128),
}

/// Number of ledgers a proposal of a given action must wait before execution.
pub const TIMELOCK_LEDGERS_UPGRADE: u32 = 17_280;
/// Number of ledgers a proposal of a given action must wait before execution.
pub const TIMELOCK_LEDGERS_ADMIN: u32 = 17_280;
/// Number of ledgers a proposal of a given action must wait before execution.
pub const TIMELOCK_LEDGERS_STANDARD: u32 = 720;