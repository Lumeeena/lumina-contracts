# Deploying the Lumina Registry

Deploying the registry is optional — the rest of Lumina (indexer/GraphQL/frontend)
works without it. Deploy (or reuse the existing testnet deployment below) when
you want the indexer to discover contracts from a live on-chain manifest
instead of (or in addition to) a static `INDEXED_CONTRACT_IDS` list.

## Already deployed on testnet

```
Contract ID: CAYUDQPV3RKPM3EXDFGI3457FV677JLUCJ4OLKWGCUBPRIHYKXK3WFAZ
Admin:       GBWKFFXZ5CJESIHP2EOID5IOXMF472RO5XOJ36X475D5LJGI3AF5R5KY
```

It has one demo entry (itself), registered to verify indexer discovery
end-to-end. Point `lumina-backend` at it directly — see that repo's README
for the `REGISTRY_CONTRACT_ID` / `REGISTRY_READ_ACCOUNT` env vars — or deploy
your own following the steps below.

## Deploying your own

### Prerequisites

- [Stellar CLI](https://developers.stellar.org/docs/tools/stellar-cli) (`stellar`, formerly `soroban`)
- A funded testnet identity

```bash
stellar keys generate lumina-deployer --network testnet --fund
```

### Build and deploy

```bash
stellar contract build
stellar contract deploy \
  --wasm target/wasm32v1-none/release/lumina_registry.wasm \
  --source lumina-deployer \
  --network testnet \
  --alias lumina-registry
```

This prints the deployed contract's `C...` address — save it as `REGISTRY_CONTRACT_ID`.
If the deploy step fails with `HostError: Error(Storage, MissingValue)` /
"Wasm does not exist", that's just RPC propagation lag after the upload —
rerun the same `deploy` command a few seconds later; it skips re-uploading
and picks up from the create-contract step.

### Initialize

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  -- initialize --admin <your-address-G...>
```

### Register a contract for indexing

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  -- register_contract \
  --owner <owner-address-G...> \
  --contract_id <target-contract-C...> \
  --name "My Protocol" \
  --description "A DeFi protocol on Stellar"
```

### Verify discovery works

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  -- get_active_contracts --offset 0 --limit 10
```

### Manage your registration

List everything one address has registered (deactivated entries included):

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  -- get_contracts_by_owner \
  --owner <owner-address-G...> --offset 0 --limit 10
```

Correct a name or description — only the registered owner can do this:

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  -- update_metadata \
  --owner <owner-address-G...> \
  --contract_id <target-contract-C...> \
  --name "My Protocol" \
  --description "An updated description"
```

Hand the registration to a new key — callable by the current owner or the
admin:

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  -- transfer_ownership \
  --caller <current-owner-G...> \
  --contract_id <target-contract-C...> \
  --new_owner <new-owner-G...>
```

## Upgrading a live registry

A Soroban upgrade replaces the contract's **code** and keeps its **address and
storage**. Nothing has to be re-registered, and every `REGISTRY_CONTRACT_ID`
already configured downstream keeps working.

Only the admin stored at `initialize` time can do it:

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  -- get_admin
```

### 1. Check what is live now

```bash
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet -- get_version
```

### 2. Record the current wasm — this is your rollback target

```bash
stellar contract fetch --id lumina-registry --network testnet \
  --out-file rollback.wasm
sha256sum rollback.wasm   # macOS: shasum -a 256 rollback.wasm
```

Save that hash (and the wasm) somewhere durable *before* upgrading. Rolling back
is just another `upgrade` to that hash, but only if you still have it.

### 3. Build and upload the new wasm

```bash
stellar contract build
stellar contract upload \
  --wasm target/wasm32v1-none/release/lumina_registry.wasm \
  --source lumina-deployer \
  --network testnet
```

`upload` prints the new wasm hash. Verify it before submitting the upgrade —
the hash is over the exact bytes, so rebuild locally from the commit you intend
to ship and confirm the two agree:

```bash
stellar contract build
sha256sum target/wasm32v1-none/release/lumina_registry.wasm  # macOS: shasum -a 256
```

Build with `wasm32v1-none` (what `stellar contract build` uses). A
`wasm32-unknown-unknown` build of the same source produces different bytes and,
on current Rust, a module the Soroban host refuses to load — an upgrade to that
hash bricks the contract with no way to call `upgrade` again.

### 4. Submit the upgrade

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  -- upgrade \
  --admin <admin-address-G...> \
  --new_wasm_hash <hash-from-upload>
```

The swap takes effect for the *next* invocation; the call that performs it runs
to completion under the old code and emits a `registry_upgraded` event carrying
the admin, the new hash, and the version being replaced.

### 5. Verify

```bash
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet -- get_version
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet -- get_active_contracts --offset 0 --limit 10
```

`get_version` should report the new value and the registrations should come back
unchanged.

### Storage compatibility

Because storage survives the swap, the new code has to decode entries the old
code wrote:

- Adding a `DataKey` variant is safe. Renaming or repurposing one is not —
  `#[contracttype]` enums are keyed by variant name, so a rename orphans every
  entry stored under the old name.
- Adding, removing, renaming or retyping a `ContractEntry` field breaks every
  entry already stored. The struct is encoded as a map keyed by field name, so
  old entries fail to decode rather than picking up defaults.
- A release that must change `ContractEntry` needs a migration: read the old
  shape into a retained `EntryV1`-style type and write the new shape back,
  lazily on first access or through a batched admin-gated `migrate()` — do not
  assume one transaction can touch every entry.
- Bump `CONTRACT_VERSION` in `registry/src/lib.rs` with any such change so
  downstream callers can branch on `get_version()`.

`registry/src/lib.rs`'s test suite deploys the registry from wasm, registers
contracts, upgrades to `registry-v2/`, and asserts the registrations are still
readable by the new code — including a rollback back to the previous wasm hash.

### Rollback

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  -- upgrade \
  --admin <admin-address-G...> \
  --new_wasm_hash <hash-recorded-in-step-2>
```

Two caveats. Rolling back restores the old *code* only — any storage the new
version wrote stays, so a rollback across a migration needs its own reverse
migration. And rollback runs through the same `upgrade` entrypoint, so it is
only available while the deployed code still exports one: a version that drops
`upgrade` is permanent.
