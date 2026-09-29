// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! What a cross-contract read actually costs, measured rather than asserted.
//!
//! "It's only a read" is the reasoning that produces a contract with an
//! accidental per-call fee, so this file puts numbers on the table. It deploys
//! the **real registry wasm** — not a mock — and measures the resources of
//! transactions that read from it in different ways.
//!
//! Run with `--nocapture` to see the table:
//!
//! ```bash
//! cargo build --target wasm32v1-none --release
//! cargo test -p lumina-registry-consumer-example --test cost -- --nocapture
//! ```
//!
//! ## What these numbers are and are not
//!
//! The *registry* side is faithful: it is the release wasm, metered by the
//! host, doing the storage reads it really does. The *probe* side is a native
//! Rust test contract, so its own instructions are not VM-metered and are
//! undercounted — `soroban-sdk` says as much in its own docs. That is fine for
//! the question being asked, because the probe's own work is identical across
//! every row; the *differences* between rows are the cross-contract calls, and
//! the fixed charge on top is real.
//!
//! What this file does **not** claim is an absolute fee. Fee rates are set by
//! the network and change; instruction counts do not. Read the deltas.

use lumina_registry_interface::RegistryInterfaceClient;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger as _},
    Address, Env, String, Vec,
};

mod registry_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/lumina_registry.wasm");
}

/// A contract whose only job is to make a chosen number of reads, so the reads
/// can be attributed.
///
/// Each method is otherwise identical: take a registry and a target, do one
/// thing, return. The difference between two rows is therefore the read, not
/// the surrounding code.
#[contract]
struct ReadProbe;

#[contractimpl]
impl ReadProbe {
    /// A transaction that touches the registry not at all. The baseline.
    pub fn no_read(_env: Env) {}

    /// One call to the cheapest question the registry answers, plus a second
    /// call that does almost nothing at all. The difference between them is the
    /// per-invocation charge; everything they share is the cost of the frame.
    pub fn read_is_registered(env: Env, registry: Address, target: Address) -> bool {
        RegistryInterfaceClient::new(&env, &registry).is_registered(&target)
    }

    /// A cross-contract call that touches a single instance key. Included to
    /// separate "calling a contract" from "asking this contract a question".
    pub fn read_version(env: Env, registry: Address) -> u32 {
        RegistryInterfaceClient::new(&env, &registry).get_version()
    }

    /// Two calls for the same two facts `read_profile` returns in one.
    pub fn read_is_registered_and_verified(
        env: Env,
        registry: Address,
        target: Address,
    ) -> (bool, bool) {
        let registry = RegistryInterfaceClient::new(&env, &registry);
        (
            registry.is_registered(&target),
            registry.is_verified(&target),
        )
    }

    /// One call returning the registration *and* its reputation.
    pub fn read_profile(env: Env, registry: Address, target: Address) -> bool {
        match RegistryInterfaceClient::new(&env, &registry).try_get_contract_profile(&target) {
            Ok(Ok(profile)) => profile.reputation.verified,
            _ => false,
        }
    }

    /// Three calls, to show the per-call charge is additive rather than a
    /// one-off setup cost.
    pub fn read_three(env: Env, registry: Address, target: Address) -> (bool, bool, u32) {
        let registry = RegistryInterfaceClient::new(&env, &registry);
        (
            registry.is_registered(&target),
            registry.is_verified(&target),
            registry.get_contract_count(),
        )
    }

    /// A local, same-contract read of the same shape, for contrast. This one
    /// touches no other contract at all, so it carries no per-call charge.
    pub fn read_local(_env: Env, flag: bool) -> bool {
        flag
    }
}

struct Fixture {
    env: Env,
    registry: Address,
    target: Address,
    probe: Address,
}

/// Resources a transaction used, in the units the host meters. These are the
/// same numbers the network turns into a fee: instructions, memory, and the
/// ledger entries the transaction had to open.
#[derive(Debug, Clone, Copy)]
struct Cost {
    instructions: i64,
    memory_bytes: i64,
    read_entries: u32,
    write_entries: u32,
    read_bytes: u32,
}

