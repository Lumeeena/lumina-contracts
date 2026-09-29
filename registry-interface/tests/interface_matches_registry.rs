// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Binds `RegistryInterface` to what the registry contract actually exports.
//!
//! The interface crate re-declares the registry's types rather than importing
//! them, because depending on the contract crate would drag the registry's
//! whole `#[contractimpl]` into every consumer's wasm. That trade is only safe
//! while the two declarations agree, and nothing in the type system checks it:
//! renaming a field in the contract compiles fine here and turns into a decode
//! failure at run time, inside somebody else's contract.
//!
//! So this test reads the contract spec out of the registry's *built* wasm —
//! the same `contractspecv0` section `stellar contract bindings` and
//! `contractimport!` consume — and asserts that every function, type and error
//! code declared in this crate is present there with the same shape. A change
//! to the registry that is not mirrored here fails the test, naming the
//! signature that moved.
//!
//! It complements rather than duplicates `registry/tests/interface.rs`: that
//! one guards the contract against unreviewed change, this one guards the
//! *published interface* against the contract.
//!
//! Run the wasm build first:
//!
//! ```bash
//! cargo build --target wasm32v1-none --release && cargo test -p lumina-registry-interface
//! ```

use lumina_registry_interface::{
    Category, ContractEntry, ContractPage, ContractProfile, ContractProfilePage, RegistryError,
    RegistryInterfaceClient, RegistryStats, Reputation, SlashRecord,
};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::xdr::{ScSpecEntry, ScSpecTypeDef, ScSpecUdtUnionCaseV0};
use soroban_sdk::Address;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The registry wasm this interface is written against.
fn registry_wasm() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop(); // registry-interface/
    path.push("target/wasm32v1-none/release/lumina_registry.wasm");
    path
}

