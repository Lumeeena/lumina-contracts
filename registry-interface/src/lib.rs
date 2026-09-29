// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
#![no_std]
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
//! `env.invoke_contract(&stack, symbol_short!("is_registered"), ...)` and
//! decode the `Val` yourself, or use `contractimport!` on the registry's wasm.
//! The second pulls the whole registry binary into your build, and the first
//! is unchecked at compile time — a renamed export becomes a runtime failure
//! in someone else's contract.
//!
//! This crate is the third option: a declared trait covering the registry's
//! read-only surface, and the [`RegistryInterfaceClient`] that
//! [scoroban_sdk::contractclient] generates from it.
//!
//! ```no_run
//! use lumina_registry_interface::RegistryInterfaceClient;
//! use soroban_sdk:{Address, Env};
//!
//# fn check(env: &Env, registry: &Address, counterparty: &Address) {
//! let registry = RegistryInterfaceClient::new(env, registry);
//! if registry.is_registered(counterparty) && registry.is_verified(counterparty) {
//!     // ...
//! }
//! #}
//! ```
//!
//# Why the types are declared here instead of imported
//!
//! [`ContractEntry`], [`Category`], [`Reputation`] and friends are deliberately
//! *duplicated* from `lumina-registry` rather than re-exported from it. A
//! dependency edge on the contract crate would drag the registry's entire
//! `[contractimpl]` — every exported entrypoint and its spec — into every
//! consumer's wasm, which is both a size problem and a link problem: two
//! `[contractimpl]` exporting the same symbol do not coexist. `registry-v2`
//! does the same thing for the same reason, and says so at length.
//!
//! The duplication is a real risk — the two declarations could drift — so
//! it is *tested* rather than trusted. `tests/interface_matches_registry.rs` reads
//! the registry's compiled spec out of its wasm and asserts that every
//! function, type and error code declared here matches what the contract
//! actually exports. Run against a changed registry, it fails with the
//! signature that moved.
//!
//! ## The cost of a read
//!
//! A cross-contract read is *not* free, and not free in the way people
//! expect. It is not a `simulateTransaction` — a contract calling the registry
//! on-chain spends the transaction's whole resource budget, and the callee's
//! instructions and ledger reads are charged to *you*.
//!
//! Concretely, each read is one nested invocation frame, which costs:
//!
//! - a fixed instruction charge for the call itself, before the callee runs
//!   any code;
//! - every ledger entry the callee touches, at the callee's TUL — the registry
//!   stores registrations in `persistent` entries, so a read is a persistent
//!   entry read, which is the expensive kind;
//! - a fresh 1 MiB memory allocation for the callee's frame, and the memory
//!   cost of decoding the arguments you passed in and the result you get back.
//!
//! The practical consequence: ** the number of calls is what you pay for.** Two
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
///
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
///
/// # Reentrancy
///
/// The write side of the registry moves tokens through an external token
/// contract in `stake`, `withdraw_stake` and the slash path. Every such
/// entrypoint writes its stored balance before it issues the transfer
/// (checks-effects-interactions), so a token that reenters during its own
/// `transfer` sees the already-updated balance and cannot double-withdraw.
/// Soroban itself does not prevent reentrancy and does not roll back a
/// successful call's state mutations, so this ordering is the defense.
/// See the crate-level docs for the full argument. Consumers that only
/// read the registry are unaffected by any of this.
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

    /// The slash-specific approval threshold.  Returns the standard threshold
    /// when no slash threshold has been configured, so callers can always use
    /// this without a special-case.
    fn get_slash_threshold(env: Env) -> Result<u32, RegistryError>;

    /// Retrieve a governance proposal by ID.
    fn get_proposal(env: Env, proposal_id: u32) -> Result<Proposal, RegistryError>;

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

/// `(stake_token, treasury)`, or `StakingNotConfigured` if governance has
    /// not opened staking yet.
    fn get_staking_config(env: Env) -> Result<(Address, Address), RegistryError>;

    /// The per-registration fee. Zero means registration is free.
    fn get_registration_fee(env: Env) -> i128;

