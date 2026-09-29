# Contributing to Lumina Contracts

Thanks for helping improve the Soroban contracts that power Lumina registry discovery.

## Before you start

Read the repository README and the issue you plan to address. Keep changes focused on that issue and explain any interface or storage impact in the pull request. Never include secret keys, deployed credentials, or private account data.

## Build and test sequence

The registry upgrade tests deploy compiled WebAssembly fixtures, so build the release wasm before running the test suite:

~~~bash
cargo build --target wasm32v1-none --release
cargo test
~~~

Use wasm32v1-none, not wasm32-unknown-unknown. The Soroban host expects the former target on current Rust toolchains.

The build creates the fixtures consumed by the registry upgrade tests:

- target/wasm32v1-none/release/lumina_registry.wasm
- target/wasm32v1-none/release/lumina_registry_v2.wasm

If cargo test reports that an upgrade fixture is missing, rerun the wasm build first.

The interface test also reads the built registry wasm. It compares the exported contract specification with registry/interface.snap:

~~~bash
UPDATE_INTERFACE_SNAPSHOT=1 cargo test --test interface
~~~

Only use the snapshot update command when the interface change is intentional. Commit the resulting snapshot with the code change and explain why the change is compatible for the indexer, frontend, and registrants.

## Where tests go

- Unit and registry behavior tests live in registry/src/lib.rs alongside the implementation.
- Cross-cutting interface checks live in registry/tests/interface.rs.
- Upgrade behavior belongs with the existing upgrade tests and must use the compiled wasm fixtures.
- Prefer focused tests that assert both the success path and the relevant authorization or validation failure.

Keep tests deterministic. Do not depend on a live Stellar network, deployed contract IDs, wall-clock timing, or credentials.

## Adding an upgrade fixture

When an upgrade test needs a new contract version:

1. Add or update the minimal fixture crate under registry-v2/.
2. Ensure its wasm artifact is produced by the workspace release build.
3. Import the fixture from the upgrade test using the existing project pattern.
4. Document the storage and interface compatibility being exercised.
5. Run the wasm build before the tests so both release artifacts exist.

A fixture should stay deliberately minimal. It exists to exercise an upgrade path, not to duplicate the full registry implementation.

## Pull requests

Use a descriptive branch and commit message. A pull request should include:

- A short summary of the behavior changed.
- The issue number and a clear scope statement.
- Focused tests for new behavior, when applicable.
- Any interface snapshot or migration impact.
- The verification commands run locally.

Keep unrelated formatting and refactors out of the branch. Pull requests that change exported methods, types, error codes, event topics, or storage shapes need extra compatibility context because the backend indexer and frontend bind to this contract at runtime.

## License

By contributing, you agree that your work is provided under the repository's MIT license.