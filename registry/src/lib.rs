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
//!
//! ## Resource cost benchmarks
//!
//! Soroban meters execution: an entrypoint that grows past a resource limit
//! simply stops working on-chain while passing every test in the local host.
//! The scanning views (`get_active_contracts`, `get_contracts_by_category`,
//! `get_contracts_by_tag`) are the obvious candidates because their cost grows
//! with the size of the registry index.
//!
//! `registry/tests/bench.rs` uses the test host's budget instrumentation to
//! record CPU instructions and memory per entrypoint and asserts a ceiling for
//! the scanning views. The numbers below are the ceilings asserted in CI; a
//! significant regression fails the build.
//!
//! | Entrypoint | Metric | Ceiling |
//! |------------|--------|---------|
//! | `register_contract` | CPU instructions | 5_000_000 |
//! | `get_active_contracts` | CPU instructions | 20_000_000 |
//! | `get_contracts_by_category` | CPU instructions | 20_000_000 |
//! | `get_contracts_by_tag` | CPU instructions | 20_000_000 |
//! | `get_active_contracts` | Memory bytes | 1_000_000 |
//!
//! When a ceiling is intentionally raised, update this table in the same
//! commit so the regression stays visible in review.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, token, Address, BytesN, Env, String,
    Symbol, Vec,
};

// ─── Version ───────────────────────────────────────────────────────────────

/// Version of the deployed code, returned by [`LuminaRegistry::get_version`].
///
/// Bump this in the same commit as any change to the exported interface or to
/// the storage shapes below.
pub const CONTRACT_VERSION: u32 = 7;

/// Maximum number of addresses stored per chunk in the global registration index.
/// Chunks live in persistent storage to avoid the instance-storage ceiling that
/// made the old single `Vec<Address>` in `DataKey::AllContracts` scale poorly.
pub const ALL_CONTRACTS_PAGE_SIZE: u32 = 64;

/// Minimum number of admins required for multi-sig governance.
pub const MIN_ADMINS: u32 = 2;

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
///
/// ## Error code reference
///
/// Error codes cross the contract boundary as bare `u32` values, so this
/// table is the authoritative documentation for a consumer that only has a
/// numeric code in hand. It is kept next to the enum so the two are updated
/// together; if you add or renumber a variant, update this table in the same
/// commit.
///
/// | Code | Name | Meaning | Usual remedy |
/// |------|------|---------|--------------|
/// | 1 | `AlreadyInitialized` | `initialize` was called on a deployment that already has an admin set. | Do not call `initialize` again; read `get_admins` / `get_threshold` to inspect the existing configuration. |
/// | 2 | `Unauthorized` | The caller is not the registered owner and not permitted to perform this action. | Call from the registered owner's address, or route the action through the governance flow (`propose_*` → `approve_proposal` → `execute_proposal`). |
/// | 3 | `AlreadyRegistered` | A `Contract` entry already exists for this `contract_id`. | Use `update_metadata` / `set_categories` to change the existing entry, or `deregister` it first if you intend to re-register. |
/// | 4 | `ContractNotFound` | No `Contract` entry exists for the given `contract_id`. | Check `is_registered` before calling; register the contract first with `register_contract`. |
/// | 5 | `InvalidMetadata` | The supplied metadata failed validation (e.g. empty batch, batch larger than 100 entries). | Pass a non-empty batch of at most 100 entries and ensure each entry has a name and description. |
/// | 6 | `NotOwner` | The caller is not the `owner` recorded on the registration. | Call from the recorded owner's address, or have the current owner call `transfer_ownership` first. |
/// | 7 | `NotInitialized` | The registry has no admin set because `initialize` was never called. | Deploy with the `__constructor` bootstrap admin, or call `initialize` once with a non-empty admin set. |
/// | 8 | `ProposalNotFound` | No proposal exists for the given `proposal_id`. | Read `get_proposal` for a valid ID; IDs are assigned sequentially starting at 0. |
/// | 9 | `ThresholdNotMet` | The proposal has not collected enough approvals, or has not yet become ready. | Have additional admins call `approve_proposal` until `approvals.len()` reaches `get_threshold`. |
/// | 10 | `TimelockNotElapsed` | Fewer than `TIMELOCK_LEDGERS` ledgers have passed since the proposal became ready. | Wait until `ready_at + TIMELOCK_LEDGERS` and retry `execute_proposal`. |
/// | 11 | `AlreadyApproved` | This admin address has already approved this proposal. | Do not re-approve; have a different admin approve instead. |
/// | 12 | `NotAdmin` | The caller is not a member of the current admin set. | Call from an address returned by `get_admins`, or propose adding the caller via `propose_add_admin`. |
/// | 13 | `InvalidThreshold` | The admin set would be empty, or the threshold is zero or exceeds the set size. | Pass a non-empty admin set with `1 <= threshold <= admins.len()`. |
/// | 14 | `AlreadyExecuted` | The proposal has already been executed. | Do not retry; create a new proposal if further action is needed. |
/// | 15 | `StakingNotConfigured` | No stake token / treasury has been set, so staking is not open. | Have governance pass `propose_configure_staking` and execute it before staking. |
/// | 16 | `InvalidAmount` | A stake, slash, or fee amount was zero or negative. | Pass a strictly positive amount for `stake` / `propose_slash`, and a non-negative fee for `propose_set_registration_fee`. |
/// | 17 | `InsufficientStake` | The registration's staked balance is smaller than the requested amount. | Stake more first with `stake`, or reduce the requested amount to at most `get_stake`. |
/// | 18 | `StakeLocked` | The stake is still inside the post-slash lock window. | Wait until `get_reputation(...).withdraw_locked_until` and retry `withdraw_stake`. |
/// | 19 | `RegistrationActive` | The registration is still active, so it cannot be withdrawn or deregistered. | Call `deactivate` first, then retry `withdraw_stake` or `deregister`. |
/// | 20 | `NoCategories` | A registration or category query declared no categories. | Pass at least one `Category` (use `Category::Other` if none of the vocabulary fits). |
/// | 21 | `StakeNotEmpty` | The registration still holds stake, so it cannot be deregistered. | Call `withdraw_stake` until `get_stake` returns zero, then retry `deregister`. |
/// | 22 | `InvalidRateLimit` | The rate limit configuration is invalid (zero window with a non-zero limit, or a window larger than `max_ttl`). | Pass `window_ledgers` in `1..=max_ttl` when `limit > 0`, or set `limit = 0` to disable limiting. |
/// | 23 | `NotAllowlisted` | The owner is not allowlisted while permissioned registration is enabled. | Have governance execute `propose_set_allowlisted(owner, true)`, or disable the allowlist with `propose_set_allowlist_enabled(false)`. |
/// | 24 | `RegistrationRateLimited` | The per-owner registration rate limit has been exceeded for the current window. | Wait for the current window to elapse, or have governance raise the limit via `propose_configure_registration_rate_limit`. |
/// | 25 | `InsufficientFee` | The registration fee was not paid. | Ensure the owner holds at least `get_registration_fee()` of the stake token and approves the transfer before registering. |
/// | 26 | `InvalidTags` | The tag count exceeds 10, or a tag is longer than 16 characters. | Pass at most 10 tags, each at most 16 characters long. |
/// | 27 | `InvalidAttestation` | Attestation label is empty, too long, or the registration already has the maximum number of attestations. | Pass a non-empty label of at most `MAX_ATTESTATION_LABEL_LEN` bytes, or revoke an existing attestation first. |
/// | 28 | `AttestationNotFound` | The caller has no attestation to revoke on this registration. | Only the attester themselves can revoke; check `get_attestations` for the caller's address. |
/// | 29 | `NotManager` | The caller is neither the registered owner nor the owner-appointed manager. | Call from the owner's address, or have the owner appoint the caller via `set_manager`. |
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
    /// The proposed treasury or stake-token address is itself a registered
    /// contract.
    OverlappingAddress = 29,
    /// The admin set would have fewer than `MIN_ADMINS` members.
    AdminSetTooSmall = 30,
    /// The proposed address is already a member of the admin set.
    AlreadyAdmin = 31,
    /// The proposed address to remove is not a member of the admin set.
    AdminNotFound = 32,
    /// The proposed threshold is already the current threshold.
    ThresholdAlreadySet = 33,
    /// The proposed verification status matches the contract's current status.
    AlreadyVerified = 34,
    /// Staking is already configured with the proposed token and treasury.
    StakingAlreadyConfigured = 35,readyConfigured = 35,
 main
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
    /// Owner's optional response to the slash.
    pub response: Option<String>,
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
    /// The contract that supersedes this one, if the owner has set one.
    pub superseded_by: Option<Address>,
}

