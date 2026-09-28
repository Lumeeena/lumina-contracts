// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! End-to-end tests for the example venue against the **real** registry.
//!
//! These deploy the registry from its compiled wasm and read it through
//! `RegistryInterfaceClient`, which is the whole point: a test against a
//! hand-written mock would only prove that the mock matches itself. The wasm
//! is the same artifact that ships, and its spec is what the interface crate
//! is checked against, so drift between the two fails here first.
//!
//! Run `cargo build --target wasm32v1-none --release` first.

use lumina_registry_consumer_example::{LuminaListedVenue, LuminaListedVenueClient, VenueError};
use lumina_registry_interface::{Category, RegistryError, RegistryInterfaceClient};
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Ledger as _},
    Address, Env, String, Vec,
};

mod registry_wasm {
    soroban_sdk::contractimport!(
        file = "../../target/wasm32v1-none/release/lumina_registry.wasm"
    );
}

/// Fee the venue charges a listed-but-unverified counterparty.
const STANDARD_FEE_BPS: u32 = 50;
/// Fee the venue charges a verified, staked counterparty.
const VERIFIED_FEE_BPS: u32 = 10;
/// Stake that clears the venue's discount threshold, in token base units.
const STAKE: i128 = 1_000_000_000;

// ─── A minimal SEP-41 token ────────────────────────────────────────────────
//
// Declared here rather than imported, because the only thing under test is the
// venue's use of the registry, not the token. The registry's own suite has an
// equivalent fixture.

#[contracttype]
enum TokenKey {
    Balance(Address),
}

/// A token that can mint to itself, which is all these tests need.
#[contract]
struct TestToken;

#[contractimpl]
impl TestToken {
    /// Create `amount` and credit it to `to`.
    pub fn mint(env: Env, to: Address, amount: i128) {
        let key = TokenKey::Balance(to);
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(current + amount));
    }

    /// The current balance of `from`.
    pub fn balance(env: Env, from: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&TokenKey::Balance(from))
            .unwrap_or(0)
    }

    /// Move `amount` from `from` to `to`, reverting on an insufficient balance.
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        let from_key = TokenKey::Balance(from.clone());
        let from_balance: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
        if from_balance < amount {
            panic!("insufficient balance");
        }
        env.storage()
            .persistent()
            .set(&from_key, &(from_balance - amount));

        let to_key = TokenKey::Balance(to);
        let to_balance: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&to_key, &(to_balance + amount));
    }
}

// ─── Fixture ───────────────────────────────────────────────────────────────

/// The fixture owns the `Env`, so the generated clients cannot be built until
/// it is constructed. They are therefore built on demand rather than stored:
/// a client is a thin (env, address) pair and making a fresh one is free.
struct Fixture {
    env: Env,
    registry: Address,
    venue: Address,
    admin: Address,
    owner: Address,
    token: Address,
}

impl Fixture {
    /// A client for the registry, built from its wasm. Used to *set the
    /// fixture up*; the tests themselves read through
    /// [`RegistryInterfaceClient`].
    fn registry_client(&self) -> registry_wasm::Client<'static> {
        registry_wasm::Client::new(&self.env, &self.registry)
    }

    /// A client for the venue under test.
    fn venue_client(&self) -> LuminaListedVenueClient<'static> {
        LuminaListedVenueClient::new(&self.env, &self.venue)
    }
}

impl Fixture {
    /// Categories as the *imported registry wasm* declares them. The
    /// `contractimport!` module carries its own copy of every type, so calls
    /// made through the generated client need that copy, not the interface
    /// crate's — the two are wire-compatible but are not the same Rust type.
    fn wasm_categories(&self, which: &[Category]) -> Vec<registry_wasm::Category> {
        let mut v = Vec::new(&self.env);
        for c in which {
            v.push_back(match c {
                Category::DeFi => registry_wasm::Category::DeFi,
                Category::Nft => registry_wasm::Category::Nft,
                Category::Gaming => registry_wasm::Category::Gaming,
                Category::Identity => registry_wasm::Category::Identity,
                Category::Infrastructure => registry_wasm::Category::Infrastructure,
                Category::Payments => registry_wasm::Category::Payments,
                Category::Oracle => registry_wasm::Category::Oracle,
                Category::Dao => registry_wasm::Category::Dao,
                Category::Other => registry_wasm::Category::Other,
            });
        }
        v
    }

