# Lumina Contracts

> Soroban smart contracts for Lumina, an open-source event indexer and GraphQL data layer for the Stellar network.

Part of the Lumina project, split across three repos:

- [lumina-frontend](https://github.com/Lumeeena/lumina-frontend) — Next.js explorer UI
- [lumina-backend](https://github.com/Lumeeena/lumina-backend) — indexer + GraphQL API + PostgreSQL schema
- [lumina-contracts](https://github.com/Lumeeena/lumina-contracts) — this repo

## Lumina Registry

`registry/` — an on-chain manifest of Soroban contracts registered for Lumina indexing. Any project can call `register_contract()` to add their contract; [lumina-backend](https://github.com/Lumeeena/lumina-backend)'s indexer can then discover and index their events.

```rust
registry.register_contract(owner, contract_id, "My Protocol", "A DeFi protocol on Stellar", vec![Category::DeFi])
```

`get_active_contracts(offset, limit)` returns a paginated list of active registrations for discovery.

### Categories

Every registration declares at least one category, so the Registry supports
browsing rather than only a flat list:

`DeFi` · `Nft` · `Gaming` · `Identity` · `Infrastructure` · `Payments` ·
`Oracle` · `Dao` · `Other`

| Method | Who can call it |
| --- | --- |
| `get_active_contracts_by_category(category, offset, limit)` | anyone — same paging semantics as `get_active_contracts` |
| `get_categories(contract_id)` | anyone |
| `set_categories(owner, contract_id, categories)` | the registered owner only |

A contract can be filed under several categories and is discoverable under each.
Duplicates are collapsed, so passing a category twice indexes it once.

The vocabulary is a fixed enum rather than free-form tags because the point is
browsing, and free-form tags fragment it immediately — `DeFi`, `defi` and `De-Fi`
become three categories each holding part of the answer. Adding a category is a
contract upgrade; `Other` is the escape hatch until then.

`deactivate` does not rewrite category indices. `get_active_contracts_by_category`
filters on `active`, exactly as the global listing does, which is what keeps a
deactivated registration out of browsing.

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

### Staking & reputation

Registration itself stays free and permissionless — anyone can list a contract
for indexing. On top of that, a registrant can post collateral, and governance
can attest or penalise, so consumers of the Registry can tell a well-run project
apart from a name that was typed into a form:

| Method | Who can call it |
| --- | --- |
| `stake(owner, contract_id, amount)` | the registered owner — additive, tops up an existing stake |
| `withdraw_stake(owner, contract_id)` | the registered owner, in good standing (see below) |
| `propose_set_verified(proposer, contract_id, verified)` | an admin — takes effect only after approval + timelock |
| `propose_slash(proposer, contract_id, amount, reason)` | an admin — same |
| `propose_configure_staking(proposer, token, treasury)` | an admin — same |
| `get_reputation(contract_id)` | anyone — stake, verified, lifetime slashed, lock expiry |
| `get_contract_profile(contract_id)` | anyone — the entry and its reputation in one call |
| `get_active_profiles(offset, limit)` | anyone — `get_active_contracts` with reputation attached |
| `get_stake` / `is_verified` / `get_slashes` / `get_staking_config` | anyone |

Verified status has no non-governance path: a registrant cannot attest their own
contract, which is the entire value of the signal. Slashes move stake to the
treasury and record their reason on-chain permanently, so a penalty stays
auditable long after the stake it was taken from is gone.

**Good standing**, the condition for `withdraw_stake`, is three things: you are
the registered owner, the registration is deactivated (you get collateral back
by leaving, not while still listed), and no slash has landed within the last
`SLASH_LOCK_LEDGERS` (~24 h). The lock is what stops an owner emptying the stake
the moment a first slash reveals they are being watched.

Staking is closed until governance runs `propose_configure_staking` to name a
SEP-41 token (native XLM via its Stellar Asset Contract works) and a treasury.
Routing that through governance rather than `initialize` means the already-live
registry can adopt staking after an upgrade instead of being redeployed.

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

### Interface snapshot

[registry/interface.snap](./registry/interface.snap) is the registry's exported
interface as read from the built wasm's contract spec: every function signature,
struct, union, enum and error code, one per line and without doc comments.
`cargo test` compares the current build against it, so CI fails on any change
nobody reviewed, and the failure message lists the lines that changed.

To accept an intended change, run one line after the wasm build and commit the
updated snapshot along with the change:

```bash
UPDATE_INTERFACE_SNAPSHOT=1 cargo test --test interface
```

The snapshot diff in the PR is the review. A line that is only added is usually
safe. A line that changes or disappears breaks the consumers described in
[What breaks downstream](#what-breaks-downstream-when-the-interface-changes).

## Deploying

Deployed on **testnet** at:

```
CAYUDQPV3RKPM3EXDFGI3457FV677JLUCJ4OLKWGCUBPRIHYKXK3WFAZ
```

[lumina-backend](https://github.com/Lumeeena/lumina-backend)'s indexer polls this contract for discovery when configured with `REGISTRY_CONTRACT_ID` (see that repo's README). See [DEPLOY.md](./DEPLOY.md) for the deployment steps used, and how to register your own contract.

## License

MIT