/// Paginated result of contract entries with pagination info.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractPage {
    /// The contracts in this page.
    pub entries: Vec<ContractEntry>,
    /// True if more results are available after this page.
    pub has_more: bool,
}

/// Paginated result of contract profiles with pagination info.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractProfilePage {
    /// The profiles in this page.
    pub entries: Vec<ContractProfile>,
    /// True if more results are available after this page.
    pub has_more: bool,
}

// ─── Third-party attestations ──────────────────────────────────────────────
//
// ## Why this is not `Verified`
//
// `Verified` is the governance signal: only the admin set, through a
// threshold-and-timelocked proposal, can set it, and a registrant cannot
// vouch for themselves. That is exactly the property that makes it worth
// anything, so this feature deliberately does not touch it — there is no
// `attest`-driven path to `Verified` and no counter that aggregates
// attestations into it.
//
// An attestation is a weaker, explicitly *named* claim. "Account X says this
// contract is audited" is a different and more modest statement than
// "the registry vouches for this contract", and conflating them would let
// anyone inflate the verified signal by attaching cheap labels to a
// registration. Keeping them separate means a consumer can weight them
// differently, and can show the attester's address either way.
//
// What an attestation *is* good for is the transparency property: the
// attester's address is recorded on-chain, so a claim cannot be anonymous,
// and the attester can withdraw it themselves. A wrong attestation is
// therefore contestable by the party it misleads, without needing governance
// to act.
//
// Labels are bounded in both count and length (see
// [`MAX_ATTESTATIONS_PER_CONTRACT`] and [`MAX_ATTESTATION_LABEL_LEN`]) so one
// party cannot inflate a registration's state with unbounded storage at a
// cost imposed on every future reader of that list.

/// One third party's claim about a registration.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Attestation {
    /// Who made the attestation. Recorded so the claim is attributable rather
    /// than anonymous, and so the attester can revoke it.
    pub attester: Address,
    /// Short, bounded free-text label describing the basis of the claim
    /// (e.g. "audited", "used in production"). Bounded by
    /// [`MAX_ATTESTATION_LABEL_LEN`].
    pub label: String,
    /// Ledger at which the attestation was made.
    pub created_at: u32,
}

/// Maximum attestations a single registration may accumulate.
///
/// Bounded so the cost of listing a registration's attestations is a property
/// of the contract rather than of how many parties choose to speak up.
pub const MAX_ATTESTATIONS_PER_CONTRACT: u32 = 20;

/// Maximum length of an attestation label, in bytes.
pub const MAX_ATTESTATION_LABEL_LEN: u32 = 64;

/// Upper bound for a registration name. This is small enough for UI cards and
/// large enough for a short human-readable project label without letting a
/// caller force unbounded storage or rendering cost onto every consumer.
pub const MAX_NAME_LEN: u32 = 64;

/// Upper bound for a registration description. The limit is intentionally high
/// enough for a summary while still keeping storage and rendering costs bounded.
pub const MAX_DESCRIPTION_LEN: u32 = 512;

/// Entry for batch registration.
#[contracttype]
#[derive(Clone, Debug)]
pub struct RegistrationEntry {
    /// Contract to register.
    pub contract_id: Address,
    /// Human-readable name.
    pub name: String,
    /// Short description.
    pub description: String,
    /// Categories for browsing.
    pub categories: Vec<Category>,
}

