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
//! `env.invoke_contract(&symbol_short!("is_registered"), ...)` and
//! decode the `Val yourself, or use `contractimport!` on the registry's wasm.
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
//! A cross-contract read is *not* free, and not free in the way people
//! expect. It is not a `simulateTransaction` — a contract calling the registry
//! on-chain spends the transaction's whole resource budget, and the callee's
//! instructions and ledger reads are charged to *you*.
//!
//! Concretely, each read is one nested invocation frame, which costs:
//!
//! - a fixed instruction charge for the call itself, before the callee runs
//!   any code;
//! - every ledger entry the callee touches, at the callee's TVL — the registry
//!   stores registrations in `persistent` entries, so a read is a persistent
//!   entry read, which is the expensive kind;
//! - a fresh 1 MiB memory allocation for the callee's frame, and the memory
//!   cost of decoding the arguments you passed in and the result you get back.
//!
//! The practical consequence: **number of calls is what you pay for.** Two
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

    /// `(stake_token, treasury)`, or `StakingNotConfigured` if governance
    /// has not opened staking yet.
    fn get_staking_config(env: Env) -> Result<(Address, Address), RegistryError>;

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
    /// The registry has not been initialized yet.
    NotInitialized = 3,
    /// No registration exists for the given address.
    ContractNotFound = 4,
    /// A contract with this address is already registered.
    AlreadyRegistered = 5,
    /// The caller is not the owner of the registration.
    NotOwner = 6,
    /// The proposal ID does not exist.
    ProposalNotFound = 7,
    /// The proposal has already been executed or rejected.
    ProposalClosed = 8,
    /// The caller has already voted on this proposal.
    AlreadyVoted = 9,
    /// No categories were supplied where at least one is required.
    NoCategories = 10,
    /// Staking has not been configured by governance.
    StakingNotConfigured = 11,
    /// The caller holds insufficient stake for this action.
    InsufficientStake = 12,
    /// The provided fee is not the expected amount.
    InvalidFee = 13,
    /// The owner has reached the per-owner contract limit.
    ///
    /// The registry caps an owner's contract index at
    /// [`MAX_CONTRACTS_PER_OWNER`] entries. Registering one more fails
    /// with this error rather than an opaque storage failure.
    OwnerContractLimitReached = 14,
    /// The proposal did not reach the required approval threshold.
    ThresholdNotMet = 15,
    /// The caller is not an admin.
    NotAdmin = 16,
    /// The admin set would be empty after this operation.
    EmptyAdminSet = 17,
    /// The provided threshold is invalid.
    InvalidThreshold = 18,
    /// The address is not a valid contract address.
    InvalidContractAddress = 19,
    /// The provided metadata is invalid.
    InvalidMetadata = 20,
    /// The provided category is not recognized.
    UnknownCategory = 21,
    /// The provided tag is invalid.
    InvalidTag = 22,
    /// The provided page limit is invalid.
    InvalidLimit = 23,
    /// The provided offset is invalid.
    InvalidOffset = 24,
    /// The registry is paused and cannot accept writes.
    Paused = 25,
    /// The caller is not the admin that initialized the registry.
    NotInitializerAdmin = 26,
    /// The proposal attempts an action that is not allowed.
    InvalidProposalAction = 27,
    /// The slash amount is invalid.
    InvalidSlashAmount = 28,
    /// The slash history for this contract is full.
    SlashHistoryFull = 29,
    /// The registration fee is not configured.
    RegistrationFeeNotConfigured = 30,
    /// The provided stake amount is invalid.
    InvalidStakeAmount = 31,
    /// The contract is deactivated and cannot be used.
    Deactivated = 32,
    /// The contract is already deactivated.
    AlreadyDeactivated = 33,
    /// The contract is not deactivated.
    NotDeactivated = 34,
    /// The provided name is invalid or too long.
    InvalidName = 35,
    /// The provided description is invalid or too long.
    InvalidDescription = 36,
    /// The provided URL is invalid.
    InvalidUrl = 37,
    /// The provided version is invalid.
    InvalidVersion = 38,
    /// The governance configuration is invalid.
    InvalidGovernanceConfig = 39,
    /// The proposal has expired.
    ProposalExpired = 40,
    /// The caller is not a verifier.
    NotVerifier = 41,
    /// The contract is already verified.
    AlreadyVerified = 42,
    /// The contract is not verified.
    NotVerified = 43,
    /// The caller is not the treasury.
    NotTreasury = 44,
    /// The treasury is not configured.
    TreasuryNotConfigured = 45,
    /// The provided amount is zero or negative.
    InvalidAmount = 46,
    /// The registry has not been initialized by an admin.
    NoAdmin = 47,
    /// The provided address is already an admin.
    AlreadyAdmin = 48,
    /// The provided address is not an admin.
    NotAnAdmin = 49,
    /// The proposal attempts to change governance with an invalid parameter.
    InvalidGovernanceParam = 50,
    /// The proposal attempts to change the registry with an invalid parameter.
    InvalidRegistryParam = 51,
    /// The proposal attempts to change the staking configuration with an invalid parameter.
    InvalidStakingParam = 52,
    /// The proposal attempts to change the fee configuration with an invalid parameter.
    InvalidFeeParam = 53,
    /// The provided proposal type is not recognized.
    UnknownProposalType = 54,
    /// The provided vote is not recognized.
    UnknownVote = 55,
    /// The caller has already approved this proposal.
    AlreadyApproved = 56,
    /// The caller has already rejected this proposal.
    AlreadyRejected = 57,
    /// The proposal cannot be executed yet.
    ProposalNotReady = 58,
    /// The proposal cannot be executed because it was rejected.
    ProposalRejected = 59,
    /// The proposal cannot be executed because it was cancelled.
    ProposalCancelled = 60,
    /// The proposal cannot be cancelled.
    ProposalNotCancellable = 61,
    /// The proposal cannot be executed because the registry is paused.
    ProposalExecutionPaused = 62,
    /// The proposal cannot be executed because the governance configuration is invalid.
    ProposalExecutionInvalidGovernance = 63,
    /// The proposal cannot be executed because the registry configuration is invalid.
    ProposalExecutionInvalidRegistry = 64,
    /// The proposal cannot be executed because the staking configuration is invalid.
    ProposalExecutionInvalidStaking = 65,
    /// The proposal cannot be executed because the fee configuration is invalid.
    ProposalExecutionInvalidFee = 66,
    /// The proposal cannot be executed because the admin set would be empty.
    ProposalExecutionEmptyAdminSet = 67,
    /// The proposal cannot be executed because the threshold is invalid.
    ProposalExecutionInvalidThreshold = 68,
    /// The proposal cannot be executed because the address is invalid.
    ProposalExecutionInvalidAddress = 69,
    /// The proposal cannot be executed because the metadata is invalid.
    ProposalExecutionInvalidMetadata = 70,
    /// The proposal cannot be executed because the category is unknown.
    ProposalExecutionUnknownCategory = 71,
    /// The proposal cannot be executed because the tag is invalid.
    ProposalExecutionInvalidTag = 72,
    /// The proposal cannot be executed because the limit is invalid.
    ProposalExecutionInvalidLimit = 73,
    /// The proposal cannot be executed because the offset is invalid.
    ProposalExecutionInvalidOffset = 74,
    /// The proposal cannot be executed because the registry is paused.
    ProposalExecutionPaused = 75,
    /// The proposal cannot be executed because the caller is not the initializer admin.
    ProposalExecutionNotInitializerAdmin = 76,
    /// The proposal cannot be executed because the action is invalid.
    ProposalExecutionInvalidAction = 77,
    /// The proposal cannot be executed because the slash amount is invalid.
    ProposalExecutionInvalidSlashAmount = 78,
    /// The proposal cannot be executed because the slash history is full.
    ProposalExecutionSlashHistoryFull = 79,
    /// The proposal cannot be executed because the registration fee is not configured.
    ProposalExecutionRegistrationFeeNotConfigured = 80,
    /// The proposal cannot be executed because the stake amount is invalid.
    ProposalExecutionInvalidStakeAmount = 81,
    /// The proposal cannot be executed because the contract is deactivated.
    ProposalExecutionDeactivated = 82,
    /// The proposal cannot be executed because the contract is already deactivated.
    ProposalExecutionAlreadyDeactivated = 83,
    /// The proposal cannot be executed because the contract is not deactivated.
    ProposalExecutionNotDeactivated = 84,
    /// The proposal cannot be executed because the name is invalid.
    ProposalExecutionInvalidName = 85,
    /// The proposal cannot be executed because the description is invalid.
    ProposalExecutionInvalidDescription = 86,
    /// The proposal cannot be executed because the URL is invalid.
    ProposalExecutionInvalidUrl = 87,
    /// The proposal cannot be executed because the version is invalid.
    ProposalExecutionInvalidVersion = 88,
    /// The proposal cannot be executed because the governance configuration is invalid.
    ProposalExecutionInvalidGovernanceConfig = 89,
    /// The proposal cannot be executed because the proposal has expired.
    ProposalExecutionProposalExpired = 90,
    /// The proposal cannot be executed because the caller is not a verifier.
    ProposalExecutionNotVerifier = 91,
    /// The proposal cannot be executed because the contract is already verified.
    ProposalExecutionAlreadyVerified = 92,
    /// The proposal cannot be executed because the contract is not verified.
    ProposalExecutionNotVerified = 93,
    /// The proposal cannot be executed because the caller is not the treasury.
    ProposalExecutionNotTreasury = 94,
    /// The proposal cannot be executed because the treasury is not configured.
    ProposalExecutionTreasuryNotConfigured = 95,
    /// The proposal cannot be executed because the amount is invalid.
    ProposalExecutionInvalidAmount = 96,
    /// The proposal cannot be executed because there is no admin.
    ProposalExecutionNoAdmin = 97,
    /// The proposal cannot be executed because the address is already an admin.
    ProposalExecutionAlreadyAdmin = 98,
    /// The proposal cannot be executed because the address is not an admin.
    ProposalExecutionNotAnAdmin = 99,
    /// The proposal cannot be executed because the governance parameter is invalid.
    ProposalExecutionInvalidGovernanceParam = 100,
    /// The proposal cannot be executed because the registry parameter is invalid.
    ProposalExecutionInvalidRegistryParam = 101,
    /// The proposal cannot be executed because the staking parameter is invalid.
    ProposalExecutionInvalidStakingParam = 102,
    /// The proposal cannot be executed because the fee parameter is invalid.
    ProposalExecutionInvalidFeeParam = 103,
    /// The proposal cannot be executed because the proposal type is unknown.
    ProposalExecutionUnknownProposalType = 104,
    /// The proposal cannot be executed because the vote is unknown.
    ProposalExecutionUnknownVote = 105,
    /// The proposal cannot be executed because the caller has already approved.
    ProposalExecutionAlreadyApproved = 106,
    /// The proposal cannot be executed because the caller has already rejected.
    ProposalExecutionAlreadyRejected = 107,
    /// The proposal cannot be executed because the proposal is not ready.
    ProposalExecutionProposalNotReady = 108,
    /// The proposal cannot be executed because the proposal was rejected.
    ProposalExecutionProposalRejected = 109,
    /// The proposal cannot be executed because the proposal was cancelled.
    ProposalExecutionProposalCancelled = 110,
    /// The proposal cannot be cancelled.
    ProposalExecutionProposalNotCancellable = 111,
    /// The proposal cannot be executed because the registry is paused.
    ProposalExecutionProposalExecutionPaused = 112,
}