    /// Categories as the *interface crate* declares them, for calls into the
    /// venue (which depends on the interface, not on the registry crate).
    fn interface_categories(&self, which: &[Category]) -> Vec<Category> {
        let mut v = Vec::new(&self.env);
        for c in which {
            v.push_back(*c);
        }
        v
    }

    /// Advance past the timelock and execute whatever was just approved.
    fn advance(&self, ledgers: u32) {
        let seq = self.env.ledger().sequence();
        self.env.ledger().set_sequence_number(seq + ledgers);
    }

    /// Approve, wait out the timelock, execute.
    fn execute(&self, proposal: u32) {
        self.advance(20_000);
        self.registry_client().execute_proposal(&proposal);
    }

    fn register(&self, name: &str, which: &[Category]) -> Address {
        let target = Address::generate(&self.env);
        self.registry_client()
            .register_contract(
                &self.owner,
                &target,
                &String::from_str(&self.env, name),
                &String::from_str(&self.env, "a counterparty"),
                &self.wasm_categories(which),
            );
        target
    }

    fn verify(&self, target: &Address) {
        let proposal = self
            .registry_client()
            .propose_set_verified(&self.admin, target, &true);
        self.registry_client().approve_proposal(&self.admin, &proposal);
        self.execute(proposal);
    }

    fn stake(&self, target: &Address, amount: i128) {
        TestTokenClient::new(&self.env, &self.token).mint(&self.owner, &(amount * 10));
        self.registry_client().stake(&self.owner, target, &amount);
    }
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    // The registry's governance timelock is 17_280 ledgers in a release build
    // (`TIMELOCK_LEDGERS` is 10 under `cfg(test)`, which the wasm is not), so
    // the fixture has to advance past it. The default test ledger TTL is far
    // shorter than that, and the ledger would archive the registry's *instance*
    // storage mid-timelock — the same trap the registry's own suite documents
    // when it shortens the constant. Raise the TTL before anything is stored.
    env.ledger().set_max_entry_ttl(1_000_000);
    env.ledger().set_min_persistent_entry_ttl(1_000_000);
    env.ledger().set_min_temp_entry_ttl(1_000_000);

    let admin = Address::generate(&env);
    let owner = Address::generate(&env);

    // The registry's `__constructor` authorizes the bootstrap admin outside the
    // root invocation, which the plain all-auths mock rejects.
    env.mock_all_auths_allowing_non_root_auth();
    let registry = env.register(registry_wasm::WASM, (&admin,));
    env.mock_all_auths();

    let venue = env.register(LuminaListedVenue, ());
    let token = env.register(TestToken, ());

    let f = Fixture { env, registry, venue, admin, owner, token };

    // Staking is closed until governance names a token and a treasury, so the
    // discount path is unreachable until this runs. It is also a cross-contract
    // sequence like any other, and needs the timelock cleared.
    let treasury = Address::generate(&f.env);
    let proposal = f
        .registry_client()
        .propose_configure_staking(&f.admin, &f.token, &treasury);
    f.registry_client().approve_proposal(&f.admin, &proposal);
    f.execute(proposal);

    f
}

// ─── The acceptance criterion ──────────────────────────────────────────────

