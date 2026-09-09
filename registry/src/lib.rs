#![no_std]
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
    contract, contractimpl, contracttype, contracterror,
    Address, BytesN, Env, Symbol, String, Vec,
};

// ─── Version ───────────────────────────────────────────────────────────────

/// Version of the deployed code, returned by [`LuminaRegistry::get_version`].
///
/// Bump this in the same commit as any change to the exported interface or to
/// the storage shapes below.
pub const CONTRACT_VERSION: u32 = 1;

/// Minimum number of ledgers that must elapse between a proposal reaching
/// threshold and becoming executable.  At ~6 s per ledger this is roughly
/// 24 h, giving affected parties a window to notice and react before an
/// admin action takes effect.
///
/// In tests we use a much smaller value so ledger-advance doesn't archive
/// instance storage entries before `execute_proposal` can read them.
#[cfg(not(test))]
pub const TIMELOCK_LEDGERS: u32 = 17_280;

#[cfg(test)]
pub const TIMELOCK_LEDGERS: u32 = 10;

// ─── Errors ────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RegistryError {
    AlreadyInitialized  = 1,
    Unauthorized        = 2,
    AlreadyRegistered   = 3,
    ContractNotFound    = 4,
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
    ContractCount,
    Contract(Address),
    OwnerContracts(Address), // owner → Vec<Address>
    AllContracts,            // insertion-ordered Vec<Address> of every registered contract

    // ── Legacy key kept for upgrade compatibility ────────────────────────
    /// Single-admin key written by the original v1 initialize.  Retained so
    /// that the registry-v2 upgrade tests, which read `DataKey::Admin` from
    /// instance storage, continue to decode correctly after an upgrade.
    Admin,
}

// ─── Contract ──────────────────────────────────────────────────────────────

#[contract]
pub struct LuminaRegistry;

#[contractimpl]
impl LuminaRegistry {
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
        for i in 0..admins.len() {
            admins.get(i).unwrap().require_auth();
        }

        env.storage().instance().set(&DataKey::Admins, &admins);
        env.storage().instance().set(&DataKey::Threshold, &threshold);
        env.storage().instance().set(&DataKey::ProposalCount, &0u32);
        env.storage().instance().set(&DataKey::ContractCount, &0u32);

        // Write the legacy Admin key with the first admin so the v2 upgrade
        // tests (which read DataKey::Admin) continue to pass unchanged.
        let first_admin = admins.get(0).unwrap();
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

        env.events().publish(
            (Symbol::new(&env, "registry_upgraded"),),
            (admin, new_wasm_hash, CONTRACT_VERSION),
        );