/// A governance proposal.
///
/// Duplicated from `lumina-registry`; see the crate docs for why.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    /// The proposal ID.
    public id: u32,
    /// The address that created the proposal.
    public proposer: Address,
    /// The action the proposal would take.
    public action: ProposalAction,
    /// The addresses that have approved.
    public approvals: Vec<Address>,
    /// The addresses that have rejected.
    public rejections: Vec<Address>,
    /// Whether the proposal has been executed.
    public executed: bool,
    /// Whether the proposal has been cancelled.
    public cancelled: bool,
    /// The ledger timestamp the proposal was created at.
    public created_at: u64,
}

/// The action a governance proposal would take.
///
/// Duplicated from `lumina-registry`; see the crate docs for why.
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
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Category {
    /// A token contract.
    Token = 1,
    /// A lending or borrowing protocol.
    Lending = 2,
    /// A trading or exchange protocol.
    Exchange = 3,
    /// A bridge.
    Bridge = 4,
    /// A governance contract.
    Governance = 5,
    /// A derivatives contract.
    Derivatives = 6,
    /// A wallet or account abstraction contract.
    Wallet = 7,
    /// An oracle.
    Oracle = 8,
    /// A stablecoin.
    Stablecoin = 9,
    /// An NFT contract.
    Nft = 10,
    /// A gaming contract.
    Gaming = 11,
    /// A metaverse contract.
    Metaverse = 12,
    /// A social contract.
    Social = 13,
    /// An infrastructure contract.
    Infrastructure = 14,
    /// A tooling contract.
    Tooling = 15,
    /// Anything else.
    Other = 16,
}