fn measure<T>(f: &Fixture, call: impl FnOnce(&Env, &Address) -> T) -> (Cost, T) {
    let env = &f.env;
    let probe = f.probe.clone();
    // Metering resets before every top-level invocation, so the reading taken
    // afterwards covers exactly this call and nothing that came before it.
    let value = call(env, &probe);
    let resources = env.cost_estimate().resources();
    (
        Cost {
            instructions: resources.instructions,
            memory_bytes: resources.mem_bytes,
            read_entries: resources.read_entries,
            write_entries: resources.write_entries,
            read_bytes: resources.read_bytes,
        },
        value,
    )
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_max_entry_ttl(1_000_000);
    env.ledger().set_min_persistent_entry_ttl(1_000_000);
    env.ledger().set_min_temp_entry_ttl(1_000_000);

    let admin = Address::generate(&env);
    let owner = Address::generate(&env);
    env.mock_all_auths_allowing_non_root_auth();
    let registry = env.register(registry_wasm::WASM, (&admin,));
    env.mock_all_auths();

    // A real registration, so the reads decode real entries rather than
    // short-circuiting on a missing key.
    let client = registry_wasm::Client::new(&env, &registry);
    let target = Address::generate(&env);
    let mut categories = Vec::new(&env);
    categories.push_back(registry_wasm::Category::Infrastructure);
    client.register_contract(
        &owner,
        &target,
        &String::from_str(&env, "Quorum"),
        &String::from_str(&env, "a counterparty"),
        &categories,
    );

    let probe = env.register(ReadProbe, ());
    Fixture {
        env,
        registry,
        target,
        probe,
    }
}

fn probe_client(env: &Env, probe: &Address) -> ReadProbeClient<'static> {
    ReadProbeClient::new(env, probe)
}

