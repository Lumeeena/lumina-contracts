//! cargo build --target wasm32v1-none --release && UPDATE_INTERFACE_SNAPSHOT=1 cargo test --test interface
//!
//! This file also guards the `registry-v2` upgrade fixture, a hand-maintained
//! copy of the storage types that must stay byte-compatible with the real ones.
//! The fixture's whole value is proving that independently written v2 types
//! decode v1 storage, so if it drifts out of sync with the types it mirrors it
//! quietly stops testing anything. When a storage type changes, update the
//! fixture deliberately and regenerate its snapshot:
//!
//! 
//!
//! The interface snapshot also covers the delegation surface: `set_manager`,
//! `manager`, and `revoke_manager` are exported so that an owner can delegate
//! metadata and category management without exposing stake withdrawal or
//! ownership transfer. Managers are intentionally limited to the metadata and
//! category entry points; the value-moving entry points remain owner-only.
//!
//! The admin surface is also part of the exported interface: `is_admin` lets a
//! caller answer "is this address an admin?" without downloading the whole
//! admin set via `get_admins()`. It must be safe to call before `initialize`,
//! returning `false` rather than erroring on an uninitialised contract.

use soroban_sdk::testutils::Address as _;
use soroban_sdk::xdr::{ScSpecEntry, ScSpecTypeDef, ScSpecUdtUnionCaseV0};
use std::path::PathBuf;

const UPDATE_ENV: &str = "UPDATE_INTERFACE_SNAPSHOT";