/// Registry statistics aggregating key metrics.
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
    /// Set the registration fee in the stake token; zero disables it.
    SetRegistrationFee(i128),
    /// Set the minimum stake threshold; zero disables it.
    ConfigureMinimumStake(i128),
    /// Withdraw from the treasury.
    WithdrawFromTreasury(i128),
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
///
/// ## Storage keys, storage types and lifetimes
///
/// Every key the contract writes is listed below with the storage it lives in
/// and how long it is expected to survive. This matters operationally because
/// Soroban archives instance and persistent entries independently: an entry
/// whose TTL lapses becomes unloadable, and any index that still names it
/// becomes stale (see `prune_category` / `prune_all_contracts`).
///
/// | Key | Storage | Holds | Lifetime / TTL behaviour |
/// |-----|---------|-------|--------------------------|
/// | `Admins` | instance | `Vec<Address>` — current admin set | Lives as long as the contract instance; refreshed by `initialize` / admin-set proposals. |
/// | `Threshold` | instance | `u32` — approvals required to pass | Same as the instance; changed only by `ChangeThreshold` proposals. |
/// | `ProposalCount` | instance | `u32` — monotonic proposal counter | Never expires while the instance lives; never decremented. |
/// | `ProposalData(u32)` | persistent | `Proposal` — full proposal record | Persistent; survives until its TTL lapses. Never deleted, so executed proposals remain readable. |
/// | `ContractCount` | instance | `u32` — live registrations | Instance lifetime; decremented on `deregister`. |
/// | `TotalRegistered` | instance | `u32` — lifetime registrations | Instance lifetime; never decremented. Missing on pre-existing deployments — `get_total_registered` falls back to `ContractCount`. |
/// | `Contract(Address)` | persistent | `ContractEntry` — registration metadata | Persistent; the authoritative entry. Removed by `deregister`; may be archived by TTL, which is what the prune entrypoints clean up after. |
/// | `OwnerContracts(Address)` | persistent | `Vec<Address>` — per-owner index | Persistent index; grows with registrations. Must stay consistent with `Contract` entries — eager cleanup on `deregister`, pruned via `prune_all_contracts` for archival. |
/// | `AllContracts` | instance | `Vec<Address>` — global insertion-ordered index | Instance lifetime; a single bounded entry. Must stay consistent with `Contract` entries — eager cleanup on `deregister`, `prune_all_contracts` for archival. |
/// | `StakeToken` | instance | `Address` — SEP-41 stake token | Instance lifetime; set once via `ConfigureStaking`. |
/// | `Treasury` | instance | `Address` — slash destination | Instance lifetime; set once via `ConfigureStaking`. |
/// | `Stake(Address)` | persistent | `i128` — staked balance | Persistent; zeroed by `withdraw_stake`, removed by `deregister`. |
/// | `Verified(Address)` | persistent | `bool` — governance-attested status | Persistent; removed by `deregister`. |
/// | `Slashes(Address)` | persistent | `Vec<SlashRecord>` — slash history | Persistent and deliberately kept after `deregister` so penalties stay auditable. |
/// | `WithdrawLockedUntil(Address)` | persistent | `u32` — post-slash lock ledger | Persistent; removed by `deregister`. |
/// | `MinimumStake` | instance | `i128` — minimum stake threshold | Instance lifetime; zero disables it. |
/// | `Categories(Address)` | persistent | `Vec<Category>` — declared categories | Persistent; removed by `deregister`. |
/// | `ByCategory(Category)` | persistent | `Vec<Address>` — per-category index | Persistent index; grows with registrations. Must stay consistent with `Contract` entries — eager cleanup on `deregister` / `set_categories`, `prune_category` for archival. |
/// | `AllowlistEnabled` | instance | `bool` — permissioned registration flag | Instance lifetime; toggled by `SetAllowlistEnabled` proposals. |
/// | `Allowlisted(Address)` | persistent | `bool` — allowlist membership | Persistent; set by `SetAllowlisted` proposals. |
/// | `RegistrationRateLimit` | instance | `u32` — per-owner limit | Instance lifetime; zero disables limiting. |
/// | `RegistrationRateWindow` | instance | `u32` — window size in ledgers | Instance lifetime. |
/// | `RegistrationWindow(Address)` | persistent | `RegistrationWindow` — current window counter | Persistent; rolls over as windows elapse. |
/// | `RegistrationFee` | instance | `i128` — fee in the stake token | Instance lifetime; zero disables it. |
/// | `Tags(Address)` | persistent | `Vec<String>` — owner-set tags | Persistent; removed by `deregister`. |
/// | `Attestations(Address)` | persistent | `Vec<Attestation>` — third-party attestations | Persistent; removed by `deregister` since opinions about a gone registration have nothing to refer to. |
/// | `TotalStaked` | instance | `i128` — total staked across registrations | Instance lifetime; adjusted on `stake` / `withdraw_stake` / `deregister`. |
/// | `VerifiedCount` | instance | `u32` — count of verified registrations | Instance lifetime; adjusted on `SetVerified` / `deregister`. |
/// | `Admin` | instance | `Address` — legacy single-admin key | Instance lifetime; written by `__constructor` / `initialize` and read by `upgrade` and `get_admin` for v1/v2 upgrade compatibility. |
///
/// Indexes that must stay consistent with their entries: `OwnerContracts`,
/// `AllContracts`, and `ByCategory`. Each names `Contract` entries, so a
/// removed or archived entry leaves a dead reference behind until the eager
/// cleanup paths or the prune entrypoints run.
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
    /// Contract(Address) — the stored entry for one registered contract.
    Contract(Address),
    /// Vec<Address> — list of contracts registered by a specific owner.
    OwnerContracts(Address),
    /// Legacy Vec<Address> — retained so old instance-backed deployments can be
    /// migrated to the chunked persistent index on first access.
    AllContracts,
/// u32 — number of entries in the chunked persistent `AllContracts` index.
AllContractsLength,

/// Vec<Address> — one chunk of the insertion-ordered global registration list.
AllContractsPage(u32),

Expiry(Address),

    // ── Staking & reputation ────────────────────────────────────────────────
    /// Address — the SEP-41 token stakes are denominated in.
    StakeToken,
    /// Address — where slashed stake is sent.
    Treasury,
    /// Address — the previous staking token, if any (for reconfiguration tracking).
    PreviousStakeToken,
    /// Address — the previous treasury, if any (for reconfiguration tracking).
    PreviousTreasury,
    /// i128 — currently staked balance for a registration.
    Stake(Address),
    /// bool — governance-attested verified status.
    Verified(Address),
    /// Vec<SlashRecord> — every slash ever levied, oldest first.
    Slashes(Address),
    /// u32 — ledger before which `withdraw_stake` is refused.
    WithdrawLockedUntil(Address),
    /// i128 — minimum stake threshold; zero disables it.
    MinimumStake,

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

    // ── Registration fee ────────────────────────────────────────────────────
    /// i128 — governance-set registration fee in the stake token; zero disables it.
    RegistrationFee,

    // ── Tags ────────────────────────────────────────────────────────────────
    /// Vec<String> — owner-set normalized tags for a registration.
    Tags(Address),
    /// Option<Address> — replacement contract that supersedes this one.
    SupersededBy(Address),

    // ── Succession ──────────────────────────────────────────────────────────
    /// Address — the contract that supersedes this registration, if any.
    SupersededBy(Address),

    // ── Third-party attestations ────────────────────────────────────────────
    /// Vec<Attestation> — third-party attestations on a registration, oldest
    /// first. Kept beside `ContractEntry` rather than inside it, for the same
    /// reason as `Reputation`: a new field on `ContractEntry` would break every
    /// entry already written (see the upgrade-compatibility rules above),
    /// whereas a new `DataKey` variant is safe.
    Attestations(Address),

    // ── Registry statistics ────────────────────────────────────────────────
    /// i128 — total staked across all registrations.
    TotalStaked,
    /// u32 — count of verified registrations.
    VerifiedCount,

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
env.storage()
    .instance()
    .set(&DataKey::Admin, &bootstrap_admin);

env.storage()
    .persistent()
    .set(&DataKey::AllContractsLength, &0u32);
}

/// Read the global registration index from its chunked persistent form,
/// migrating any legacy instance-backed vector the first time it is used.
fn all_contracts(env: &Env) -> Vec<Address> {
    if env.storage().persistent().has(&DataKey::AllContractsLength) {
        let len: u32 = env.storage().persistent()
            .get::<DataKey, u32>(&DataKey::AllContractsLength)
            .unwrap_or(0);

        let mut all = Vec::new(env);
        let page_size = ALL_CONTRACTS_PAGE_SIZE;
        let pages = len.div_ceil(page_size);

        for page in 0..pages {
            let page_entries: Vec<Address> = env.storage().persistent()
                .get(&DataKey::AllContractsPage(page))
                .unwrap_or(Vec::new(env));

            for contract_id in page_entries.iter() {
                all.push_back(contract_id);
            }
        }

        return all;
    }

    let legacy: Vec<Address> = env.storage().instance()
        .get(&DataKey::AllContracts)
        .unwrap_or(Vec::new(env));

    if !legacy.is_empty() {
        Self::set_all_contracts_index(env, &legacy);
        env.storage().instance().remove(&DataKey::AllContracts);
    } else {
        env.storage().persistent().set(
            &DataKey::AllContractsLength,
            &0u32,
        );
    }

    legacy
}

