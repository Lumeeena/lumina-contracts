// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
#no_std
cwarn(missing_docs)
/// Typed, read-only client for the Lumina Registry — for *contracts*, not
/// wallets.
///
/// A Soroban contract that wants to ask "is this address listed, and is it
/// verified?" has two options today, and both are bad: hand-write
/// `env.invoke_contract(&stack, symbol_short!("is_registered"), ...)` and
/// decode the `Val` yourself, or use `contractimport!` on the registry's wasm.
/// The second pulls the whole registry binary into your build, and the first
/// is unchecked at compile time — a renamed export becomes a runtime failure
/// in someone else's contract.
///
/// This crate is the third option: a declared trait covering the registry's
/// read-only surface, and the [`RegistryInterfaceClient`] that
/// [`soroban_sdk::contractclient`] generates from it.
///
/// ```no_run
/// use lumina_registry_interface::RegistryInterfaceClient;
/// use soroban_sdk:{Address, Env};
///
/// # fn check(env: &Env, registry: &Address, counterparty: &Address) {
/// let registry = RegistryInterfaceClient::new(env, registry);
/// if registry.is_registered(counterparty) && registry.is_verified(counterparty) {
///     // ...
/// }
/// # }
/// ```
///
/// ## Why the types are declared here instead of imported
///
/// [`ContractEntry`], [`Category`], [`Reputation`] and friends are deliberately
/// *duplicated* from `lumina-registry` rather than re-exported from it. A
/// dependency edge on the contract crate would drag the registry's entire
/// `#[contractimpl]` — every exported entrypoint and its spec — into every
/// consumer's wasm, which is both a size problem and a link problem: two
/// `#[contractimpl]`s exporting the same symbol do not coexist. `registry-v2`
/// does the same thing for the same reason, and says so at length.
///
/// The duplication is a real risk — the two declarations could drift — so it
/// is *tested* rather than trusted. `tests/interface_matches_registry.rs` reads
/// the registry's compiled spec out of its wasm and asserts that every
/// function, type and error code declared here matches what the contract
/// actually exports. Run against a changed registry, it fails with the
/// signature that moved.
///
/// ## The cost of a read
///
/// A cross-contract read is **not** free, and not free in the way people
/// expect. It is not a `simulateTransaction` — a contract calling the registry
/// on-chain spends the transaction's whole resource budget, and the callee's
/// instructions and ledger reads are charged to *you*.
///
/// Concretely, each read is one nested invocation frame, which costs:
///
/// - a fixed instruction charge for the call itself, before the callee runs
///   any code;
/// - every ledger entry the callee touches, at the callee's TTL — the registry
///   stores registrations in `persistent` entries, so a read is a persistent
///   entry read, which is the expensive kind;
/// - a fresh 1 MiB memory allocation for the callee's frame, and the memory
///   cost of decoding the arguments you passed in and the result you get back.
///
/// The practical consequence: **the number of calls is what you pay for.** Two
/// `is_*` calls cost strictly more than one `get_contract_profile` that returns
/// both facts, and a loop over counterparties multiplies the fixed per-call
/// charge every iteration. The `examples/registry-consumer` crate measures this
/// on the real registry wasm rather than estimating it — see its `cost` module
/// and the "What a cross-contract read costs" section of the README.

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
}

/// A page of registration entries.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractPage {
    /// The entries in this page.
    pub entries: Vec<ContractEntry>,
    /// Whether more entries follow.
    pub has_more: bool,
}

/// A registration joined with its reputation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractProfile {
    /// The registration entry.
    pub entry: ContractEntry,
    /// The registration's reputation.
    pub reputation: Reputation,
}

/// A page of registration profiles.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractProfilePage {
    /// The profiles in this page.
    pub entries: Vec<ContractProfile>,
    /// Whether more entries follow.
    pub has_more: bool,
}

/// A governance proposal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    /// The action to execute.
    pub action: ProposalAction,
    /// Admins who have approved.
    pub approvals: Vec<Address>,
    /// Whether the proposal has been executed.
    pub executed: bool,
    /// The proposal ID.
    pub id: u32,
    /// The admin who proposed it.
    pub proposer: Address,
    /// Ledger timestamp when the timelock elapses.
    pub ready_at: u32,
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
}