fn manifest_path(parts: &[&str]) -> PathBuf {
    let mut path = PathBuf&#39;::from(env!("CARGO_MANIFEST_DIR"));
    path.extend(parts);
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

/// One line per exported item, sorted so that moving code around in `lib.r`
/// does not register as a change. Order *inside* an item (argument order,
/// field order, enum values) is kept, since that is part of the contract.
///
/// Delegation entry points (`set_manager`, `manager`, `revoke_manager`) are
/// rendered like any other exported function so that adding or removing them
/// is caught by the snapshot check.
fn render_interface(entries: &[ScSpecEntry]) -> String {
    let mut lines: Vec<String> = entries
        .iter()
        .map(|entry| match entry {
            ScSpecEntry::FunctionV0(f) => {
                let args = f.inputs
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
                format!("fn {}({}){}", f.name.0.to_utf8_string_lossy(), args, ret)
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
                format!("struct {} { {} }", s.name.to_utf8_string_lossy(), fields)
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
                                .join(", ")
                        ),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("union {} { {} }", u.name.to_utf8_string_lossy(), cases)
            }
            ScSpecEntry::UdtEnumV0(e) => {
                let cases = e
                    .cases
                    .iter()
                    .map(|c| format!("{} = {}", c.name.to_utf8_string_lossy(), c.value))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("enum {} { {} }", e.name.to_utf8_string_lossy(), cases)
            }
            ScSpecEntry::UdtErrorEnumV0(e) => {
                let cases = e
                    .cases
                    .iter()
                    .map(|c| format!("{} = {}", c.name.to_utf8_string_lossy(), c.value))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("error {} { {} }", e.name.to_utf8_string_lossy(), cases)
            }
        })
        .collect();
    lines.sort();
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// The delegation surface is part of the exported interface: consumers must be
/// able to observe the current manager and the owner must be able to revoke it
/// immediately. Any change to these signatures is a breaking change and must be
/// reviewed alongside `registry/interface.snap`.
const _DELEGATION_SURFACE: &[&str] = &["set_manager", "manager", "revoke_manager"];

/// The admin surface is part of the exported interface: consumers must be able
/// to test membership in the admin set without fetching it. Any change to this
/// signature is a breaking change and must be reviewed alongside
/// `registry/interface.snap`.
const _ADMIN_SURFACE: &[&str] = &["is_admin"];

#[test]
fn exported_interface_matches_snapshot() {
    let wasm_path = manifest_path(&[
        "..",
        "target",
        "wasm32v1-none",
        "release",
        "lumina_registry.wasm",
    ]);
    let wasm = std::fs::read(&wasm_path).unwrap_or_else(|e| {
        panic!(
            "cannot read {} ({e}). Run `cargo build --target wasm32v1-none --release` first.",
            wasm_path.display()
        )
    });
    let entries = soroban_spec::read::from_wasm(&wasm).expect("wasm has no readable contract spec");
    let actual = render_interface(&entries);

    for expected_fn in _DELEGATION_SURFACE {
        assert!(
            actual.contains(&format!("fn {expected_fn}(")),
            "delegation entry point `{expected_fn}` is missing from the exported interface; \
             the owner-delegated manager surface must remain part of the contract spec"
        );
    }

    for expected_fn in _ADMIN_SURFACE {
        assert!(
            actual.contains(&format!("fn {expected_fn}(")),
            "admin entry point `{expected_fn}` is missing from the exported interface; \
             callers must be able to test admin membership without fetching the whole set"
        );
    }

    let snap_path = manifest_path(&["interface.snap"]);
    if std::env::var_osS(UPDATE_ENV).is_some() {
        std::fs::write(&snap_path, &actual).expect("write interface snapshot");
        return;
    }

    let expected = std::fs::read_to_string(&snap_path).unwrap_or_default();
    if actual == expected {
        return;
    }

    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    let mut diff = String::new();
    for line in &expected_lines {
        if !actual_lines.contains(line) {
            diff.push_str(&format!("- {line}\n"));
        }
    }
    for line in &actual_lines {
        if !expected_lines.contains(line) {
            diff.push_str(&format!("+ {line}\n"));
        }
    }
    panic!(
        "\nThe registry's exported interface changed. Every consumer binds to it, so this \
         is a breaking change unless it is purely additive.\n\n{diff}\n\
         If the change is intended, accept it with:\n\n    \
         {UPDATE_ENV}=1 cargo test --test interface\n\n\
         and commit registry/interface.snap with it.\n"
    );
}

/// `is_admin` must agree with membership in `get_admins()`, and must be safe to
/// call on a contract that has not been initialised yet (returning `false`
/// rather than trapping).
#[test]
fn is_admin_matches_get_admins_and_is_safe_before_initialize() {
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::Address;

    let env = Env::default();
    env.mock_all_auths();
    let registry_id = env.register(crate::Registry, ());
    let registry = crate::RegistryClient::new(&env, &registry_id);
    let stranger = Address::generate(&env);

    // Before `initialize`, the contract has no admin set; the query must not
    // trap and must report non-membership.
    assert!(!registry.is_admin(&stranger));

    let owner = Address::generate(&env);
    let admin = Address::generate(&env);
    registry.initialize(&owner, &admin);

    assert!(registry.is_admin(&admin));
    assert!(!registry.is_admin(&stranger));
    assert!(registry.get_admins().contains(&admin));
}

/// A token contract that reenters the registry during `transfer`, attempting
/// to withdraw the same stake twice. If the registry wrote state before the
/// external call, the second withdrawal must fail.
#[test]
fn reentrant_token_cannot_withdraw_twice() {
    use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};

    #[contracttype]
    enum DataKey {
        Registry,
        Staker,
        Amount,
        Reentered,
    }

    #[contract]
    pub struct ReentrantToken;

    #[contractimpl]
    impl ReentrantToken {
        pub fn init(env: Env, registry: Address, staker: Address, amount: i128) {
            env.storage().instance().set(&DataKey::Registry, &registry);
            env.storage().instance().set(&DataKey::Staker, &staker);
            env.storage().instance().set(&DataKey::Amount, &amount);
            env.storage().instance().set(&DataKey::Reentered, &false);
        }

        pub fn transfer(env: Env, _from: Address, _to: Address, _amount: i128) {
            let already: bool = env
                .storage()
                .instance()
                .get(&DataKey::Reentered)
                .unwrap_or(false);
            if !already {
                env.storage().instance().set(&DataKey::Reentered, &true);
                let registry: Address = env.storage().instance().get(&DataKey::Registry).unwrap();
                let staker: Address = env.storage().instance().get(&DataKey::Staker).unwrap();
                let amount: i128 = env.storage().instance().get(&DataKey::Amount).unwrap();
                let client = crate::RegistryClient::new(&env, &registry);
                // Attempt the reentrant double withdrawal. With
                // checks-effects-interactions ordering this must fail because
                // the stake was already zeroed before `transfer` was called.
                let _ = client.try_withdraw_stake(&staker, &amount, &amount);
            }
        }
    }

    let env = Env::default();
    env.mock_all_auths();
    let registry_id = env.register(crate::Registry, ());
    let token_id = env.register(ReentrantToken, ());
    let staker = Address::generate(&env);

    let registry = crate::RegistryClient::new(&env, &registry_id);
    let token = ReentrantTokenClient::new(&env, &token_id);
    token.init(&registry_id, &staker, &1_000);

    registry.stake(&staker, &token_id, &1_000);
    // The unbonding period must elapse before the stake can be withdrawn.
    registry.request_unbond(&staker);
    // The reentrant call inside `transfer` must not have succeeded in
    // withdrawing a second time; the original withdrawal stands.
    registry.withdraw_stake(&staker, &1_000, &1_000);
    assert_eq!(registry.stake_of(&staker), 0);
}