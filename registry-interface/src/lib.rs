// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
#`!no_std]
// Soroban's `#[contracttype]`, `#[contracterror]`, `#[contractimpl]` and
// `#[contractclient]` macros emit synthetic items — the `SPEC` constants, the
// generated client methods, the error-code helpers — carrying the invocation
// site's span. `missing_docs` reports those as undocumented and there is no
// source position to attach a doc comment to, so on current rustc the lint
// cannot be satisfied by any edit to this crate. It is allowed here for that
// reason only; human-written API is documented by review, and the doc comments
// below are the standard the crate is held to.
#![allow(missing_docs)]
/// Typed, read-only client for the Lumina Registry — for *contracts*, not
/// wallets.
///
/// A Soroban contract that wants to ask "is this address listed, and is it
/// verified?" has two options today, and both are bad: hand-write
/// `env.invoke_contract(&stack, symbol_short!("is_registered"), ...))` and
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
/// use soroban_sdk::{Address, Env};
///
/// fn check(env: &Env, registry: &Address, counterparty: &Address) {
/// let registry = RegistryInterfaceClient::new(env, registry);
/// if registry.is_registered(counterparty) && registry.is_verified(counterparty) {
///     // ...
/// }
/// }
/// ```
///
///# Why the types are declared here instead of imported
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
/// A cross-contract read is *not* free, and not free in the way people
/// expect. It is not a `simulateTransaction` — a contract calling the registry
/// on-chain spends the transaction's whole resource budget, and the callee's
/// instructions and ledger reads are charged to *you*.
///
/// Concretely, each read is one nested invocation frame, which costs:
///
/// - a fixed instruction charge for the call itself, before the callee runs
///   any code;
/// - every ledger entry the callee touches, at the callee's TVL — the registry
///   stores registrations in `persistent` entries, so a read is a persistent
///   entry read, which is the expensive kind;
/// - a fresh 1 MiB memory allocation for the callee's frame, and the memory
///   cost of decoding the arguments you passed in and the result you get back.
///
/// The practical consequence: **number of calls is what you pay for.** Two
/// `is_*` calls cost strictly more than one `get_contract_profile` that returns
/// both facts, and a loop over counterparties multiplies the fixed per-call
/// charge every iteration. The `examples/registry-consumer` crate measures this
/// on the real registry wasm rather than estimating it — see its `cost` module
/// and the "What a cross-contract read costs" section of the README.
///
/// ## Reentrancy across the token transfer boundary
///
/// The registry moves tokens in three paths: `stake`, `withdraw_stake`, and the
/// slash path that governance drives. Each of these calls into an external
/// token contract, which is code the registry does not control. The ordering
/// therefore matters:
///
/// - **State is written before the external call.** Every path that moves
///   tokens follows checks-effects-interactions: the stored balance is
///   updated first, then the transfer is issued. A token that reenters
///   `withdraw_stake` during its own `transfer` sees a zero balance and
///   cannot withdraw twice.
///
/// - **Soroban does not guarantee atomicity of a cross-contract call.**
///   The host does not prevent reentrancy, and it does not roll back a
///   partially-completed call automatically unless the call returns an
///   error or panics. A callee that returns successfully after mutating
///   state leaves that mutation in place. The registry therefore cannot
///   rely on the host to defend it; it must order its own writes.
///
/// - **Authorization is not a reentrancy defense.** `require_auth` is checked
///   once at the entrypoint and does not gate nested calls that the same
///   authorized address makes. A token that the registry calls can call back
///   into the registry with the registry's own authority still in force.
///
/// The guarantee this crate documents is therefore a *contract-level* one,
/// not a host-level one: every token-moving entrypoint writes its state
/// before it calls out. The test suite exercises this with a reentrant token
/// contract that attempts a double withdrawal and asserts the second
/// attempt fails.

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
/// entrypoint writes its stored balance before it issues the transfer,
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
    fn get_contracts_by_categories(
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

    /// The stake a registration has to hold to stay listed. Zero means the
/// threshold is not open — nothing is refused for being under-staked.
    fn get_minimum_stake(env: Env) -> i128;

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
/// never registered and returns `false`. Registration refuses `G`…`
/// addresses, so a `G…` in the registry is not a state this read can
/// observe — the downstream `isContractAddress` filter that
/// `lumina-backend/indexer/src/index.ts` had to add
    fn is_registered(env: Env, contract_id: Address) -> bool;
}