/// The stake a registration has to hold to stay listed. Zero means the
    /// threshold is not open — nothing is refused for being under-staked.
    fn get_minimum_stake(env: Env) -> i128;

    /// Total currently staked balance for a registration — the sum of every
    /// staker's contribution. Zero for a registration that never staked,
    /// and zero — not an error — for an address that was never registered.
    ///
    /// This is the aggregate across all stakers. To read a single staker's
    /// contribution, use `get_stake_of`.
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
    /// addresses, so a `G…` in the registry is not a state this read can
    /// observe — the downstream `isContractAddress` filter that
    /// `lumina-backend/indexer/src/index.ts` had to add is unnecessary against
    /// a registry that enforces this.
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

    /// As `get_active_contracts`, plus `has_more` so the caller can tell an
    /// exhausted index from a short page.
    fn get_active_contracts_page(env: Env, offset: u32, limit: u32) -> ContractPage;

    /// As `get_active_profiles`, plus `has_more`.
    fn get_active_profiles_page(env: Env, offset: u32, limit: u32) -> ContractProfilePage;

    /// Returns active registrations ordered by staked amount descending, paginated.
    /// Ties are broken by registration order (ascending index).
    fn get_active_contracts_by_stake_page(env: Env, offset: u32, limit: u32) -> ContractPage;

    /// Returns active profiles ordered by staked amount descending, paginated.
    /// Ties are broken by registration order (ascending index).
    fn get_active_profiles_by_stake_page(env: Env, offset: u32, limit: u32) -> ContractProfilePage;

    /// Every contract registered by `owner`, **including** deactivated ones.
    /// Deprecated in favour of `get_contracts_by_owner_after`.
    fn get_contracts_by_owner(
        env: Env,
        owner: Address,
        offset: u32,
        limit: u32,
    ) -> Vec<ContractEntry>;

    /// Cursor form of `get_contracts_by_owner`, including deactivated entries.
    /// `cursor` is the `contract_id` last returned, or `None` to start.
    fn get_contracts_by_owner_after(
        env: Env,
        owner: Address,
        cursor: Option<Address>,
        limit: u32,
    ) -> Vec<ContractEntry>;

    /// Registrations whose normalised name starts with `prefix`, up to `limit`.
    ///
    /// Matching is case-insensitive: both the stored name and `prefix` are
    /// lowercased before comparison. An unmatched prefix returns an empty
    /// list rather than erroring.
    ///
    /// On-chain prefix matching is deliberately limited to a prefix scan over
    /// the name index; anything richer (substring, fuzzy, ranked) belongs in
    /// the indexer, not in the contract.
    fn find_by_name_prefix(env: Env, prefix: String, limit: u32) -> Vec<ContractEntry>;
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
/// The registry has not been initialized.
NotInitialized = 3,
/// No registration exists for the given address.
    ContractNotFound = 4,
/// The registration is already present.
    AlreadyRegistered = 5,
    /// The caller is not the registration's owner.
    NotOwner = 6,
    /// The registration has been deactivated.
    Deactivated = 7,
    /// Staking has not been configured by governance.
    StakingNotConfigured = 8,
    /// The stake amount is not positive.
    InvalidStake = 9,
    /// The staker has insufficient balance to cover the stake.
    InsufficientBalance = 10,
    /// The staker has nothing to withdraw.
    Nostake = 11,
    /// The proposal ID does not exist.
    ProposalNotFound = 12,
    /// The proposal has already been executed.
    ProposalExecuted = 13,
    /// The proposal has expired.
    ProposalExpired = 14,
    /// The caller has already approved the proposal.
    AlreadyApproved = 15,
    /// The category list was empty.
    NoCategories = 16,
    /// Too many categories were supplied.
    TooManyCategories = 17,
    /// The category is not a recognized value.
    InvalidCategory = 18,
    /// The tag is not a valid length or character set.
    InvalidTag = 19,
    /// Too many tags were supplied.
    TooManyTags = 20,
    /// The admin set would be empty.
    NoAdmins = 21,
    /// The threshold is not positive or exceeds the admin count.
    InvalidThreshold = 22,
    /// The address is not a valid contract address.
    InvalidContractId = 23,
    /// The metadata field is out of bounds.
    InvalidMetadata = 24,
    /// The caller is not an admin.
    NotAdmin = 25,
    /// The registration fee could not be collected.
    FeePaymentFailed = 26,
    /// The slash amount is not positive.
    InvalidSlash = 27,
    /// The slash exceeds the total staked balance.
    SlashExceedsStake = 28,
    /// The registration is not verified.
    NotVerified = 29,
    /// The registration is already verified.
    AlreadyVerified = 30,
    /// The address is already in the admin set.
    AdminExists = 31,
    /// The address is not in the admin set.
    AdminNotFound = 32,
    /// The admin set would fall below the threshold.
    ThresholdNotMet = 33,
    /// The proposal kind is not recognized.
    InvalidProposalKind = 34,
    /// The proposal payload is malformed.
    InvalidPayload = 35,
    /// The argument is out of the accepted range.
    InvalidArgument = 36,
    /// The contract has not been initialized.
    Uninitialized = 37,
}

/// A single registration as the registry stores it.
///
/// Duplicated from `lumina-registry` for the reasons in the crate docs.
///
/// The maximum number of categories a single registration may claim.
///
/// The [`Category`] vocabulary is the natural upper bound, but relying on its
/// size means the limit silently changes every time a category is added. This
/// constant makes the cap explicit and independent of the enum's growth.
///
/// A registration that claims every category is not categorised in any useful
/// sense — it is spam in a discovery surface. Claiming more than this cap is
/// rejected with [`RegistryError::TooManyCategories`] rather than silently
/// truncated.
pub const MAX_CATEGORIES_PER_CONTRACT: u32 = 5;

