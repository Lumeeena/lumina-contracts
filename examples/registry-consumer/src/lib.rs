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
//! Example consumer of the Lumina Registry's typed read-only interface.
//!
//! This contract exists to answer one question with runnable code: **what
//! does it look like for a contract to ask the registry "is this address
//! listed, and is it verified?"** Everything here is small and real — it stores
//! state, it enforces a rule, it reverts — so the interface usage is not hidden
//! behind scaffolding.
//!
//! The contract is a minimal venue with a **counterparty policy**: deposits
//! name an `operator` contract, and the venue only accepts deposits from
//! operators the registry lists. Operators the registry has additionally
//! attested as verified, and that have posted stake, get a lower fee. The
//! decision is copied into the venue's own record at listing time, so it stays
//! auditable later without re-reading the registry.
//!
//! ## The part worth copying
//!
//! Everything this contract knows about the registry comes from
//! [`RegistryInterfaceClient`]. There is no `invoke_contract`, no
//! `contractimport!`, and no hand-decoded `Val` anywhere in the crate:
//!
//! ```no_run
//! # use soroban_sdk::{Env, Address};
//! # use lumina_registry_interface::RegistryInterfaceClient;
//! # fn f(env: &Env, registry: &Address, operator: &Address) {
//! let registry = RegistryInterfaceClient::new(env, registry);
//! let profile = registry.get_contract_profile(operator);
//! let _listed = true;
//! let _verified = profile.reputation.verified;
//! # }
//! ```
//!
//! Note *which* method that is. `is_registered` and `is_verified` are two
//! calls; `get_contract_profile` returns both facts in one, and the venue needs
//! the stake and categories anyway. That is not an accident — see below.
//!
//! ## What a cross-contract read costs
//!
//! This is a read, but it is not free, and "it's only a read" is the reasoning
//! that produces a contract with an accidental per-call fee cost. Every call
//! this venue makes into the registry is a nested invocation charged to *this*
//! transaction's resource budget:
//!
//! - a fixed instruction charge for the call, paid before the registry runs
//!   any of its own code;
//! - the ledger entries the registry touches, which are `persistent` entry
//!   reads — the expensive kind. A registration is stored under several keys,
//!   so even a profile read is more than one entry;
//! - a fresh 1 MiB memory allocation for the callee's frame, plus the memory
//!   cost of the arguments going in and the decoded result coming out.
//!
//! `tests/cost.rs` measures this against the real registry wasm rather than
//! estimating it, and asserts the ordering that actually matters: one profile
//! read costs less than the two `is_*` calls it replaces, and every extra call
//! adds a fixed cost on top. Run it with `--nocapture` for the numbers.

use lumina_registry_interface::{
    Category, ContractProfile, RegistryError, RegistryInterfaceClient,
};
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, token, Address, Env, Map, String, Symbol,
    Vec,
};

/// Basis-point denominator. 10_000 bps = 100%.
const BPS_DENOMINATOR: i128 = 10_000;

/// Fee charged when the counterparty is listed but not verified.
const STANDARD_FEE_BPS: u32 = 50;

/// Fee charged when the counterparty is listed, verified, *and* staked.
const VERIFIED_FEE_BPS: u32 = 10;

/// Minimum stake a verified counterparty must post to earn the discount.
///
/// Verification alone is a governance decision that costs the counterparty
/// nothing to obtain. The stake threshold is what makes the discount a
/// skin-in-the-game signal rather than a free badge. The registry reports
/// stakes in its own stake token, so the units here are that token's base
/// units — 1 XLM at 7 decimals for the native asset contract.
const VERIFIED_STAKE_THRESHOLD: i128 = 1_000_000_000;

/// Storage key for the venue's counterparty map.
const COUNTERPARTIES: &str = "counterparties";

/// Errors returned by the example venue.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum VenueError {
    /// The named operator has no registration in the Lumina Registry.
    OperatorNotListed = 1,
    /// The counterparty is already listed on this venue.
    OperatorAlreadyListed = 2,
    /// The named operator is not listed on this venue.
    OperatorNotFound = 3,
    /// The amount deposited was zero or negative.
    InvalidAmount = 4,
    /// The named operator is filed under none of the accepted categories.
    OperatorWrongCategory = 5,
    /// The registry could not be reached, or answered with something this
    /// interface version cannot decode.
    RegistryUnreachable = 6,
}