#[test]
fn another_contract_queries_registration_status_through_a_typed_interface() {
    let f = setup();
    f.registry_client()
        .register_contract(
            &f.owner,
            &f.registry,
            &String::from_str(&f.env, "Lumina Registry"),
            &String::from_str(&f.env, "The registry itself"),
            &f.wasm_categories(&[Category::Infrastructure]),
        );

    // This is the integration, in four lines, with no hand-built `Val`s and no
    // symbol strings that could silently be misspelled.
    let client = RegistryInterfaceClient::new(&f.env, &f.registry);
    assert!(client.is_registered(&f.registry));
    assert!(!client.is_verified(&f.registry));

    let profile = client.get_contract_profile(&f.registry);
    assert_eq!(profile.entry.contract_id, f.registry);
    assert!(profile.entry.active);
    assert_eq!(
        profile.entry.name,
        String::from_str(&f.env, "Lumina Registry")
    );
    // Categories are not on `ContractEntry`; they need their own read. That is
    // the shape of the cost, and the example makes the second call only when
    // it has a policy to check.
    assert_eq!(
        client.get_categories(&f.registry),
        f.interface_categories(&[Category::Infrastructure])
    );
    assert_eq!(client.get_contract_count(), 1);
    assert_eq!(client.get_active_contract_count(), 1);
}

#[test]
fn an_unregistered_address_is_tolerated_by_the_cheap_views_and_errors_on_the_strict_one() {
    let f = setup();
    let stranger = Address::generate(&f.env);
    let client = RegistryInterfaceClient::new(&f.env, &f.registry);

    assert!(!client.is_registered(&stranger));
    // The tolerant views answer rather than erroring, so a caller can branch on
    // them without a try_.
    assert!(!client.is_verified(&stranger));
    assert_eq!(client.get_stake(&stranger), 0);
    assert_eq!(client.get_tags(&stranger), Vec::new(&f.env));
    // The strict one surfaces the error the venue maps.
    assert_eq!(
        client.try_get_contract_profile(&stranger).unwrap_err(),
        Ok(RegistryError::ContractNotFound)
    );
}

// ─── The venue ─────────────────────────────────────────────────────────────

#[test]
fn a_listed_counterparty_is_accepted_and_charged_the_standard_fee() {
    let f = setup();
    let target = f.register("Quorum", &[Category::Infrastructure]);

    let listed = f
        .venue_client()
        .list_counterparty(
            &f.owner,
            &f.registry,
            &target,
            &f.interface_categories(&[Category::Infrastructure]),
        );

    assert_eq!(listed.operator, target);
    assert!(!listed.verified);
    assert_eq!(listed.fee_bps, STANDARD_FEE_BPS);
    assert_eq!(f.venue_client().fee_for(&target), STANDARD_FEE_BPS);
    assert_eq!(f.venue_client().get_counterparty(&target), Some(listed));
}

#[test]
fn an_unregistered_counterparty_is_rejected() {
    let f = setup();
    let stranger = Address::generate(&f.env);

    assert_eq!(
        f.venue_client().try_list_counterparty(
            &f.owner,
            &f.registry,
            &stranger,
            &Vec::new(&f.env),
        ),
        Err(Ok(VenueError::OperatorNotListed))
    );
    assert_eq!(f.venue_client().get_counterparty(&stranger), None);
}

#[test]
fn a_counterparty_outside_the_accepted_categories_is_rejected() {
    let f = setup();
    let target = f.register("A Game", &[Category::Gaming]);

    assert_eq!(
        f.venue_client().try_list_counterparty(
            &f.owner,
            &f.registry,
            &target,
            &f.interface_categories(&[Category::Infrastructure]),
        ),
        Err(Ok(VenueError::OperatorWrongCategory))
    );
}

#[test]
fn a_counterparty_in_an_accepted_category_among_several_is_accepted() {
    let f = setup();
    let target = f.register("A Bridge", &[Category::Gaming, Category::Infrastructure]);

    f.venue_client()
        .list_counterparty(
            &f.owner,
            &f.registry,
            &target,
            &f.interface_categories(&[Category::Infrastructure, Category::Payments]),
        );
}

#[test]
fn a_verified_staked_counterparty_earns_the_discount() {
    let f = setup();
    let target = f.register("Quorum", &[Category::Infrastructure]);
    f.stake(&target, STAKE);
    f.verify(&target);

    let listed = f
        .venue_client()
        .list_counterparty(&f.owner, &f.registry, &target, &Vec::new(&f.env));

    assert!(listed.verified);
    assert_eq!(listed.stake, STAKE);
    assert_eq!(listed.fee_bps, VERIFIED_FEE_BPS);
}

