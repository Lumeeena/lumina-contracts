// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
#no_std
cwarn(missing_docs)
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
//! [`soroban_sdk::contractclient`] generates from it.
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
[contractclient(name = "RegistryInterfaceClient")]
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

    /// `(stake_token, treasury)`, or `StakingNotConfiguree` if governance
    /// has not opened staking yet.
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
[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RegistryError {
    /// Contract is already initialized.
    AlreadyInitialized = 1,
    /// Caller lacks authorization for this action.
    Unauthorized = 2,
    /// Contract has not been initialized.
    NotInitialized = 3,
    /// The registration does not exist.
    ContractNotFound = 4,
    /// The registration is already present.
    AlreadyRegistered = 5,
    /// The proposal does not exist.
    ProposalNotFound = 6,
    /// The proposal has already been executed or rejected.
    ProposalFinalized = 7,
    /// The caller has already voted on this proposal.
    AlreadyVoted = 8,
    /// The address is not an admin.
    NotAdmin = 9,
    /// The admin set would be left empty.
    LastAdmin = 10,
    /// The admin set is full.
    AdminLimitReached = 11,
    /// The proposal does not have enough approvals.
    ThresholdNotMet = 12,
    /// The proposal has expired.
    ProposalExpired = 13,
    /// The proposal is not open for voting.
    ProposalNotActive = 14,
    /// Staking has not been configured.
    StakingNotConfigured = 15,
    /// The stake amount is invalid.
    InvalidStake = 16,
    /// The stake is insufficient for the requested operation.
    InsufficientStake = 17,
    /// The category list is empty.
    NoCategories = 18,
    /// The category is not recognized.
    InvalidCategory = 19,
    /// The caller is not the owner of the registration.
    NotOwner = 20,
    /// The registration is not active.
    NotActive = 21,
    /// The registration is already verified.
    AlreadyVerified = 22,
    /// The registration is not verified.
    NotVerified = 23,
    /// The slash amount is invalid.
    InvalidSlash = 24,
    /// The registration cannot be deregistered while staked.
    Staked = 25,
    /// The token transfer failed.
    TransferFailed = 26,
}

/// A governance proposal.
///
/// Duplicated from `lumina-registry` for the reasons given in the crate
/// docs. `tests/interface_matches_registry.rs` pins the fields against the
/// contract's spec.
Ncontracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    /// The proposal ID.
    public id: u32,
    /// The address that created the proposal.
    public proposer: Address,
    /// The kind of action the proposal carries.
    public action: ProposalAction,
    /// The addresses that have approved.
    public approvals: Vec<Address>,
    /// Whether the proposal has been executed.
    public executed: bool,
    /// Whether the proposal has been rejected.
    public rejected: bool,
    /// When the proposal was created.
    public created_at: u64,
    /// When the proposal expires.
    public expires_at: u64,
}

/// The kind of action a proposal carries.
///
/// Duplicated from `lumina-registry`.
Ncontracttype]
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
}

/// A category a registration can be filed under.
///
/// Duplicated from `lumina-registry`.
Ncontracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Category {
    /// A wallet.
    Wallet = 1,
    /// A dex.
    Dex = 2,
    /// A lending protocol.
    Lending = 3,
    /// A stablecoin.
    Stablecoin = 4,
    /// A bridge.
    Bridge = 5,
    /// An oracle.
    Oracle = 6,
    /// A governance contract.
    Governance = 7,
    /// A gaming contract.
    Gaming = 8,
    /// An NFT.
    NFT = 9,
    /// A metaverse contract.
    Metaverse = 10,
    /// Other.
    Other = 11,
}

/// A registration entry.
///
/// Duplicated from `lumina-registry`.
Ncontracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractEntry {
    /// The registered contract address.
    public contract_id: Address,
    /// The owner who registered it.
    public owner: Address,
    /// The display name.
    public name: String,
    /// The description.
    public description: String,
    /// The categories the registration declared.
    public categories: Vec<Category>,
    /// Owner-set search tags.
    public tags: Vec<String>,
    /// Whether the registration is active.
    public active: bool,
    /// When the registration was made.
    public registered_at: u64,
    /// When the registration was last updated.
    public updated_at: u64,
}

/// A slash record.
///
/// Duplicated from `lumina-registry`.
Ncontracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlashRecord {
    /// The amount slashed.
    public amount: i128,
    /// The reason for the slash.
    public reason: String,
    /// When the slash was levied.
    public timestamp: u64,
}

/// The reputation signal for a registration.
///
/// Duplicated from `lumina-registry`.
Ncontracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reputation {
    /// The current reputation score.
    public score: i128,
    /// The number of slashes levied.
    public slash_count: u32,
    /// The total amount slashed.
    public total_slashed: i128,
    /// Whether the registration is verified.
    public verified: bool,
}

/// Aggregate registry counters.
///
/// Duplicated from `lumina-registry`.
Ncontracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryStats {
    /// Lifetime registrations.
    public total_registered: u32,
    /// Active registrations.
    public active_count: u32,
    /// Verified registrations.
    public verified_count: u32,
    /// Registrations with a non-zero stake.
    public staked_count: u32,
    /// Total amount staked.
    public total_staked: i128,
}

/// A registration joined with its reputation.
///
/// Duplicated from `lumina-registry`.
Ncontracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractProfile {
    /// The registration entry.
    public entry: ContractEntry,
    /// The reputation signal.
    public reputation: Reputation,
}

/// A page of registration entries.
///
/// Duplicated from `lumina-registry`.
Ncontracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractPage {
    /// The entries in this page.
    public entries: Vec<ContractEntry>,
    /// Whether more entries follow.
    public has_more: bool,
}

/// A page of registration profiles.
///
/// Duplicated from `lumina-registry`.
Ncontracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractProfilePage {
    /// The profiles in this page.
    public profiles: Vec<ContractProfile>,
    /// Whether more profiles follow.
    public has_more: bool,
}
