# Lumina Contracts

> Soroban smart contracts for Lumina, an open-source event indexer and GraphQL data layer for the Stellar network.

Part of the Lumina project, split across three repos:

- [lumina-frontend](https://github.com/Lumeeena/lumina-frontend) — Next.js explorer UI
- [lumina-backend](https://github.com/Lumeeena/lumina-backend) — indexer + GraphQL API + PostgreSQL schema
- [lumina-contracts](https://github.com/Lumeeena/lumina-contracts) — this repo

## Lumina Registry

`registry/` — an on-chain manifest of Soroban contracts registered for Lumina indexing. Any project can call `register_contract()` to add their contract; [lumina-backend](https://github.com/Lumeeena/lumina-backend)'s indexer can then discover and index their events.

```rust
registry.register_contract(owner, contract_id, "My Protocol", "A DeFi protocol on Stellar")
```

`get_active_contracts(offset, limit)` returns a paginated list of active registrations for discovery.

Registrations are also manageable after the fact:

| Method | Who can call it |
| --- | --- |
| `get_contracts_by_owner(owner, offset, limit)` | anyone — paginated, includes the owner's deactivated entries |
| `update_metadata(owner, contract_id, name, description)` | the registered owner only |
| `transfer_ownership(caller, contract_id, new_owner)` | the current owner or the admin |
| `deactivate(caller, contract_id)` | the current owner or the admin |

### Upgrades

The registry is upgradeable in place, so a fix or a new entrypoint does not
orphan existing registrations at a new address:

| Method | Who can call it |
| --- | --- |
| `get_version()` | anyone — which build is live at this address |
| `get_admin()` | anyone |
| `upgrade(admin, new_wasm_hash)` | the admin only |

`upgrade` swaps the contract's code and keeps its address and storage, so a new
version must stay compatible with the storage shapes documented on `DataKey` and
`ContractEntry` in [registry/src/lib.rs](./registry/src/lib.rs). See
[DEPLOY.md](./DEPLOY.md#upgrading-a-live-registry) for the live runbook.

## Build & Test

```bash
cargo build --target wasm32v1-none --release
cargo test
```

The wasm build has to come first: the upgrade tests deploy the registry from its
compiled wasm — the only form a Soroban upgrade can be performed on — and upgrade
it to `registry-v2/`, a deliberately minimal second version that exists only as
that test's upgrade target and is never deployed.

Use `wasm32v1-none`, not `wasm32-unknown-unknown`; on current Rust the latter
emits the reference-types proposal, which the Soroban host refuses to load.

## Deploying

Deployed on **testnet** at:

```
CAYUDQPV3RKPM3EXDFGI3457FV677JLUCJ4OLKWGCUBPRIHYKXK3WFAZ
```

[lumina-backend](https://github.com/Lumeeena/lumina-backend)'s indexer polls this contract for discovery when configured with `REGISTRY_CONTRACT_ID` (see that repo's README). See [DEPLOY.md](./DEPLOY.md) for the deployment steps used, and how to register your own contract.

## License

MIT