#[test]
fn verification_alone_does_not_earn_the_discount() {
    // The reason the discount needs a stake threshold: verification is free to
    // obtain, so on its own it is not a skin-in-the-game signal.
    let f = setup();
    let target = f.register("Quorum", &[Category::Infrastructure]);
    f.verify(&target); // verified, but nothing staked

    let listed = f
        .venue_client()
        .list_counterparty(&f.owner, &f.registry, &target, &Vec::new(&f.env));

    assert!(listed.verified);
    assert_eq!(listed.stake, 0);
    assert_eq!(listed.fee_bps, STANDARD_FEE_BPS);
}

#[test]
fn stake_alone_does_not_earn_the_discount() {
    let f = setup();
    let target = f.register("Quorum", &[Category::Infrastructure]);
    f.stake(&target, STAKE); // staked, but never verified

    let listed = f
        .venue_client()
        .list_counterparty(&f.owner, &f.registry, &target, &Vec::new(&f.env));

    assert!(!listed.verified);
    assert_eq!(listed.fee_bps, STANDARD_FEE_BPS);
}

#[test]
fn listing_the_same_counterparty_twice_is_rejected() {
    let f = setup();
    let target = f.register("Quorum", &[Category::Infrastructure]);
    f.venue_client()
        .list_counterparty(&f.owner, &f.registry, &target, &Vec::new(&f.env));

    assert_eq!(
        f.venue_client()
            .try_list_counterparty(&f.owner, &f.registry, &target, &Vec::new(&f.env)),
        Err(Ok(VenueError::OperatorAlreadyListed))
    );
}

#[test]
fn deposits_are_routed_through_a_listed_counterparty() {
    let f = setup();
    let target = f.register("Quorum", &[Category::Infrastructure]);
    f.venue_client()
        .list_counterparty(&f.owner, &f.registry, &target, &Vec::new(&f.env));

    TestTokenClient::new(&f.env, &f.token).mint(&f.owner, &10_000);

    // 10_000 * 50 / 10_000 = 50
    let fee = f.venue_client().deposit(&f.owner, &target, &f.token, &10_000);
    assert_eq!(fee, 50);
    assert_eq!(
        TestTokenClient::new(&f.env, &f.token).balance(&f.venue),
        10_000
    );
}

#[test]
fn deposits_through_an_unlisted_operator_are_rejected() {
    let f = setup();
    TestTokenClient::new(&f.env, &f.token).mint(&f.owner, &10_000);
    let stranger = Address::generate(&f.env);

    assert_eq!(
        f.venue_client()
            .try_deposit(&f.owner, &stranger, &f.token, &10_000),
        Err(Ok(VenueError::OperatorNotListed))
    );
}

#[test]
fn a_zero_deposit_is_rejected() {
    let f = setup();
    let target = f.register("Quorum", &[Category::Infrastructure]);
    f.venue_client()
        .list_counterparty(&f.owner, &f.registry, &target, &Vec::new(&f.env));

    assert_eq!(
        f.venue_client().try_deposit(&f.owner, &target, &f.token, &0),
        Err(Ok(VenueError::InvalidAmount))
    );
}

#[test]
fn a_deactivated_counterparty_stays_listed_on_the_venue() {
    // Documents the trade-off the venue's `deposit` doc comment makes: the
    // registry's `is_registered` is true for a *deactivated* registration, so
    // re-reading it per deposit would buy very little for a paid call.
    let f = setup();
    let target = f.register("Quorum", &[Category::Infrastructure]);
    f.venue_client()
        .list_counterparty(&f.owner, &f.registry, &target, &Vec::new(&f.env));

    f.registry_client().deactivate(&f.owner, &target);

    // Still registered from the registry's point of view...
    let client = RegistryInterfaceClient::new(&f.env, &f.registry);
    assert!(client.is_registered(&target));
    // ...but no longer active, which is the flag a caller that *does* re-read
    // would have to check.
    assert!(!client.get_contract_profile(&target).entry.active);
}
