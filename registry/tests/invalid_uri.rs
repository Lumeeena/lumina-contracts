// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Dedicated tests for the `InvalidUri` error path.
//!
//! `RegistryError::InvalidUri` (code 41) is returned by `update_metadata_uri`
//! when the caller supplies a URI that is either too long (> 2 048 bytes) or
//! uses an unsupported scheme.  Callers branch on the bare integer, so both
//! raise sites need a failing test: removing either guard must break the
//! matching assertion.
//!
//! Issue: Add a dedicated test for the InvalidUri error path.

use lumina_registry::{Category, LuminaRegistry, LuminaRegistryClient, RegistryError};
use soroban_sdk::{testutils::Address as _, Address, Env, String, Vec};

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn setup() -> (Env, LuminaRegistryClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let registry_id = env.register(LuminaRegistry, (&admin,));
    let client = LuminaRegistryClient::new(&env, &registry_id);
    (env, client, admin)
}

fn categories(env: &Env) -> Vec<Category> {
    Vec::from_slice(env, &[Category::Infrastructure])
}

/// Register a fresh contract and return `(owner, contract_id)`.
fn register(env: &Env, client: &LuminaRegistryClient) -> (Address, Address) {
    let owner = Address::generate(env);
    let target = Address::generate(env);
    client.register_contract(
        &owner,
        &target,
        &String::from_str(env, "Example"),
        &String::from_str(env, "URI test registration"),
        &categories(env),
    );
    (owner, target)
}

// ─── InvalidUri: unsupported scheme ──────────────────────────────────────────

/// A URI with an unsupported scheme must be rejected.
/// Removing the scheme check (`!slice.starts_with(b"http://") && …`) must
/// cause this assertion to fail.
#[test]
fn invalid_uri_for_unsupported_scheme() {
    let (env, client, _admin) = setup();
    let (owner, target) = register(&env, &client);

    assert_eq!(
        client.try_update_metadata_uri(
            &owner,
            &target,
            &Some(String::from_str(&env, "ftp://example.com/meta.json")),
        ),
        Err(Ok(RegistryError::InvalidUri))
    );
}

/// A bare path with no scheme is also rejected.
#[test]
fn invalid_uri_for_bare_path() {
    let (env, client, _admin) = setup();
    let (owner, target) = register(&env, &client);

    assert_eq!(
        client.try_update_metadata_uri(
            &owner,
            &target,
            &Some(String::from_str(&env, "/ipfs/QmFoo")),
        ),
        Err(Ok(RegistryError::InvalidUri))
    );
}

/// An empty string has no recognised scheme.
#[test]
fn invalid_uri_for_empty_string() {
    let (env, client, _admin) = setup();
    let (owner, target) = register(&env, &client);

    assert_eq!(
        client.try_update_metadata_uri(&owner, &target, &Some(String::from_str(&env, ""))),
        Err(Ok(RegistryError::InvalidUri))
    );
}

// ─── InvalidUri: length exceeds 2 048 bytes ──────────────────────────────────

/// A URI longer than 2 048 bytes is rejected regardless of its scheme.
/// Removing the `len > 2048` guard must cause this assertion to fail.
#[test]
fn invalid_uri_for_uri_exceeding_max_length() {
    let (env, client, _admin) = setup();
    let (owner, target) = register(&env, &client);

    // Build a URI whose total byte length is 2 049 — one over the limit.
    // "https://example.com/" is 20 bytes; pad with 'x' to reach 2 049.
    let padding = "x".repeat(2049 - 20);
    let too_long = std::format!("https://example.com/{}", padding);
    assert_eq!(too_long.len(), 2049);

    assert_eq!(
        client.try_update_metadata_uri(
            &owner,
            &target,
            &Some(String::from_str(&env, &too_long)),
        ),
        Err(Ok(RegistryError::InvalidUri))
    );
}

// ─── Happy paths ─────────────────────────────────────────────────────────────

/// All four accepted schemes must be accepted.
#[test]
fn valid_uri_accepted_for_all_supported_schemes() {
    let (env, client, _admin) = setup();

    for scheme_uri in &[
        "http://example.com/meta.json",
        "https://example.com/meta.json",
        "ipfs://QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG",
        "ipns://k51qzi5uqu5dh9ihj0ml3hd5nkr9xzaap4",
    ] {
        let (owner, target) = register(&env, &client);
        assert_eq!(
            client.try_update_metadata_uri(
                &owner,
                &target,
                &Some(String::from_str(&env, scheme_uri)),
            ),
            Ok(Ok(()))
        );
    }
}

/// A URI of exactly 2 048 bytes is at the boundary and must be accepted.
#[test]
fn valid_uri_at_exactly_max_length_is_accepted() {
    let (env, client, _admin) = setup();
    let (owner, target) = register(&env, &client);

    let padding = "x".repeat(2048 - 20);
    let at_limit = std::format!("https://example.com/{}", padding);
    assert_eq!(at_limit.len(), 2048);

    assert_eq!(
        client.try_update_metadata_uri(
            &owner,
            &target,
            &Some(String::from_str(&env, &at_limit)),
        ),
        Ok(Ok(()))
    );
}

/// Passing `None` always succeeds and clears any previously stored URI.
#[test]
fn none_uri_is_always_accepted() {
    let (env, client, _admin) = setup();
    let (owner, target) = register(&env, &client);

    assert_eq!(
        client.try_update_metadata_uri(&owner, &target, &None),
        Ok(Ok(()))
    );
}