/// Rewrite the global registration index in its new chunked persistent form.
fn set_all_contracts_index(env: &Env, all: &Vec<Address>) {
    let len = all.len();
    let page_size = ALL_CONTRACTS_PAGE_SIZE;
    let pages = len.div_ceil(page_size);

    for page in 0..pages {
        let start = page * page_size;
        let end = start + page_size;
        let mut chunk = Vec::new(env);

        for i in start..end {
            if let Some(contract_id) = all.get(i) {
                chunk.push_back(contract_id);
            }
        }

        env.storage()
            .persistent()
            .set(&DataKey::AllContractsPage(page), &chunk);
    }

    let mut stale = pages;

    while env.storage().persistent().has(&DataKey::AllContractsPage(stale)) {
        env.storage()
            .persistent()
            .remove(&DataKey::AllContractsPage(stale));

        stale += 1;
    }

    env.storage()
        .persistent()
        .set(&DataKey::AllContractsLength, &len);
}
    }

    // ── Initialization ──────────────────────────────────────────────────────

    /// One-time setup.  `admins` must be non-empty and `threshold` must be
    /// between 1 and `admins.len()`.
    pub fn initialize(env: Env, admins: Vec<Address>, threshold: u32) -> Result<(), RegistryError> {
        if env.storage().instance().has(&DataKey::Admins) {
            return Err(RegistryError::AlreadyInitialized);
        }

        if admins.len() < MIN_ADMINS {
            return Err(RegistryError::AdminSetTooSmall);
        }

        if threshold == 0
            || threshold > admins.len()
        {
            return Err(RegistryError::InvalidThreshold);
        }

        // Every admin must authorize the initialization.
        for admin in admins.iter() {
            admin.require_auth();
        }

        env.storage().instance().set(&DataKey::Admins, &admins);
        env.storage()
            .instance()
            .set(&DataKey::Threshold, &threshold);
        env.storage().instance().set(&DataKey::ProposalCount, &0u32);
        env.storage().instance().set(&DataKey::ContractCount, &0u32);
        env.storage()
            .instance()
            .set(&DataKey::TotalRegistered, &0u32);

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
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &proposer)?;

        // Make sure the target actually exists.
        if !env
            .storage()
            .persistent()
            .has(&DataKey::Contract(contract_id.clone()))
        {
            return Err(RegistryError::ContractNotFound);
        }

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::Deactivate(contract_id.clone()),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (
                proposal_id,
                proposer,
                Symbol::new(&env, "deactivate"),
                contract_id,
            ),
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
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &proposer)?;

        if admins.contains(&new_admin) {
            return Err(RegistryError::AlreadyAdmin);
        }

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::AddAdmin(new_admin.clone()),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (
                proposal_id,
                proposer,
                Symbol::new(&env, "add_admin"),
                new_admin,
            ),
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
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &proposer)?;

        if !admins.contains(&admin_to_remove) {
            return Err(RegistryError::AdminNotFound);
        }

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::RemoveAdmin(admin_to_remove.clone()),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (
                proposal_id,
                proposer,
                Symbol::new(&env, "remove_admin"),
                admin_to_remove,
            ),
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
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &proposer)?;

        if new_threshold == 0 || new_threshold > admins.len() {
            return Err(RegistryError::InvalidThreshold);
        }

        let current_threshold: u32 = env.storage().instance().get(&DataKey::Threshold).unwrap_or(1);
        if new_threshold == current_threshold {
            return Err(RegistryError::ThresholdAlreadySet);
        }

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::ChangeThreshold(new_threshold),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (
                proposal_id,
                proposer,
                Symbol::new(&env, "change_threshold"),
                new_threshold,
            ),
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
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &proposer)?;

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::Upgrade(new_wasm_hash.clone()),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (
                proposal_id,
                proposer,
                Symbol::new(&env, "upgrade"),
                new_wasm_hash,
            ),
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
    ///
    /// Both `token` and `treasury` must not be addresses already registered in
    /// the registry.  Permitting an overlap would create a confusing state: a
    /// `ContractEntry` whose owner could receive its own slashes, making the
    /// slash semantics circular.  The validation is intentionally placed here —
    /// at proposal time — so an obviously-invalid configuration is rejected
    /// immediately rather than sitting through the timelock only to revert on
    /// execution.
    pub fn propose_configure_staking(
        env: Env,
        proposer: Address,
        token: Address,
        treasury: Address,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &proposer)?;

        // Reject if either address is already a registered contract.
        // See `RegistryError::OverlappingAddress` for the full rationale.
        if env
            .storage()
            .persistent()
            .has(&DataKey::Contract(token.clone()))
        {
            return Err(RegistryError::OverlappingAddress);
        }
        if env
            .storage()
            .persistent()
            .has(&DataKey::Contract(treasury.clone()))
        {
            return Err(RegistryError::OverlappingAddress);
        }

        if let (Some(cur_token), Some(cur_treasury)) = (
            env.storage().instance().get::<DataKey, Address>(&DataKey::StakeToken),
            env.storage().instance().get::<DataKey, Address>(&DataKey::Treasury),
        ) {
            if cur_token == token && cur_treasury == treasury {
                return Err(RegistryError::StakingAlreadyConfigured);
            }
        }

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::ConfigureStaking(token.clone(), treasury.clone()),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (
                proposal_id,
                proposer,
                Symbol::new(&env, "configure_staking"),
                token,
            ),
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
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &proposer)?;

        if !env
            .storage()
            .persistent()
            .has(&DataKey::Contract(contract_id.clone()))
        {
            return Err(RegistryError::ContractNotFound);
        }

        let current_verified = env.storage().persistent()
            .get::<DataKey, bool>(&DataKey::Verified(contract_id.clone()))
            .unwrap_or(false);
        if current_verified == verified {
            return Err(RegistryError::AlreadyVerified);
        }

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::SetVerified(contract_id.clone(), verified),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (
                proposal_id,
                proposer,
                Symbol::new(&env, "set_verified"),
                contract_id,
            ),
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
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &proposer)?;

        if !env
            .storage()
            .persistent()
            .has(&DataKey::Contract(contract_id.clone()))
        {
            return Err(RegistryError::ContractNotFound);
        }
        Self::validate_positive_amount(amount)?;

        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::Slash(contract_id.clone(), amount, reason.clone()),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (
                proposal_id,
                proposer,
                Symbol::new(&env, "slash"),
                (contract_id, amount, reason),
            ),
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
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &proposer)?;
        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::SetAllowlistEnabled(enabled),
        );
        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (
                proposal_id,
                proposer,
                Symbol::new(&env, "set_allowlist"),
                enabled,
            ),
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
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &proposer)?;
        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::SetAllowlisted(owner.clone(), allowed),
        );
        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (
                proposal_id,
                proposer,
                Symbol::new(&env, "set_allowlisted"),
                (owner, allowed),
            ),
        );
        Ok(proposal_id)
    }

    /// Govern a fixed-window per-owner registration limit. Zero disables it.
    ///
    /// Named `propose_set_rate_limit` rather than the longer
    /// `propose_configure_registration_rate_limit`: Soroban caps exported
    /// contract function names at 32 characters, and the descriptive spelling
    /// overflows that (41 chars), which the SDK rejects at compile time.
    pub fn propose_set_rate_limit(
        env: Env,
        proposer: Address,
        limit: u32,
        window_ledgers: u32,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &proposer)?;
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
            (
                proposal_id,
                proposer,
                Symbol::new(&env, "configure_rate_limit"),
                (limit, window_ledgers),
            ),
        );
        Ok(proposal_id)
    }

    /// Set the registration fee. Zero disables it (registration becomes free).
    pub fn propose_set_registration_fee(
        env: Env,
        proposer: Address,
        fee: i128,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &proposer)?;
        if fee < 0 {
            return Err(RegistryError::InvalidAmount);
        }
        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::SetRegistrationFee(fee),
        );
        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (
                proposal_id,
                proposer,
                Symbol::new(&env, "set_registration_fee"),
                fee,
            ),
        );
        Ok(proposal_id)
    }

    /// Set the minimum stake threshold. Zero disables it (no minimum).
    pub fn propose_configure_minimum_stake(
        env: Env,
        proposer: Address,
        minimum: i128,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &proposer)?;
        if minimum < 0 {
            return Err(RegistryError::InvalidAmount);
        }
        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::ConfigureMinimumStake(minimum),
        );
        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (
                proposal_id,
                proposer,
                Symbol::new(&env, "configure_minimum_stake"),
                minimum,
            ),
        );
        Ok(proposal_id)
    }

    /// Propose withdrawing from the treasury.
    pub fn propose_withdraw_from_treasury(
        env: Env,
        proposer: Address,
        amount: i128,
    ) -> Result<u32, RegistryError> {
        proposer.require_auth();
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &proposer)?;
        if amount <= 0 {
            return Err(RegistryError::InvalidAmount);
        }
        let proposal_id = Self::create_proposal(
            &env,
            proposer.clone(),
            ProposalAction::WithdrawFromTreasury(amount),
        );
        env.events().publish(
            (Symbol::new(&env, "proposal_proposed"),),
            (
                proposal_id,
                proposer,
                Symbol::new(&env, "withdraw_from_treasury"),
                amount,
            ),
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
        let admins = Self::admin_index(&env);
        Self::assert_is_admin(&admins, &admin)?;

        let mut proposal = Self::load_proposal(&env, proposal_id)?;

        if proposal.executed {
            return Err(RegistryError::AlreadyExecuted);
        }

        // Reject duplicate approvals.
        if proposal.approvals.contains(&admin) {
            return Err(RegistryError::AlreadyApproved);
        }

        proposal.approvals.push_back(admin.clone());

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .unwrap_or(1);

        env.events().publish(
            (Symbol::new(&env, "proposal_approved"),),
            (proposal_id, admin.clone(), proposal.approvals.len(), threshold),
        );

        // Transition to ready when threshold is first reached.
        if proposal.ready_at == u32::MAX && proposal.approvals.len() >= threshold {
            proposal.ready_at = env.ledger().sequence();
            let executable_from = proposal.ready_at + TIMELOCK_LEDGERS;
            env.events().publish(
                (Symbol::new(&env, "proposal_ready"),),
                (proposal_id, proposal.ready_at, executable_from),
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

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .unwrap_or(1);
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
    pub fn deactivate(
        env: Env,
        caller: Address,
        contract_id: Address,
    ) -> Result<(), RegistryError> {
        caller.require_auth();

        let mut entry: ContractEntry = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        // Owners can self-deactivate instantly.  Admins deactivating someone
        // else's contract must go through the governance flow.
        if caller != entry.owner {
            return Err(RegistryError::Unauthorized);
        }

        entry.active = false;
        env.storage()
            .persistent()
            .set(&DataKey::Contract(contract_id.clone()), &entry);

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

        let entry: ContractEntry = env
            .storage()
            .persistent()
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
                env.storage()
                    .persistent()
                    .set(&DataKey::ByCategory(category), &index);
            }
        }
        env.storage()
            .persistent()
            .remove(&DataKey::Categories(contract_id.clone()));

        // Drop the global and owner indexes.
let mut all = Self::all_contracts(&env); main
        if let Some(i) = all.first_index_of(&contract_id) {
            all.remove(i);
            Self::set_all_contracts_index(&env, &all);
        }
        let mut owned = Self::owner_index(&env, &entry.owner);
        if let Some(i) = owned.first_index_of(&contract_id) {
            owned.remove(i);
            Self::set_owner_index(&env, &entry.owner, &owned);
        }

        // Delete the entry and its live reputation state. Slashes are kept
        // for auditability (see docstring above).
        let was_verified = env
            .storage()
            .persistent()
            .get::<DataKey, bool>(&DataKey::Verified(contract_id.clone()))
            .unwrap_or(false);
        let staked = Self::stake_of(&env, &contract_id);

        env.storage()
            .persistent()
            .remove(&DataKey::Contract(contract_id.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::Stake(contract_id.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::Verified(contract_id.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::WithdrawLockedUntil(contract_id.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::Tags(contract_id.clone()));
        // Attestations are opinions about a live registration; once the entry
        // is gone they have nothing left to refer to. Slashes, by contrast,
        // are kept above, because those stay auditable after the fact.
        env.storage()
            .persistent()
            .remove(&DataKey::Attestations(contract_id.clone()));

        if was_verified {
            let count: u32 = env
                .storage()
                .instance()
                .get(&DataKey::VerifiedCount)
                .unwrap_or(1);
            env.storage()
                .instance()
                .set(&DataKey::VerifiedCount, &count.saturating_sub(1));
        }

        if staked > 0 {
            let total_staked: i128 = env
                .storage()
                .instance()
                .get(&DataKey::TotalStaked)
                .unwrap_or(0);
            env.storage()
                .instance()
                .set(&DataKey::TotalStaked, &(total_staked - staked));
        }

        // Live total goes down; lifetime total does not (see #141).
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::ContractCount)
            .unwrap_or(1);
        env.storage()
            .instance()
            .set(&DataKey::ContractCount, &count.saturating_sub(1));

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
            if env
                .storage()
                .persistent()
                .has(&DataKey::Contract(contract_id.clone()))
            {
                live.push_back(contract_id);
            } else {
                removed += 1;
            }
        }

        if removed > 0 {
            env.storage()
                .persistent()
                .set(&DataKey::ByCategory(category), &live);
        }

        env.events()
            .publish((Symbol::new(&env, "category_pruned"),), (category, removed));

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
let mut all = Self::all_contracts(&env);
        let mut live = Vec::new(&env);
        let mut removed: u32 = 0;

        for contract_id in all.iter() {
            if env
                .storage()
                .persistent()
                .has(&DataKey::Contract(contract_id.clone()))
            {
                live.push_back(contract_id);
            } else {
                removed += 1;
            }
        }

        if removed > 0 {
            Self::set_all_contracts_index(&env, &live);
        }

        env.events()
            .publish((Symbol::new(&env, "all_contracts_pruned"),), (removed,));

        removed
    }

    // ── Legacy upgrade kept for backward-compatibility with existing tests ───

    /// Direct upgrade, kept for the upgrade-path tests in this crate (which
    /// deploy v1 wasm via `contractimport!` and then call `upgrade` with the
    /// old single-admin signature).
    ///
    /// For new deployments, use `propose_upgrade` / `approve_proposal` /
    /// `execute_proposal` instead.
    pub fn upgrade(
        env: Env,
        admin: Address,
        new_wasm_hash: BytesN<32>,
    ) -> Result<(), RegistryError> {
        admin.require_auth();

        // Accept either the old single-admin key or membership in the new set.
        let is_old_admin = env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::Admin)
            .map(|a| a == admin)
            .unwrap_or(false);
        let is_new_admin = Self::admin_index(&env).contains(&admin);

        if !is_old_admin && !is_new_admin {
            return Err(RegistryError::Unauthorized);
        }

        env.deployer()
            .update_current_contract_wasm(new_wasm_hash.clone());

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

        if env
            .storage()
            .instance()
            .get(&DataKey::AllowlistEnabled)
            .unwrap_or(false)
            && !env
                .storage()
                .persistent()
                .get(&DataKey::Allowlisted(owner.clone()))
                .unwrap_or(false)
        {
            return Err(RegistryError::NotAllowlisted);
        }

        if env
            .storage()
            .persistent()
            .has(&DataKey::Contract(contract_id.clone()))
        {
            return Err(RegistryError::AlreadyRegistered);
        }

        Self::validate_contract_metadata(&name, &description)?;

        let categories = Self::dedup_categories(&env, &categories)?;

        Self::consume_registration_rate(&env, &owner)?;

        let fee: i128 = env
            .storage()
            .instance()
            .get(&DataKey::RegistrationFee)
            .unwrap_or(0);
        if fee > 0 {
            let (token_id, treasury) = Self::staking_config(&env)?;
            token::Client::new(&env, &token_id).transfer(&owner, &treasury, &fee);
        }

        let entry = ContractEntry {
            contract_id: contract_id.clone(),
            owner: owner.clone(),
            name: name.clone(),
            description,
            registered_at: env.ledger().sequence(),
            active: true,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Contract(contract_id.clone()), &entry);

        env.storage().persistent().set(&DataKey::Expiry(contract_id.clone()), &(env.ledger().sequence() + EXPIRY_LEDGERS));
        let mut owned = Self::owner_index(&env, &owner);
        owned.push_back(contract_id.clone());
        Self::set_owner_index(&env, &owner, &owned);

let mut all = Self::all_contracts(&env);
        all.push_back(contract_id.clone());
        Self::set_all_contracts_index(&env, &all);

        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::ContractCount)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::ContractCount, &(count + 1));
        // Lifetime total: never decremented, so it keeps counting across
        // `deregister`. `ContractCount` above is the live total.
        let total: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TotalRegistered)
            .unwrap_or(count);
        env.storage()
            .instance()
            .set(&DataKey::TotalRegistered, &(total + 1));

        Self::index_categories(&env, &contract_id, &categories);

        env.events().publish(
            (Symbol::new(&env, "contract_registered"),),
            (contract_id, owner, name, categories),
        );

        Ok(())
    }

    /// Register multiple contracts in a single atomic transaction.
    /// Fails and registers nothing if any entry is invalid or already registered.
    /// The combined count of new registrations is subject to the per-owner cap
    /// if registration rate limiting is enabled.
    pub fn register_contracts(
        env: Env,
        owner: Address,
        entries: Vec<RegistrationEntry>,
    ) -> Result<(), RegistryError> {
        owner.require_auth();

        if entries.is_empty() {
            return Err(RegistryError::InvalidMetadata);
        }

        let max_batch: u32 = 100;
        if entries.len() > max_batch {
            return Err(RegistryError::InvalidMetadata);
        }

        if env
            .storage()
            .instance()
            .get(&DataKey::AllowlistEnabled)
            .unwrap_or(false)
            && !env
                .storage()
                .persistent()
                .get(&DataKey::Allowlisted(owner.clone()))
                .unwrap_or(false)
        {
            return Err(RegistryError::NotAllowlisted);
        }

        for entry in entries.iter() {
            if env
                .storage()
                .persistent()
                .has(&DataKey::Contract(entry.contract_id.clone()))
            {
                return Err(RegistryError::AlreadyRegistered);
            }
            Self::validate_contract_metadata(&entry.name, &entry.description)?;
            Self::dedup_categories(&env, &entry.categories)?;
        }

        let fee: i128 = env
            .storage()
            .instance()
            .get(&DataKey::RegistrationFee)
            .unwrap_or(0);
        if fee > 0 {
            let total_fee = fee * (entries.len() as i128);
            let (token_id, treasury) = Self::staking_config(&env)?;
            token::Client::new(&env, &token_id).transfer(&owner, &treasury, &total_fee);
        }

        for entry in entries.iter() {
            Self::consume_registration_rate(&env, &owner)?;

            let contract_entry = ContractEntry {
                contract_id: entry.contract_id.clone(),
                owner: owner.clone(),
                name: entry.name.clone(),
                description: entry.description.clone(),
                registered_at: env.ledger().sequence(),
                active: true,
            };

            env.storage().persistent().set(
                &DataKey::Contract(entry.contract_id.clone()),
                &contract_entry,
            );

            env.storage().persistent().set(&DataKey::Expiry(entry.contract_id.clone()), &(env.ledger().sequence() + EXPIRY_LEDGERS));
            let mut owned = Self::owner_index(&env, &owner);
            owned.push_back(entry.contract_id.clone());
            Self::set_owner_index(&env, &owner, &owned);

use soroban_sdk::{
    contractimpl, token, Address, Env, Result, String, Symbol, Vec,
};

// Assuming all required structs, data keys, and errors are imported:
// DataKey, ContractEntry, Category, RegistryError, Proposal, Attestation,
// SlashRecord, Reputation, ContractProfile, RegistryStats, etc.

pub struct LuminaRegistry;

#[contractimpl]
impl LuminaRegistry {
    // ── Registration Metadata & Taxonomies ──────────────────────────────────

    /// Re-declare which categories a registration is browsable under.
    pub fn set_categories(
        env: Env,
        owner: Address,
        contract_id: Address,
        categories: Vec<Category>,
    ) -> Result<(), RegistryError> {
        owner.require_auth();

        let entry: ContractEntry = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        if owner != entry.owner {
            return Err(RegistryError::NotOwner);
        }

        let categories = Self::dedup_categories(&env, &categories)?;

        // Drop the registration from any category it is leaving
        for previous in Self::categories_of(&env, &contract_id).iter() {
            if !categories.contains(previous) {
                let mut index = Self::category_index(&env, &previous);
                if let Some(i) = index.first_index_of(&contract_id) {
                    index.remove(i);
                    env.storage()
                        .persistent()
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

    /// Set normalized, owner-defined tags for a registration.
    pub fn set_tags(
        env: Env,
        owner: Address,
        contract_id: Address,
        tags: Vec<String>,
    ) -> Result<(), RegistryError> {
        owner.require_auth();

        let entry: ContractEntry = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        if owner != entry.owner {
            return Err(RegistryError::NotOwner);
        }

        const MAX_TAG_COUNT: u32 = 10;
        const MAX_TAG_LEN: u32 = 16;

        if tags.len() > MAX_TAG_COUNT {
            return Err(RegistryError::InvalidTags);
        }

        for tag in tags.iter() {
            if tag.len() > MAX_TAG_LEN {
                return Err(RegistryError::InvalidTags);
            }
        }

        env.storage()
            .persistent()
            .set(&DataKey::Tags(contract_id.clone()), &tags);

        env.events().publish(
            (Symbol::new(&env, "tags_updated"),),
            (contract_id, owner, tags.len()),
        );

        Ok(())
    }

    /// Get tags for a registration.
    pub fn get_tags(env: Env, contract_id: Address) -> Vec<String> {
        env.storage()
            .persistent()
            .get(&DataKey::Tags(contract_id))
            .unwrap_or(Vec::new(&env))
    }

    /// Point a registration at its replacement.
    /// Requires the owner to own both the old and new contract entries.
    pub fn set_superseded_by(
        env: Env,
        owner: Address,
        contract_id: Address,
        replacement: Address,
    ) -> Result<(), RegistryError> {
        owner.require_auth();

        let old_entry: ContractEntry = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        if owner != old_entry.owner {
            return Err(RegistryError::NotOwner);
        }

        let replacement_entry: ContractEntry = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(replacement.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        if replacement_entry.owner != owner {
            return Err(RegistryError::Unauthorized);
        }

        env.storage()
            .persistent()
            .set(&DataKey::SupersededBy(contract_id.clone()), &replacement);

        env.events().publish(
            (Symbol::new(&env, "superseded_by"),),
            (contract_id, replacement),
        );

        Ok(())
    }

    // ── Third-Party Attestations ────────────────────────────────────────────

    /// Vouch for a registration with a short, bounded label.
    pub fn attest(
        env: Env,
        attester: Address,
        contract_id: Address,
        label: String,
    ) -> Result<(), RegistryError> {
        attester.require_auth();

        if !env
            .storage()
            .persistent()
            .has(&DataKey::Contract(contract_id.clone()))
        {
            return Err(RegistryError::ContractNotFound);
        }

        if label.is_empty() || label.len() > MAX_ATTESTATION_LABEL_LEN {
            return Err(RegistryError::InvalidAttestation);
        }

        let mut attestations = Self::attestations_of(&env, &contract_id);
        let created_at = env.ledger().sequence();

        for i in 0..attestations.len() {
            if let Some(existing) = attestations.get(i) {
                if existing.attester == attester {
                    let recorded = label.clone();
                    attestations.set(
                        i,
                        Attestation {
                            attester: attester.clone(),
                            label,
                            created_at,
                        },
                    );
                    env.storage()
                        .persistent()
                        .set(&DataKey::Attestations(contract_id.clone()), &attestations);
                    env.events().publish(
                        (Symbol::new(&env, "attestation_updated"),),
                        (contract_id, attester, recorded),
                    );
                    return Ok(());
                }
            }
        }

        if attestations.len() >= MAX_ATTESTATIONS_PER_CONTRACT {
            return Err(RegistryError::InvalidAttestation);
        }

        attestations.push_back(Attestation {
            attester: attester.clone(),
            label,
            created_at,
        });
        env.storage()
            .persistent()
            .set(&DataKey::Attestations(contract_id.clone()), &attestations);

        env.events().publish(
            (Symbol::new(&env, "attestation_added"),),
            (contract_id, attester, attestations.len()),
        );

        Ok(())
    }

    /// Withdraw your own attestation from a registration.
    pub fn revoke_attestation(
        env: Env,
        attester: Address,
        contract_id: Address,
    ) -> Result<u32, RegistryError> {
        attester.require_auth();

        if !env
            .storage()
            .persistent()
            .has(&DataKey::Contract(contract_id.clone()))
        {
            return Err(RegistryError::ContractNotFound);
        }

        let mut attestations = Self::attestations_of(&env, &contract_id);

        let index = (0..attestations.len())
            .find(|i| {
                attestations
                    .get(*i)
                    .map(|a| a.attester == attester)
                    .unwrap_or(false)
            })
            .ok_or(RegistryError::AttestationNotFound)?;

        attestations.remove(index);
        let remaining = attestations.len();
        env.storage()
            .persistent()
            .set(&DataKey::Attestations(contract_id), &attestations);

        env.events().publish(
            (Symbol::new(&env, "attestation_revoked"),),
            (attester, remaining),
        );

        Ok(remaining)
    }

    /// Every third-party attestation on a registration, oldest first.
    pub fn get_attestations(env: Env, contract_id: Address) -> Vec<Attestation> {
        Self::attestations_of(&env, &contract_id)
    }

    // ── Staking ─────────────────────────────────────────────────────────────

    /// Post collateral against a registration you own.
    pub fn stake(
        env: Env,
        owner: Address,
        contract_id: Address,
        amount: i128,
    ) -> Result<(), RegistryError> {
        owner.require_auth();

        Self::validate_positive_amount(amount)?;

        let entry: ContractEntry = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        if owner != entry.owner {
            return Err(RegistryError::NotOwner);
        }

        let (token_id, _) = Self::staking_config(&env)?;

        token::Client::new(&env, &token_id).transfer(
            &owner,
            &env.current_contract_address(),
            &amount,
        );

        let old_stake = Self::stake_of(&env, &contract_id);
        let staked = old_stake + amount;
        env.storage()
            .persistent()
            .set(&DataKey::Stake(contract_id.clone()), &staked);

        // Increment active staked count counter if transitioning from zero stake
        if old_stake == 0 && staked > 0 {
            let count: u32 = env
                .storage()
                .instance()
                .get(&DataKey::StakedCount)
                .unwrap_or(0);
            env.storage()
                .instance()
                .set(&DataKey::StakedCount, &(count + 1));
        }

        let total_staked: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalStaked)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::TotalStaked, &(total_staked + amount));

        let minimum: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MinimumStake)
            .unwrap_or(0);
        if minimum > 0 && old_stake < minimum && staked >= minimum {
            env.events().publish(
                (Symbol::new(&env, "stake_crossed_minimum"),),
                (
                    contract_id.clone(),
                    staked,
                    minimum,
                    Symbol::new(&env, "above"),
                ),
            );
        }

        env.events().publish(
            (Symbol::new(&env, "stake_deposited"),),
            (contract_id, owner, amount, staked),
        );

        Ok(())
    }

    /// Reclaim the full remaining stake for a registration.
    pub fn withdraw_stake(
        env: Env,
        owner: Address,
        contract_id: Address,
    ) -> Result<i128, RegistryError> {
        owner.require_auth();

        let entry: ContractEntry = env
            .storage()
            .persistent()
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

        token::Client::new(&env, &token_id).transfer(
            &env.current_contract_address(),
            &owner,
            &staked,
        );

        env.storage()
            .persistent()
            .set(&DataKey::Stake(contract_id.clone()), &0i128);

        // Decrement staked contract count
        let staked_count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::StakedCount)
            .unwrap_or(0);
        if staked_count > 0 {
            env.storage()
                .instance()
                .set(&DataKey::StakedCount, &(staked_count - 1));
        }

        let total_staked: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalStaked)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::TotalStaked, &(total_staked - staked));

        let minimum: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MinimumStake)
            .unwrap_or(0);
        if minimum > 0 && staked >= minimum {
            env.events().publish(
                (Symbol::new(&env, "stake_crossed_minimum"),),
                (
                    contract_id.clone(),
                    0i128,
                    minimum,
                    Symbol::new(&env, "below"),
                ),
            );
        }

        env.events().publish(
            (Symbol::new(&env, "stake_withdrawn"),),
            (contract_id, owner, staked),
        );

        Ok(staked)
    }

    // ── Views ──────────────────────────────────────────────────────────────

    /// Which build of the registry is live at this address.
    pub fn get_version(_env: Env) -> u32 {
        CONTRACT_VERSION
    }

    /// The first admin address.
    pub fn get_admin(env: Env) -> Result<Address, RegistryError> {
        if let Some(a) = env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::Admin)
        {
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

    /// The categories a registration declared.
    pub fn get_categories(env: Env, contract_id: Address) -> Vec<Category> {
        Self::categories_of(&env, &contract_id)
    }

    /// Paginated list of active registrations in one category.
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
                if let Some(entry) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id))
                {
                    if entry.active {
                        result.push_back(entry);
                    }
                }
            }
            i += 1;
        }

        result
    }

    /// Paginated list of active registrations in multiple categories.
    pub fn get_contracts_by_categories(
        env: Env,
        categories: Vec<Category>,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<ContractEntry>, RegistryError> {
        if categories.is_empty() {
            return Err(RegistryError::NoCategories);
        }

        let mut seen = Vec::new(&env);
        let mut result = Vec::new(&env);

        for category in categories.iter() {
            let index = Self::category_index(&env, &category);
            for contract_id in index.iter() {
                if !seen.contains(&contract_id) {
                    seen.push_back(contract_id.clone());

                    if let Some(entry) = env
                        .storage()
                        .persistent()
                        .get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id.clone()))
                    {
                        if entry.active {
                            result.push_back(entry);
                        }
                    }
                }
            }
        }

        let total = result.len();
        let mut page = Vec::new(&env);
        let mut i = offset;
        while i < total && page.len() < limit {
            if let Some(entry) = result.get(i) {
                page.push_back(entry);
            }
            i += 1;
        }

        Ok(page)
    }

    pub fn get_staking_config(env: Env) -> Result<(Address, Address), RegistryError> {
        Self::staking_config(&env)
    }

    pub fn get_registration_fee(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::RegistrationFee)
            .unwrap_or(0)
    }

    pub fn get_minimum_stake(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::MinimumStake)
            .unwrap_or(0)
    }

    pub fn get_stake(env: Env, contract_id: Address) -> i128 {
        Self::stake_of(&env, &contract_id)
    }

    pub fn is_verified(env: Env, contract_id: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Verified(contract_id))
            .unwrap_or(false)
    }

    /// Aggregate registry statistics running in O(1) time.
    pub fn get_registry_stats(env: Env) -> RegistryStats {
        let total_registered = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::TotalRegistered)
            .unwrap_or(0);
        let active_count = Self::get_active_contract_count(env.clone());
        let verified_count = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::VerifiedCount)
            .unwrap_or(0);
        let total_staked = env
            .storage()
            .instance()
            .get::<DataKey, i128>(&DataKey::TotalStaked)
            .unwrap_or(0);
        let staked_count = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::StakedCount)
            .unwrap_or(0);

        RegistryStats {
            total_registered,
            active_count,
            verified_count,
            staked_count,
            total_staked,
        }
    }

    pub fn get_slashes(env: Env, contract_id: Address) -> Vec<SlashRecord> {
        Self::slash_history(&env, &contract_id)
    }

    /// Attach an owner response to a slash record.
    pub fn respond_to_slash(
        env: Env,
        owner: Address,
        contract_id: Address,
        slash_index: u32,
        response: String,
    ) -> Result<(), RegistryError> {
        owner.require_auth();

        if response.is_empty() {
            return Err(RegistryError::InvalidInput);
        }

        let entry: ContractEntry = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        if owner != entry.owner {
            return Err(RegistryError::NotOwner);
        }

        let mut history = Self::slash_history(&env, &contract_id);

        if slash_index >= history.len() {
            return Err(RegistryError::SlashNotFound);
        }

        let mut record = history.get(slash_index).unwrap();

        if record.response.is_some() {
            return Err(RegistryError::ResponseAlreadyExists);
        }

        record.response = Some(response.clone());
        history.set(slash_index, record);

        env.storage()
            .persistent()
            .set(&DataKey::Slashes(contract_id.clone()), &history);

        env.events().publish(
            (Symbol::new(&env, "slash_response_added"),),
            (contract_id, slash_index, owner),
        );

        Ok(())
    }

    pub fn get_reputation(env: Env, contract_id: Address) -> Reputation {
        Self::reputation_of(&env, &contract_id)
    }

    pub fn get_contract_profile(
        env: Env,
        contract_id: Address,
    ) -> Result<ContractProfile, RegistryError> {
        let entry: ContractEntry = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(contract_id.clone()))
            .ok_or(RegistryError::ContractNotFound)?;

        let mut slashed_total: i128 = 0;
        let slashes: Vec<SlashRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Slashes(contract_id.clone()))
            .unwrap_or(Vec::new(&env));
        for record in slashes.iter() {
            slashed_total += record.amount;
        }

        let reputation = Reputation {
            stake: env
                .storage()
                .persistent()
                .get(&DataKey::Stake(contract_id.clone()))
                .unwrap_or(0),
            verified: env
                .storage()
                .persistent()
                .get(&DataKey::Verified(contract_id.clone()))
                .unwrap_or(false),
            slashed_total,
            withdraw_locked_until: env
                .storage()
                .persistent()
                .get(&DataKey::WithdrawLockedUntil(contract_id.clone()))
                .unwrap_or(0),
        };

        Ok(ContractProfile {
            reputation,
            entry,
            superseded_by: env
                .storage()
                .persistent()
                .get(&DataKey::SupersededBy(contract_id)),
        })
    }

    /// `get_active_contracts`, with each entry's reputation attached.
    pub fn get_active_profiles(env: Env, offset: u32, limit: u32) -> Vec<ContractProfile> {
        let all = Self::all_contracts(&env);
        let mut result = Vec::new(&env);

        let mut i = offset;
        while i < all.len() && result.len() < limit {
            if let Some(contract_id) = all.get(i) {
                if let Some(entry) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id.clone()))
                {
                    if entry.active {
                        result.push_back(ContractProfile {
                            reputation: Self::reputation_of(&env, &contract_id),
                            entry,
                            superseded_by: env
                                .storage()
                                .persistent()
                                .get(&DataKey::SupersededBy(contract_id)),
                        });
                    }
                }
            }
            i += 1;
        }

        result
    }
}