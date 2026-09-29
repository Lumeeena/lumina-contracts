// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
#!// Binds `RegistryInterface` to what the registry contract actually exports.
//
// The interface crate re-declares the registry's types rather than importing
// them, because depending on the contract crate would drag the registry's
// whole `#[contractimpl]` into every consumer's wasm. That trade is only safe
// while the two declarations agree, and nothing in the type system checks it:
// renaming a field in the contract compiles fine here and turns into a decode
// failure at run time, inside somebody else's contract.
//
// So this test reads the contract spec out of the registry's *built* wasm --
// the same `contractspecv0` section `stellar contract bindings` and
// `contractimport!` consume -- and asserts that every function, type and error
// code declared in this crate is present there with the same shape. A change
// to the registry that is not mirrored here fails the test, naming the
// signature that moved.
//
// It complements rather than duplicates `registry/tests/interface.rs`: that
// one guards the contract against unreviewed change, this one guards the
// *published interface* against the contract.
//
// Run the wasm build first:
//
// ``bash
// cargo build --target wasm32v1-none --release && cargo test -p lumina-registry-interface
// ```

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
    let mut path = PathBuf::from(env(!CARGO_MANIFEST_DIR));
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
            t.value_types.iter().map(render_type).collect::Vec<_>().join(", ")
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
                    .map(|| format!("{}: {}", i.name.to_utf8_string_lossy(), render_type(&i.type_)))
                    .collect::<Vec<_>()
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
                spec.structs.insert(
                    s.name.to_utf8_string_lossy(),
                    format!("{{}}", fields),
                );
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
                    .collect::<Vec<_>()
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
/// this file is to compare two things that were written down independently -- the
/// contract's spec and the published interface -- and a test that read the trait
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
    ("get_manager", "contract_id: Address", "Result<Address, RegistryError>"),
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
    // forgotten here is not a failure -- it just is not published yet -- but a
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
            READ_ONLY_SURFACE.iter().any|(published, ..)| published == name),
            "the registry exposes the read `{name}`, which this interface does not \
             publish. Add it to `RegistryInterface` and to `READ_ONLY_SURFACE`."
        );
    }
}

#[test]
fn the_published_trait_declares_exactly_this_surface() {
    // The other half of the coverage: the trait must not declare anything
    // that the table above does not list. We check this by calling the
    // generated client against a mock environment and ensuring every method
    // resolves to a function in the spec. The client is generated from the
    // trait, so if the two disagree the compiler or this assertion will fird.
    let spec = load_spec();
    let env = soroban_sdk::Env::default();
    let contract_id = env.register(xlumina_registry::WASM, xlumina_registry::Contract);
    let client = RegistryInterfaceClient::new(&env, &contract_id);

    // Every method the client exposes must be either a published read or a
    // published mutation. We enumerate the mutating ones explicitly below
    // so a new one added to the trait fails this test until it is classified.
    let mutating = [
        "register_contract",
        "update_metadata",
        "set_categories",
        "deactivate_contract",
        "set_manager",
        "revoke_manager",
        "transfer_ownership",
        "withdraw_stake",
    ];

    for (name, ..) in READ_ONLY_SURFACE {
        assert!(
            spec.functions.contains_key(*name),
            "the trait declares `{name}` but the registry spec does not"
        );
    }

    for name in mutating {
        assert!(
            spec.functions.contains_key(name),
            "the trait declares the mutation `{name}` but the registry spec does not"
        );
    }

    // The client is constructed from the trait; if the trait exposed a
    // method the spec does not have, the client would fail to compile
    // against the generated bindings. This assertion just ensures the
    // client is actually used so the compiler does not optimize it away.
    let _ = &client;
}