/// A governance proposal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractEntry {
    /// The registered contract address.
    pub contract_id: Address,
    /// The address that registered it.
    pub owner: Address,
    /// Human-readable name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Project URL.
    pub url: String,
    /// Whether the registration is currently active.
    pub active: bool,
/// Address the owner delegated registration management to, if any.
    pub manager: Option<Address>,
    /// Ledger timestamp of registration.
    pub registered_at: u64,
}

/// The kind of action a proposal carries.
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
    /// Set the staking configuration.
    SetStakingConfig(Address, Address),
    /// Set the registration fee.
    SetRegistrationFee(i128),
    /// Set the slash-specific approval threshold.  Zero means "use the
    /// standard threshold".
    SetSlashThreshold(u32),
}

/// A governance proposal, with the rationale its proposer attached.
///
/// Byte-compatible with `lumina_registry::Proposal`.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[member]
pub enum Category {
    /// Defi protocols.
    Dei = 1,
    /// Non-fungible tokens.
    Nft = 2,
    /// Infrastructure and tooling.
    Infrastructure = 3,
    /// Gaming.
    Gaming = 4,
    /// Social and community.
    Social = 5,
    /// Other.
    Other = 6,
}

/// The reputation signal for a registration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reputation {
    /// Whether governance has attested the registration.
    pub verified: bool,
    /// Total currently staked balance.
    pub staked: i128,
    /// Number of distinct stakers.
    pub staker_count: u32,
    /// Total amount ever slashed.
    pub slashed: i128,
    /// Number of slashes levied.
    pub slash_count: u32,
}
}

/// Maximum length, in bytes, of a proposal description.
///
/// Enforced at creation; a longer description is rejected with
/// `InvalidDescription`. Documented here so clients can validate before
/// submitting a transaction.
pub const MAX_PROPOSAL_DESCRIPTION_LEN: u32 = 256;

/// A slash record, kept for auditability.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlashRecord {
    /// The amount slashed.
    pub amount: i128,
    /// Ledger timestamp of the slash.
    pub timestamp: u64,
    /// Free-text reason.
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

/// A governance proposal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reputation {
    /// Reputation score.
    pub score: i64,
    /// Number of slashes levied.
    pub slashes_count: u32,
    /// Total amount slashed.
    pub total_slashed: i128,
    /// Whether governance has attested it.
    pub verified: bool,
    /// Lifetime total slashed, which unlike `stake` never goes down.
    pub slashed_total: i128,
    /// Ledger before which `withdraw_stake` is refused. Zero once clear.
    pub withdraw_locked_until: u32,
    /// Ledger at which an in-progress unbonding completes, or zero if none is
    /// in progress. Distinct from `withdraw_locked_until`, which is the
    /// post-slash lock.
    pub unbonding_completes_at: u32,
    /// Whether the registration is currently withdrawal-locked.
    pub withdraw_locked: bool,
}
}

/// Aggregate registry counters.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractProfile {
    /// The underlying registration.
    pub entry: ContractEntry,
    /// The reputation signal.
    pub reputation: Reputation,
    /// The contract that supersedes this one, if the owner has set one.
    pub superseded_by: Option<Address>,
    /// Optional URI pointing at richer off-chain metadata.
    pub metadata_uri: Option<String>,
    /// The Unix timestamp of when the contract was registered, or 0 if legacy.
    pub registered_at_ts: u64,
}
}

/// The reputation signal for a registration.
///
/// Duplicated from `lumina-registry`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractPage {
    /// The entries in this page.
    pub entries: Vec<ContractEntry>,
    /// Whether more entries follow.
    pub has_more: bool,
}
}

/// Aggregate registry counters.
///
/// Duplicated from `lumina-registry`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractProfilePage {
    /// The profiles in this page.
    pub entries: Vec<ContractProfile>,
    /// Whether more entries follow.
    pub has_more: bool,
}

/// A registration joined with its reputation.
///
/// Duplicated from `lumina-registry`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractProfile {
    /// The registration itself.
    pub entry: ContractEntry,
    /// The registration's reputation.
    pub reputation: Reputation,
}

/// A page of registration entries.
///
/// Duplicated from `lumina-registry`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractPage {
    /// The entries in this page.
    pub entries: Vec<ContractEntry>,
    /// Whether more entries follow.
    pub has_more: bool,
}

/// A page of profiles with a more-flag.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractProfilePage {
    /// The profiles in this page.
    pub profiles: Vec<ContractProfile>,
    /// Whether more entries follow.
    pub has_more: bool,
}

/// Number of ledgers a proposal of a given action must wait before execution.
pub const TIMELOCK_LEDGERS_UPGRADE: u32 = 17_280;
/// Number of ledgers a proposal of a given action must wait before execution.
pub const TIMELOCK_LEDGERS_ADMIN: u32 = 17_280;
/// Number of ledgers a proposal of a given action must wait before execution.
pub const TIMELOCK_LEDGERS_STANDARD: u32 = 720;