/// One counterparty as this venue understands it.
///
/// All of it is the venue's own record. The registry is the source of truth
/// about *listing*; the venue copies the decision it actually acted on rather
/// than holding a reference it would have to re-read — and re-pay for — on
/// every later query.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Counterparty {
    /// The operator contract address.
    pub operator: Address,
    /// The name the registry had for it at listing time.
    pub name: String,
    /// Whether the registry had attested it as verified at listing time.
    pub verified: bool,
    /// Its stake in the registry's stake token, at listing time.
    pub stake: i128,
    /// The categories the registry reported at listing time.
    ///
    /// Empty when the venue listed this counterparty with no category policy:
    /// the venue did not ask, so it did not pay for the second read. Treat an
    /// empty list as "unknown", not "none".
    pub categories: Vec<Category>,
    /// The fee this venue charges deposits routed through it, in basis points.
    pub fee_bps: u32,
}

/// Example venue that only takes deposits from counterparties the Lumina
/// Registry lists.
#[contract]
pub struct LuminaListedVenue;

#[contractimpl]
impl LuminaListedVenue {
    /// List a counterparty after checking it against the registry.
    ///
    /// This is the whole integration: one typed client, one method, and the
    /// result pattern-matched into the venue's own error type. A registry that
    /// reverts, or answers with a shape this interface version does not know,
    /// is reported as [`VenueError::RegistryUnreachable`] instead of trapping
    /// the caller with an undecodable error.
    ///
    /// `accepted_categories` is the venue's own policy — "I will only deal
    /// with infrastructure contracts", say — and is not a registry concept.
    /// An empty list accepts any category and, because categories live behind
    /// their own registry read, keeps this to a single cross-contract call.
    pub fn list_counterparty(
        env: Env,
        caller: Address,
        registry: Address,
        operator: Address,
        accepted_categories: Vec<Category>,
    ) -> Result<Counterparty, VenueError> {
        caller.require_auth();

        if Self::counterparties(&env).get(operator.clone()).is_some() {
            return Err(VenueError::OperatorAlreadyListed);
        }

        // ── The typed read ────────────────────────────────────────────────
        //
        // `get_contract_profile` is one call returning the entry *and* the
        // reputation. `is_registered` then `is_verified` would be two calls
        // for a subset of the same answer, and `is_registered` alone would
        // leave the stake unread. See the crate docs for why call count is
        // the thing to optimise.
        let profile = read_profile(&env, &registry, &operator)?;

        // Categories are *not* on `ContractEntry` — the registry stores them
        // under a separate key and exposes them through `get_categories`. So a
        // category policy is a second cross-contract call, and it is made
        // only when there is a policy to enforce. `list_counterparty` with an
        // empty `accepted_categories` therefore costs exactly one read; with a
        // policy it costs two, and the venue records which it paid for.
        let categories = if accepted_categories.is_empty() {
            Vec::new(&env)
        } else {
            let client = RegistryInterfaceClient::new(&env, &registry);
            let categories = client.get_categories(&operator);
            if !contains_any(&categories, &accepted_categories) {
                return Err(VenueError::OperatorWrongCategory);
            }
            categories
        };

        let counterparty = Counterparty {
            operator: operator.clone(),
            name: profile.entry.name,
            verified: profile.reputation.verified,
            stake: profile.reputation.stake,
            categories,
            fee_bps: fee_bps_for(profile.reputation.verified, profile.reputation.stake),
        };

        let mut counterparties = Self::counterparties(&env);
        counterparties.set(operator.clone(), counterparty.clone());
        env.storage()
            .persistent()
            .set(&key(&env, COUNTERPARTIES), &counterparties);

        env.events().publish(
            (Symbol::new(&env, "counterparty_listed"),),
            (operator, counterparty.fee_bps),
        );

        Ok(counterparty)
    }

    /// The counterparty record this venue holds, if it lists one.
    pub fn get_counterparty(env: Env, operator: Address) -> Option<Counterparty> {
        Self::counterparties(&env).get(operator)
    }

    /// The fee a deposit routed through `operator` would pay, in basis points.
    pub fn fee_for(env: Env, operator: Address) -> Result<u32, VenueError> {
        Self::counterparties(&env)
            .get(operator)
            .map(|c| c.fee_bps)
            .ok_or(VenueError::OperatorNotFound)
    }