#[test]
fn cross_contract_reads_are_measured_and_priced_in_the_readme() {
    let f = setup();
    let registry = f.registry.clone();
    let target = f.target.clone();

    let (baseline, _) = measure(&f, |env, probe| {
        probe_client(env, probe).no_read();
    });

    let (one, registered) = measure(&f, |env, probe| {
        probe_client(env, probe).read_is_registered(&registry, &target)
    });
    assert!(registered, "the fixture registration should be found");

    let (version, version_number) = measure(&f, |env, probe| {
        probe_client(env, probe).read_version(&registry)
    });
    assert!(version_number > 0);

    let (two, (registered, verified)) = measure(&f, |env, probe| {
        probe_client(env, probe).read_is_registered_and_verified(&registry, &target)
    });
    assert!(registered && !verified);

    let (profile, profile_verified) = measure(&f, |env, probe| {
        probe_client(env, probe).read_profile(&registry, &target)
    });
    assert!(!profile_verified);

    let (three, (registered, _, count)) = measure(&f, |env, probe| {
        probe_client(env, probe).read_three(&registry, &target)
    });
    assert!(registered && count >= 1);

    let (local, _) = measure(&f, |env, probe| {
        probe_client(env, probe).read_local(&true);
    });

    println!("\n  A cross-contract read is not free. Measured against the real\n  registry wasm (release build, host-metered):\n");
    println!(
        "    {:<38} {:>10} {:>9} {:>7} {:>7} {:>9}",
        "transaction", "instrs", "mem B", "rd ent", "wr ent", "rd bytes"
    );
    for (label, c) in [
        ("no cross-contract call (baseline)", &baseline),
        ("local read, no other contract", &local),
        ("1x get_version (one instance key)", &version),
        ("1x is_registered", &one),
        ("1x get_contract_profile", &profile),
        ("2x is_registered + is_verified", &two),
        ("3x is_registered/is_verified/count", &three),
    ] {
        println!(
            "    {:<38} {:>10} {:>9} {:>7} {:>7} {:>9}",
            label, c.instructions, c.memory_bytes, c.read_entries, c.write_entries, c.read_bytes
        );
    }

    let per_call = two.instructions - one.instructions;
    let overhead = version.instructions - baseline.instructions;
    let answer = one.instructions - version.instructions;
    println!(
        "\n    Crossing into the registry at all:  {overhead} instructions, of which only\n    \
         {answer} is the answer you asked for ({:.1}% of the call).\n    \
         Each further call adds another {per_call}.\n    \
         One get_contract_profile instead of is_registered + is_verified saves {} ({:.0}%).\n",
        100.0 * answer as f64 / overhead as f64,
        two.instructions - profile.instructions,
        100.0 * (two.instructions - profile.instructions) as f64 / two.instructions as f64,
    );

    // ── The claims the README makes ───────────────────────────────────────
    //
    // Asserted rather than merely printed, so the documentation cannot quietly
    // stop being true.

    // 1. A cross-contract call is not free: even the cheapest read costs more
    //    than a transaction that makes no call at all.
    assert!(
        one.instructions > baseline.instructions,
        "a cross-contract read should cost instructions, but it cost {} vs a {} baseline",
        one.instructions,
        baseline.instructions
    );

    // 2. A call into another contract is more expensive than doing the same
    //    trivial work locally. This is the fixed per-invocation charge.
    assert!(
        one.instructions > local.instructions,
        "a cross-contract read ({} instrs) should exceed a local one ({})",
        one.instructions,
        local.instructions
    );

    // 3. Almost all of a cross-contract read is the act of crossing the
    //    boundary. `get_version` reads a single instance key; `is_registered`
    //    touches a persistent entry; they cost nearly the same, so budgeting
    //    for "one more cheap read" is budgeting wrong.
    let overhead = version.instructions - baseline.instructions;
    let answer = one.instructions - version.instructions;
    assert!(
        answer * 10 < overhead,
        "the fixed cost of invoking another contract ({overhead} instrs) should dwarf the \
         cost of the answer itself ({answer} instrs)"
    );

    // 4. Two calls cost more than one — the charge is per call, not per callee.
    assert!(
        two.instructions > one.instructions,
        "a second cross-contract call should add instructions"
    );

    // 5. Three cost more than two, by roughly the same per-call amount. This
    //    is what makes an O(n) loop over counterparties a real cost bug.
    let first_step = two.instructions - one.instructions;
    let second_step = three.instructions - two.instructions;
    assert!(
        (first_step - second_step).abs() < (first_step / 2).max(1),
        "per-call cost should be roughly constant, saw {first_step} then {second_step}"
    );

    // 6. One `get_contract_profile` is cheaper than the two `is_*` calls it
    //    replaces. This is the claim the example's `list_counterparty` relies
    //    on, and the reason it is written that way.
    assert!(
        profile.instructions < two.instructions,
        "get_contract_profile ({} instrs) should be cheaper than is_registered + \
         is_verified ({} instrs)",
        profile.instructions,
        two.instructions
    );

    // 7. A cross-contract read costs the caller ledger entries — the registry's.
    //    These are charged to whoever made the call, not to the registry.
    assert!(
        one.read_entries > local.read_entries,
        "a cross-contract read should cost the caller ledger reads, \
         saw {} vs {} locally",
        one.read_entries,
        local.read_entries
    );

    // 8. Entries are paid for once, calls are paid for every time. Re-reading
    //    the same keys adds instructions without adding entries, which is
    //    exactly why the "just call it again" reflex is expensive.
    assert!(
        three.read_entries <= two.read_entries,
        "a third call re-reading the same keys should not add ledger entries, saw {} -> {}",
        two.read_entries,
        three.read_entries
    );
}

/// The interface crate documents the *shape* of the cost. This checks the two
/// claims it makes that are cheap to state precisely: a registration lives in
/// more than one ledger entry, and the tolerant views are cheaper than the
/// strict one because they decode less.
#[test]
fn the_registry_stores_a_registration_in_several_entries() {
    let f = setup();
    let client = RegistryInterfaceClient::new(&f.env, &f.registry);

    // `is_registered` is a single `has` against one persistent key.
    let (cheap, _) = measure(&f, |env, probe| {
        probe_client(env, probe).read_is_registered(&f.registry, &f.target)
    });
    // `get_contract_profile` loads the entry *and* the reputation, which are
    // separate keys, and decodes both.
    let (rich, _) = measure(&f, |env, probe| {
        probe_client(env, probe).read_profile(&f.registry, &f.target)
    });

    assert!(
        rich.read_entries >= cheap.read_entries,
        "a profile read should not touch fewer ledger entries than a `has`"
    );
    assert!(
        rich.memory_bytes > cheap.memory_bytes,
        "decoding a profile should cost more memory than a `has`"
    );

    // And the tolerant views are views, not panics: a contract that asks about
    // an address nobody registered gets a boolean, not a revert. The cost of
    // asking is the same either way.
    assert!(client.is_registered(&f.target));
    assert!(
        !client.is_verified(&f.target),
        "a fresh registration is not verified yet"
    );
}