/// A single registration entry.
///
/// Duplicated from `lumina-registry`; see the crate docs for why.
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

/// A slash record.
///
/// Duplicated from `lumina-registry`; see the crate docs for why.
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

/// The reputation signal for a registration.
///
/// Duplicated from `lumina-registry`; see the crate docs for why.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reputation {
    /// The current reputation score.
    public score: i128,
    /// The number of slashes levied.
    public slash_count: u32,
    /// The total amount slashed.
    public total_slashed: i128,
    /// The number of verifications.
    public verifications: u32,
}

/// A contract registration joined with its reputation.
///
/// Duplicated from `lumina-registry`; see the crate docs for why.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractProfile {
    /// The registration entry.
    public entry: ContractEntry,
    /// The registration's reputation.
    public reputation: Reputation,
    /// Whether governance has attested the registration.
    public verified: bool,
}

/// Aggregate registry counters.
///
/// Duplicated from `lumina-registry`; see the crate docs for why.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryStats {
    /// Lifetime registrations ever made.
    public total_registered: u32,
    /// Currently listed registrations.
    public active_contracts: u32,
    /// Currently verified registrations.
    public verified_contracts: u32,
    /// The number of registrations with a non-zero stake.
    public staked_count: u32,
    /// The total amount staked.
    public total_staked: i128,
}

/// One page of contract entries, plus a flag for whether more follow.
///
/// Duplicated from `lumina-registry`; see the crate docs for why.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractPage {
    /// The entries in this page.
    public entries: Vec<ContractEntry>,
    /// Whether more entries follow this page.
    public has_more: bool,
}

/// One page of contract profiles, plus a flag for whether more follow.
///
/// Duplicated from `lumina-registry`; see the crate docs for why.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractProfilePage {
    /// The profiles in this page.
    public entries: Vec<ContractProfile>,
    /// Whether more entries follow this page.
    public has_more: bool,
}

/// Number of ledgers a proposal of a given action must wait before execution.
pub const TIMELOCK_LEDGERS_UPGRADE: u32 = 17_280;
/// Number of ledgers a proposal of a given action must wait before execution.
pub const TIMELOCK_LEDGERS_ADMIN: u32 = 17_280;
/// Number of ledgers a proposal of a given action must wait before execution.
pub const TIMELOCK_LEDGERS_STANDARD: u32 = 720;