    /// Take a deposit routed through an already-listed operator, and return the
    /// fee it incurred.
    ///
    /// The counterparty is deliberately **not** re-checked against the registry
    /// here, and that is the cost decision worth noticing. Re-reading on every
    /// deposit would multiply this contract's read cost by its volume, in
    /// exchange for a "still listed right now" guarantee the registry does not
    /// really offer: a *deactivated* registration still exists, so
    /// `is_registered` keeps returning true — only `ContractEntry.active`
    /// changes. A caller that genuinely needs that would read
    /// `get_contract_profile` and check `entry.active`, and should accept that
    /// it is paying for a call per deposit.
    pub fn deposit(
        env: Env,
        caller: Address,
        operator: Address,
        asset: Address,
        amount: i128,
    ) -> Result<i128, VenueError> {
        caller.require_auth();
        if amount <= 0 {
            return Err(VenueError::InvalidAmount);
        }

        let counterparty = Self::counterparties(&env)
            .get(operator.clone())
            .ok_or(VenueError::OperatorNotListed)?;

        token::Client::new(&env, &asset).transfer(
            &caller,
            &env.current_contract_address(),
            &amount,
        );

        let fee = amount
            .checked_mul(counterparty.fee_bps as i128)
            .ok_or(VenueError::InvalidAmount)?
            / BPS_DENOMINATOR;

        env.events().publish(
            (Symbol::new(&env, "deposit_routed"),),
            (caller, operator, amount, counterparty.fee_bps),
        );

        Ok(fee)
    }
}

impl LuminaListedVenue {
    /// The venue's counterparty map. `Map`, not `Vec`, because lookup here is
    /// by operator address and a scan would be a cost bug waiting to happen.
    fn counterparties(env: &Env) -> Map<Address, Counterparty> {
        env.storage()
            .persistent()
            .get(&key(env, COUNTERPARTIES))
            .unwrap_or(Map::new(env))
    }
}

/// One typed cross-contract read, with every failure mode named.
///
/// `try_` is used rather than the plain method so the four outcomes are
/// explicit. The plain `get_contract_profile` panics on all but the first,
/// which for a venue deciding whether to onboard a counterparty is the
/// difference between a clean rejection and a trap:
///
/// - `Ok(Ok(..))` — the registry answered.
/// - `Ok(Err(..))` — it answered, but with a shape this interface version
///   cannot decode. A registry upgraded past this interface.
/// - `Err(Ok(..))` — it returned a declared contract error, normally
///   `ContractNotFound` for an unregistered address.
/// - `Err(Err(..))` — a system-level failure (budget exhausted, bad
///   footprint). Not catchable, and not this contract's to interpret.
fn read_profile(
    env: &Env,
    registry: &Address,
    operator: &Address,
) -> Result<ContractProfile, VenueError> {
    let client = RegistryInterfaceClient::new(env, registry);
    match client.try_get_contract_profile(operator) {
        Ok(Ok(profile)) => Ok(profile),
        Ok(Err(_)) | Err(Err(_)) => Err(VenueError::RegistryUnreachable),
        Err(Ok(error)) => Err(map_registry_error(error)),
    }
}

/// Translate the registry's errors into the venue's.
///
/// Only one error is reachable from this read, but the mapping is written out
/// instead of collapsed into a wildcard: adding a read that can fail
/// differently should be a compile-time decision, not a silent behaviour
/// change.
fn map_registry_error(error: RegistryError) -> VenueError {
    match error {
        RegistryError::ContractNotFound => VenueError::OperatorNotListed,
        _ => VenueError::RegistryUnreachable,
    }
}

/// The fee a counterparty earns, from the reputation the registry reported.
///
/// Both conditions are needed for the discount. Verification on its own is a
/// governance judgement the counterparty pays nothing for; the stake threshold
/// is what makes the badge skin-in-the-game.
fn fee_bps_for(verified: bool, stake: i128) -> u32 {
    if verified && stake >= VERIFIED_STAKE_THRESHOLD {
        VERIFIED_FEE_BPS
    } else {
        STANDARD_FEE_BPS
    }
}

/// `soroban_sdk::Vec::contains` exists, but there is no `any` over a second
/// vector, and the obvious loop with a mutable flag does not borrow-check
/// cleanly inside the SDK's iterator.
fn contains_any(haystack: &Vec<Category>, needles: &Vec<Category>) -> bool {
    for candidate in needles.iter() {
        if haystack.contains(candidate) {
            return true;
        }
    }
    false
}

/// `symbol_short!` caps at 9 characters, which `counterparties` is not.
fn key(env: &Env, name: &str) -> Symbol {
    Symbol::new(env, name)
}