fn render_type(ty: &ScSpecTypeDef) -> String {
    match ty {
        ScSpecTypeDef::Option(o) => format!("Option<{}>", render_type(&o.value_type)),
        ScSpecTypeDef::Result(r) => format!(
            "Result<{}, {}>",
            render_type(&r.ok_type),
            render_type(&r.error_type)
        ),
        ScSpecTypeDef::Vec(v) => format!("Vec<{}>", render_type(&v.element_type)),
        ScSpecTypeDef::Map(m) => format!(
            "Map<{}, {}>",
            render_type(&m.key_type),
            render_type(&m.value_type)
        ),
        ScSpecTypeDef::Tuple(t) => format!(
            "({})",
            t.value_types
                .iter()
                .map(render_type)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ScSpecTypeDef::BytesN(b) => format!("BytesN<{}>", b.n),
        ScSpecTypeDef::Udt(u) => u.name.to_utf8_string_lossy(),
        leaf => leaf.name().to_string(),
    }
}

/// Every exported item of the registry's spec, keyed by name.
struct Spec {
    functions: BTreeMap<String, String>,
    structs: BTreeMap<String, String>,
    unions: BTreeMap<String, String>,
    errors: BTreeMap<String, String>,
}

fn load_spec() -> Spec {
    let path = registry_wasm();
    let wasm = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {} ({e}). Run `cargo build --target wasm32v1-none --release` first.",
            path.display()
        )
    });
    let entries = soroban_spec::read::from_wasm(&wasm).expect("wasm has no readable contract spec");

    let mut spec = Spec {
        functions: BTreeMap::new(),
        structs: BTreeMap::new(),
        unions: BTreeMap::new(),
        errors: BTreeMap::new(),
    };

    for entry in entries {
        match entry {
            ScSpecEntry::FunctionV0(f) => {
                let args = f
                    .inputs
                    .iter()
                    .map(|i| {
                        format!(
                            "{}: {}",
                            i.name.to_utf8_string_lossy(),
                            render_type(&i.type_)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let ret = match f.outputs.first() {
                    Some(out) => format!(" -> {}", render_type(out)),
                    None => String::new(),
                };
                spec.functions
                    .insert(f.name.to_utf8_string_lossy(), format!("({}){}", args, ret));
            }
            ScSpecEntry::UdtStructV0(s) => {
                let fields = s
                    .fields
                    .iter()
                    .map(|f| {
                        format!(
                            "{}: {}",
                            f.name.to_utf8_string_lossy(),
                            render_type(&f.type_)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                spec.structs
                    .insert(s.name.to_utf8_string_lossy(), format!("{{{}}}", fields));
            }
            // No value enums exist on this contract: a payload-free
            // `#[contracttype] enum` is emitted as a union of void cases, and
            // every real enum here carries payloads.
            ScSpecEntry::UdtEnumV0(e) => {
                panic!(
                    "unexpected value enum `{}` in the registry spec",
                    e.name.to_utf8_string_lossy()
                );
            }
            ScSpecEntry::UdtUnionV0(u) => {
                let cases = u
                    .cases
                    .iter()
                    .map(|c| match c {
                        ScSpecUdtUnionCaseV0::VoidV0(v) => v.name.to_utf8_string_lossy(),
                        ScSpecUdtUnionCaseV0::TupleV0(t) => format!(
                            "{}({})",
                            t.name.to_utf8_string_lossy(),
                            t.type_
                                .iter()
                                .map(render_type)
                                .collect::<Vec<_>>()
                                .join(",")
                        ),
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                spec.unions.insert(u.name.to_utf8_string_lossy(), cases);
            }
            ScSpecEntry::UdtErrorEnumV0(e) => {
                let cases = e
                    .cases
                    .iter()
                    .map(|c| format!("{}={}", c.name.to_utf8_string_lossy(), c.value))
                    .collect::<Vec<_>>()
                    .join(",");
                spec.errors.insert(e.name.to_utf8_string_lossy(), cases);
            }
        }
    }

    spec
}

/// The read-only surface, written down: `(name, arguments, return type)` in the
/// exact spelling the registry's spec uses.
///
/// Deliberately transcribed rather than reflected from the trait. The point of
/// this file is to compare two things that were written down independently — the
/// contract's spec and the published interface — and a test that read the trait
/// back out of the source would only prove the trait agrees with itself. So this
/// table is the third written-down artifact, and
/// `the_published_trait_declares_exactly_this_surface` checks the two against
/// each other.
const READ_ONLY_SURFACE: [(&str, &str, &str); 30] = [
    ("get_version", "", "U32"),
    ("get_admin", "", "Result<Address, RegistryError>"),
    ("get_admins", "", "Result<Vec<Address>, RegistryError>"),
    ("get_threshold", "", "Result<U32, RegistryError>"),
    (
        "get_proposal",
        "proposal_id: U32",
        "Result<Proposal, RegistryError>",
    ),
    ("get_categories", "contract_id: Address", "Vec<Category>"),
    ("get_tags", "contract_id: Address", "Vec<String>"),
    (
        "get_active_contracts_by_category",
        "category: Category, offset: U32, limit: U32",
        "Vec<ContractEntry>",
    ),
    (
        "get_contracts_by_categories",
        "categories: Vec<Category>, offset: U32, limit: U32",
        "Result<Vec<ContractEntry>, RegistryError>",
    ),
    ("get_minimum_stake", "", "I128"),
    (
        "get_staking_config",
        "",
        "Result<(Address, Address), RegistryError>",
    ),
    ("get_registration_fee", "", "I128"),
    ("get_stake", "contract_id: Address", "I128"),
    ("is_verified", "contract_id: Address", "Bool"),
    ("is_registered", "contract_id: Address", "Bool"),
    ("get_registry_stats", "", "RegistryStats"),
    ("get_slashes", "contract_id: Address", "Vec<SlashRecord>"),
    (
        "get_attestations",
        "contract_id: Address",
        "Vec<Attestation>",
    ),
    ("get_reputation", "contract_id: Address", "Reputation"),
    (
        "get_contract_profile",
        "contract_id: Address",
        "Result<ContractProfile, RegistryError>",
    ),
    (
        "get_active_profiles",
        "offset: U32, limit: U32",
        "Vec<ContractProfile>",
    ),
    (
        "get_contract",
        "contract_id: Address",
        "Result<ContractEntry, RegistryError>",
    ),
    ("get_contract_count", "", "U32"),
    ("get_total_registered", "", "U32"),
    ("get_active_contract_count", "", "U32"),
    (
        "get_active_contracts",
        "offset: U32, limit: U32",
        "Vec<ContractEntry>",
    ),
    (
        "get_active_contract_ids",
        "offset: U32, limit: U32",
        "Vec<Address>",
    ),
    (
        "get_active_contracts_page",
        "offset: U32, limit: U32",
        "ContractPage",
    ),
    (
        "get_active_profiles_page",
        "offset: U32, limit: U32",
        "ContractProfilePage",
    ),
    (
        "get_contracts_by_owner",
        "owner: Address, offset: U32, limit: U32",
        "Vec<ContractEntry>",
    ),
];

/// The signature a spec entry must have, spelled the way `load_spec` spells it.
fn signature(args: &str, ret: &str) -> String {
    format!("({args}) -> {ret}")
}

/// Assert the registry exports a function with this exact argument list and
/// return type.
fn assert_function(spec: &Spec, name: &str, args: &str, ret: &str) {
    let actual = spec.functions.get(name).unwrap_or_else(|| {
        panic!("the registry no longer exports `{name}`; the interface is stale")
    });
    assert_eq!(
        &signature(args, ret),
        actual,
        "`{name}` does not match the interface crate's declaration"
    );
}

fn assert_struct(spec: &Spec, name: &str, fields: &str) {
    let actual = spec
        .structs
        .get(name)
        .unwrap_or_else(|| panic!("the registry no longer exports struct `{name}`"));
    assert_eq!(
        actual, fields,
        "struct `{name}` does not match the interface crate"
    );
}

fn assert_union(spec: &Spec, name: &str, cases: &str) {
    let actual = spec
        .unions
        .get(name)
        .unwrap_or_else(|| panic!("the registry no longer exports union `{name}`"));
    assert_eq!(
        actual, cases,
        "union `{name}` does not match the interface crate"
    );
}

#[test]
fn every_interface_function_is_exported_by_the_registry() {
    let spec = load_spec();

    for (name, args, ret) in READ_ONLY_SURFACE {
        assert_function(&spec, name, args, ret);
    }
}

#[test]
fn the_registry_exports_nothing_the_interface_has_not_declared() {
    let spec = load_spec();

    // The other direction. A read-only method added to the registry and
    // forgotten here is not a failure — it just is not published yet — but a
    // *mutating* method leaking into the "read-only interface" would be a
    // correctness bug in the trait's central claim, so that is what this
    // checks.
    for name in spec.functions.keys() {
        // The constructor is not part of any interface surface: the host calls
        // it at deploy time, never a consumer.
        if name == "__constructor" {
            continue;
        }
        // The invariant is about *reads*, and read entrypoints on this contract
        // are exactly the `get_*` / `is_*` ones. Every mutating export is
        // correctly absent from a read-only interface, so it is not an error
        // that `register_contract` and friends are missing from the surface.
        if !(name.starts_with("get_") || name.starts_with("is_")) {
            continue;
        }
        assert!(
            READ_ONLY_SURFACE
                .iter()
                .any(|(published, ..)| published == name),
            "the registry exposes the read `{name}`, which this interface does not \
             publish. Add it to `RegistryInterface` and to `READ_ONLY_SURFACE`."
        );
    }
}

#[test]
fn the_published_trait_declares_exactly_this_surface() {
    // The other half of the contract: the table above has to describe the trait
    // that is actually published, not a trait somebody once wrote. Renaming a
    // method in `src/lib.rs` and forgetting the test would otherwise leave the
    // crate exporting a method no registry has, and the failure would land in
    // somebody else's contract at run time.
    let declared = declared_signatures();
    let expected: BTreeMap<String, String> = READ_ONLY_SURFACE
        .iter()
        .map(|(name, args, ret)| (name.to_string(), signature(args, ret)))
        .collect();

    assert_eq!(
        declared.len(),
        expected.len(),
        "`RegistryInterface` declares {} methods, this test expects {}. One of them \
         moved without the other.",
        declared.len(),
        expected.len()
    );
    for (name, expected_signature) in &expected {
        let actual = declared.get(name).unwrap_or_else(|| {
            panic!(
                "`RegistryInterface` no longer declares `{name}`; the published interface is stale"
            )
        });
        assert_eq!(
            actual, expected_signature,
            "`{name}` in `RegistryInterface` does not match the signature the registry exports"
        );
    }
}

/// The trait's methods, as the spec would spell them, read straight out of the
/// source file rather than out of the compiled crate.
fn declared_signatures() -> BTreeMap<String, String> {
    // Doc comments are dropped first: the crate's own module docs quote this
    // trait, and prose — a doc comment, or a `{` in one — must not be mistaken
    // for the declaration.
    let source: String = include_str!("../src/lib.rs")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    let body = source
        .split_once("pub trait RegistryInterface {")
        .expect("`RegistryInterface` is not declared in src/lib.rs")
        .1
        .split_once("\n}\n")
        .expect("the `RegistryInterface` block is not closed")
        .0;

    let declarations = body.split('\n').collect::<Vec<_>>().join(" ");

    let mut out = BTreeMap::new();
    for declaration in declarations.split(';') {
        let declaration = declaration.trim();
        if !declaration.starts_with("fn ") {
            continue;
        }
        let declaration = declaration.trim_start_matches("fn ");
        let (head, ret) = declaration
            .split_once("->")
            .expect("a trait method without a return type");
        let (name, args) = head
            .split_once('(')
            .map(|(name, rest)| (name.trim(), rest.trim().trim_end_matches(')')))
            .expect("a trait method without an argument list");
        // The leading `env: Env` is the SDK's own plumbing and is not part of
        // the wire signature, which is why the spec does not mention it. Drop
        // the whole parameter when present.
        let args = split_top_level(args)
            .iter()
            .filter(|arg| arg.trim() != "env: Env")
            .map(|arg| {
                let (arg_name, arg_type) = arg
                    .split_once(':')
                    .expect("a trait argument without a type");
                format!("{}: {}", arg_name.trim(), to_spec_type(arg_type))
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.insert(name.to_string(), signature(&args, &to_spec_type(ret)));
    }
    out
}

/// Split on commas that are not nested inside `<>` or `()`.
fn split_top_level(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (i, c) in s.char_indices() {
        match c {
            '<' | '(' => depth += 1,
            '>' | ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = s[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts
}

/// Spell a Rust type the way the contract spec spells it, so a trait signature
/// and a spec entry can be compared as strings.
fn to_spec_type(rust: &str) -> String {
    let t = rust.trim();
    if let Some(open) = t.find('<') {
        let head = &t[..open];
        let inner = &t[open + 1..t.len() - 1];
        let mapped = split_top_level(inner)
            .iter()
            .map(|part| to_spec_type(part))
            .collect::<Vec<_>>()
            .join(", ");
        return format!("{head}<{mapped}>");
    }
    match t {
        "u32" => "U32",
        "u64" => "U64",
        "i128" => "I128",
        "i64" => "I64",
        "i32" => "I32",
        "bool" => "Bool",
        other => other,
    }
    .to_string()
}

#[test]
fn interface_types_match_the_registry() {
    let spec = load_spec();

    // Struct fields come out of the spec sorted by name, not in declaration
    // order, so these expectations are alphabetical. Enum and union cases do
    // *not* get sorted — the ones below keep declaration order.
    assert_struct(
        &spec,
        "ContractEntry",
        "{active: Bool, contract_id: Address, description: String, name: String, \
         owner: Address, registered_at: U32}",
    );
    assert_struct(
        &spec,
        "ContractProfile",
        "{entry: ContractEntry, reputation: Reputation, superseded_by: Option<Address>}",
    );
    assert_struct(
        &spec,
        "Attestation",
        "{attester: Address, created_at: U32, label: String}",
    );
    assert_struct(
        &spec,
        "Reputation",
        "{slashed_total: I128, stake: I128, verified: Bool, withdraw_locked_until: U32}",
    );
    assert_struct(
        &spec,
        "SlashRecord",
        "{amount: I128, reason: String, response: Option<String>, slashed_at: U32}",
    );
    assert_struct(
        &spec,
        "ContractPage",
        "{entries: Vec<ContractEntry>, has_more: Bool}",
    );
    assert_struct(
        &spec,
        "ContractProfilePage",
        "{entries: Vec<ContractProfile>, has_more: Bool}",
    );
    assert_struct(
        &spec,
        "RegistryStats",
        "{active_count: U32, staked_count: U32, total_registered: U32, total_staked: I128, \
         verified_count: U32}",
    );
    assert_struct(
        &spec,
        "Proposal",
        "{action: ProposalAction, approvals: Vec<Address>, executed: Bool, id: U32, \
         proposer: Address, ready_at: U32}",
    );

    // A payload-free `#[contracttype] enum` is emitted by the SDK as a union of
    // void cases rather than a UDT enum, so it is checked as one. The order is
    // declaration order, not sorted.
    assert_union(
        &spec,
        "Category",
        "DeFi,Nft,Gaming,Identity,Infrastructure,Payments,Oracle,Dao,Other",
    );

    assert_union(
        &spec,
        "ProposalAction",
        "Deactivate(Address),Upgrade(BytesN<32>),AddAdmin(Address),RemoveAdmin(Address),\
         ChangeThreshold(U32),ConfigureStaking(Address,Address),SetVerified(Address,Bool),\
         Slash(Address,I128,String),SetAllowlistEnabled(Bool),SetAllowlisted(Address,Bool),\
         ConfigureRegistrationRateLimit(U32,U32),SetRegistrationFee(I128),\
         ConfigureMinimumStake(I128),WithdrawFromTreasury(I128)",
    );
}

#[test]
fn interface_error_codes_match_the_registry() {
    let spec = load_spec();

    // The whole enum, not just the codes a read can produce. A client decodes
    // a contract error by matching against the enum it was generated with, so
    // a code that is missing or renumbered here turns a well-defined error
    // into an opaque decode failure for every consumer.
    let actual = spec
        .errors
        .get("RegistryError")
        .expect("the registry no longer exports error RegistryError");

    // Compare against the declared discriminants rather than a literal string,
    // so this test also fails if the interface crate adds a variant the
    // registry does not have.
    let declared: Vec<(u32, &str)> = vec![
        (
            RegistryError::AlreadyInitialized as u32,
            "AlreadyInitialized",
        ),
        (RegistryError::Unauthorized as u32, "Unauthorized"),
        (RegistryError::AlreadyRegistered as u32, "AlreadyRegistered"),
        (RegistryError::ContractNotFound as u32, "ContractNotFound"),
        (RegistryError::InvalidMetadata as u32, "InvalidMetadata"),
        (RegistryError::NotOwner as u32, "NotOwner"),
        (RegistryError::NotInitialized as u32, "NotInitialized"),
        (RegistryError::ProposalNotFound as u32, "ProposalNotFound"),
        (RegistryError::ThresholdNotMet as u32, "ThresholdNotMet"),
        (
            RegistryError::TimelockNotElapsed as u32,
            "TimelockNotElapsed",
        ),
        (RegistryError::AlreadyApproved as u32, "AlreadyApproved"),
        (RegistryError::NotAdmin as u32, "NotAdmin"),
        (RegistryError::InvalidThreshold as u32, "InvalidThreshold"),
        (RegistryError::AlreadyExecuted as u32, "AlreadyExecuted"),
        (
            RegistryError::StakingNotConfigured as u32,
            "StakingNotConfigured",
        ),
        (RegistryError::InvalidAmount as u32, "InvalidAmount"),
        (RegistryError::InsufficientStake as u32, "InsufficientStake"),
        (RegistryError::StakeLocked as u32, "StakeLocked"),
        (
            RegistryError::RegistrationActive as u32,
            "RegistrationActive",
        ),
        (RegistryError::NoCategories as u32, "NoCategories"),
        (RegistryError::StakeNotEmpty as u32, "StakeNotEmpty"),
        (RegistryError::InvalidRateLimit as u32, "InvalidRateLimit"),
        (RegistryError::NotAllowlisted as u32, "NotAllowlisted"),
        (
            RegistryError::RegistrationRateLimited as u32,
            "RegistrationRateLimited",
        ),
        (RegistryError::InsufficientFee as u32, "InsufficientFee"),
        (RegistryError::InvalidTags as u32, "InvalidTags"),
        (
            RegistryError::InvalidAttestation as u32,
            "InvalidAttestation",
        ),
        (
            RegistryError::AttestationNotFound as u32,
            "AttestationNotFound",
        ),
        (
            RegistryError::OverlappingAddress as u32,
            "OverlappingAddress",
        ),
    ];

    for (code, name) in declared {
        let needle = format!("{name}={code}");
        assert!(
            actual.contains(&needle),
            "RegistryError.{name} = {code} is declared by the interface crate but not by \
             the registry, or with a different discriminant. A consumer decoding that \
             error would fail at run time.\n\nregistry declares: {actual}\n"
        );
    }
}

/// The client this crate publishes is usable as a trait object bound, which is
/// what makes it substitutable in a consumer's own code.
#[test]
fn the_client_is_constructible_and_typed() {
    // A compile-time assertion: if the generated client's shape changes, this
    // stops building. Nothing is invoked — `Env` here is only needed to make
    // the address well-formed.
    let env = soroban_sdk::Env::default();
    let registry = Address::generate(&env);
    let _client: RegistryInterfaceClient = RegistryInterfaceClient::new(&env, &registry);

    // The re-declared types are the ones a consumer actually names.
    let _: Option<ContractEntry> = None;
    let _: Option<ContractProfile> = None;
    let _: Option<Reputation> = None;
    let _: Option<SlashRecord> = None;
    let _: Option<ContractPage> = None;
    let _: Option<ContractProfilePage> = None;
    let _: Option<RegistryStats> = None;
    let _: Option<Category> = None;
    let _: Option<soroban_sdk::String> = None;
    let _: Option<soroban_sdk::Vec<u32>> = None;
}