        Ok(())
    }

    // ── Registry ────────────────────────────────────────────────────────────

    /// Register a Soroban contract for Lumina indexing.
    /// Anyone can register — the owner must authorize the call.
    pub fn register_contract(
        env: Env,
        owner: Address,
        contract_id: Address,
        name: String,
        description: String,
    ) -> Result<(), RegistryError> {
        owner.require_auth();

        if env.storage().persistent().has(&DataKey::Contract(contract_id.clone())) {
            return Err(RegistryError::AlreadyRegistered);
        }

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

        env.events().publish(
            (Symbol::new(&env, "contract_registered"),),
            (contract_id, owner, name),
        );

        Ok(())
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
        if admins.is_empty() {
            return Err(RegistryError::NotInitialized);
        }
        Ok(admins.get(0).unwrap())
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

    pub fn get_contract(env: Env, contract_id: Address) -> Result<ContractEntry, RegistryError> {
        env.storage().persistent()
            .get(&DataKey::Contract(contract_id))
            .ok_or(RegistryError::ContractNotFound)
    }

    pub fn get_contract_count(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::ContractCount).unwrap_or(0)
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
            let contract_id = all.get(i).unwrap();
            if let Some(entry) = env.storage().persistent().get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id)) {
                if entry.active {
                    result.push_back(entry);
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
            let contract_id = owned.get(i).unwrap();
            if let Some(entry) = env.storage().persistent().get::<DataKey, ContractEntry>(&DataKey::Contract(contract_id)) {
                result.push_back(entry);
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
        }
        Ok(())
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
    use soroban_sdk::testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke};
    use soroban_sdk::IntoVal;

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
        let contract_id = env.register(LuminaRegistry, ());
        let client = LuminaRegistryClient::new(&env, &contract_id);

        let a1 = Address::generate(&env);
        let a2 = Address::generate(&env);
        let a3 = Address::generate(&env);
        let mut admins = Vec::new(&env);
        admins.push_back(a1.clone());
        admins.push_back(a2.clone());
        admins.push_back(a3.clone());
        client.initialize(&admins, &2);
        (env, client, a1, a2, a3)
    }

    /// Set up a registry with a single admin for tests that don't need multi-sig.
    fn setup() -> (Env, LuminaRegistryClient<'static>, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(LuminaRegistry, ());
        let client = LuminaRegistryClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let mut admins = Vec::new(&env);
        admins.push_back(admin.clone());
        client.initialize(&admins, &1);
        (env, client, admin)
    }

    fn register_sample(env: &Env, client: &LuminaRegistryClient) -> (Address, Address) {
        let owner = Address::generate(env);
        let target = Address::generate(env);
        client.register_contract(
            &owner,
            &target,
            &String::from_str(env, "Test Contract"),
            &String::from_str(env, "A test contract"),
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
    fn initialize_with_zero_threshold_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(LuminaRegistry, ());
        let client = LuminaRegistryClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let mut admins = Vec::new(&env);
        admins.push_back(admin.clone());
        assert_eq!(
            client.try_initialize(&admins, &0),
            Err(Ok(RegistryError::InvalidThreshold))
        );
    }

    #[test]
    fn initialize_with_threshold_exceeding_set_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(LuminaRegistry, ());
        let client = LuminaRegistryClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let mut admins = Vec::new(&env);
        admins.push_back(admin.clone());
        // threshold=2 but only 1 admin
        assert_eq!(
            client.try_initialize(&admins, &2),
            Err(Ok(RegistryError::InvalidThreshold))
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
        client.try_approve_proposal(&a1, &pid).unwrap_err();

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
        );
        assert_eq!(result, Err(Ok(RegistryError::AlreadyRegistered)));
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
    fn admin_gated_calls_on_uninitialized_registry_error() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(LuminaRegistry, ());
        let client = LuminaRegistryClient::new(&env, &contract_id);
        let (_owner, target) = register_sample(&env, &client);
        let stranger = Address::generate(&env);
        let new_owner = Address::generate(&env);

        assert_eq!(
            client.try_transfer_ownership(&stranger, &target, &new_owner),
            Err(Ok(RegistryError::NotInitialized)),
        );
    }

    // ── Upgrade-path tests ──────────────────────────────────────────────────

    fn deploy_v1(env: &Env) -> (registry_v1_wasm::Client<'static>, Address, Address) {
        let contract_id = env.register(registry_v1_wasm::WASM, ());
        let client = registry_v1_wasm::Client::new(env, &contract_id);
        let admin = Address::generate(env);
        let mut admins = Vec::new(env);
        admins.push_back(admin.clone());
        client.initialize(&admins, &1);
        (client, admin, contract_id)
    }

    fn register_via(env: &Env, client: &registry_v1_wasm::Client, owner: &Address) -> Address {
        let target = Address::generate(env);
        client.register_contract(
            owner,
            &target,
            &String::from_str(env, "Test Contract"),
            &String::from_str(env, "A test contract"),
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

        assert_eq!(v1.get_version(), 1);
        assert_eq!(v1.get_contract_count(), 3);

        let v2_hash = env.deployer().upload_contract_wasm(registry_v2_wasm::WASM);
        v1.upgrade(&admin, &v2_hash);

        let v2 = registry_v2_wasm::Client::new(&env, &contract_id);
        assert_eq!(v2.get_version(), 2);
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
        assert_eq!(registry_v2_wasm::Client::new(&env, &contract_id).get_version(), 2);
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
        assert_eq!(v2.get_version(), 2);

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
    fn get_admin_before_initialize_reports_not_initialized() {
        let env = Env::default();
        let contract_id = env.register(LuminaRegistry, ());
        let client = LuminaRegistryClient::new(&env, &contract_id);
        assert_eq!(
            client.try_get_admin(),
            Err(Ok(RegistryError::NotInitialized))
        );
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
}
