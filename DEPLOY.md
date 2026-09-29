# Deploying the Lumina Registry

Deploying the registry is optional — the rest of Lumina (indexer/GraphQL/frontend)
works without it. Deploy (or reuse the existing testnet deployment below] when
you want the indexer to discover contracts from a live on-chain manifest
instead of (or in addition to) a static `INDEXED_CONTRACT_IDS` list.

## Already deployed on testnet

```
Contract ID: CAYUDQPV3RKPM3EXDFGI3457FV677JLUCJ4OLKWGCUBPRINYKXK3WFAZ
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
  --alias lumina-registry \
  -- --bootstrap_admin lumina-deployer
```

This prints the deployed contract's `C...` address — save it as `REGISTRY_CONTRACT_ID`.
If the deploy step fails with `HostError: Error(Storage, MissingValue)` /
"Wasm does not exist", that's just RPC propagation lag after the upload —
rerun the same `deploy` command a few seconds later; it skips re-uploading
and picks up from the create-contract step.

The registry is initialized atomically by `__constructor` as part of deployment.
The deploying identity must also authorize `bootstrap_admin`; there is no
uninitialized interval for a newly deployed registry.

### Bootstrap multi-sig governance

A new registry starts with one admin and a threshold of one so deployment only
requires one signer. That bootstrap admin can add the remaining admins through
the normal proposal flow, then propose a higher threshold. Until that higher
threshold proposal executes, the registry is effectively single-signer; use a
trusted bootstrap key and complete the transition promptly.

For each additional admin, propose, approve, wait for the timelock, then execute:

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  -- propose_add_admin \
  --proposer <current-admin-G...> \
  --new_admin <new-admin-G...>

stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet -- approve_proposal \
  --admin <current-admin-G...> --proposal_id <proposal-id>
# Wait TIMELOCK_LEDGERS (17,280 on network builds), then execute:
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet -- execute_proposal --proposal_id <proposal-id>
```

After adding the desired admins, propose `change_threshold`, have the required
admins approve it, wait out the timelock, and execute it:

```bash
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet -- propose_change_threshold \
  --proposer <current-admin-G...> --new_threshold <threshold>
```

`lumina-registry-cli` wraps this flow as `governance propose-add-admin`,
`governance approve`, and `governance execute` — see [CLI commands](#cli-commands).

`initialize` remains in the interface for an already-deployed pre-constructor
instance that has not yet been initialized. New deployments use the constructor
flow above; calling `initialize` on them returns `AlreadyInitialized`.

### Registration policy

Registration remains permissionless by default: allowlist mode starts disabled,
and the rate limit starts at zero (disabled). Governance can enable allowlist
mode, add or remove owners from the allowlist, and configure a per-owner count
within a fixed ledger window. Each change uses the standard proposal, approval,
timelock, and execution flow. A zero rate limit disables rate limiting; a
nonzero limit requires a nonzero window. Owners who hit the configured cap
receive `RegistrationRateLimited`, and the counter resets when the window ends.
Windows must fit within the network's maximum persistent-entry TTL so the
counter cannot expire before its configured window.

The governance entrypoints are `propose_set_allowlist_enabled`,
`propose_set_allowlisted`, and `propose_set_rate_limit`.

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
  --description "A DeFi protocol on Stellar" \
  --categories '["DeFi","Payments"]'
```

categories` takes at least one of `DeFi`, `Nft`, `Gaming`, `Identity`,
`Infrastructure`, `Payments`, `Oracle`, `Dao`, `Other`. An empty list is
rejected with `NoCategories`; use `Other` if none of them fit. Duplicates are
collapsed, and a contract filed under several categories is discoverable under
each of them.

With the CLI:

```bash
lumina-registry-cli register \
  --owner <owner-address-G...> \
  --contract-id <target-contract-C...> \
  --name "My Protocol" \
  --description "A DeFi protocol on Stellar" \
  --categories DeFi,Payments
```

### Verify discovery works

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  -- get_active_contracts --offset 0 --limit 10
```

Or browse one category — same offset/limit semantics, same `active` filtering:

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  -- get_active_contracts_by_category --category DeFi --offset 0 --limit 10
```

### Refile an existing registration

Registrations created before the taxonomy existed carry no categories and so
appear in no category listing. Their owners can classify them in place, without
re-registering — this is also how you change categories later:

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  -- set_categories \
  --owner <owner-address-G...> \
  --contract_id <target-contract-C...> \
  --categories '["Infrastructure"]'
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

## Staking, verification and slashing

Registration is free; stake is the optional signal on top of it. Nothing here
works until governance opens staking.

### Opening staking (governance, once)

Name the token stakes are denominated in and the treasury slashed stake goes to.
For native XLM, use the Stellar Asset Contract address for XLM on your network.

```bash
stellar contract invoke \
  --id lumina-registry --source lumina-deployer --network testnet \
  -- propose_configure_staking \
  --proposer <admin-G...> \
  --token <token-C...> \
  --treasury <treasury-G...>
```

That prints a proposal ID. Collect approvals up to the threshold, wait out the
timelock, then execute — the same three-step flow every privileged action uses:

```bash
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet -- approve_proposal --admin <admin-G...> --proposal_id <id>
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet -- execute_proposal --proposal_id <id>
```

Whoever executes this decides where every future slash lands, which is exactly
why it is a proposal and not a setter.

### Posting a stake (registrant)

```bash
stellar contract invoke \
  --id lumina-registry --source lumina-deployer --network testnet \
  -- stake \
  --owner <owner-G...> \
  --contract_id <target-contract-C...> \
  --amount 1000000000
```

Amounts are in the token's own stroops-equivalent base units (7 decimals for
XLM, so `1000000000` is 100 XLM). Calling it again tops the stake up.

With the CLI:

```bash
lumina-registry-cli stake \
  --owner <owner-G...> \
  --contract-id <target-contract-C...> \
  --amount 1000000000
```

### Attesting or revoking verified status (governance)

```bash
stellar contract invoke \
  --id lumina-registry --source lumina-deployer --network testnet \
  -- propose_set_verified \
  --proposer <admin-G...> \
  --contract_id <target-contract-C...> \
  --verified true
```

Then approve and execute as above. There is no direct setter — a registrant
cannot verify themselves.

### Slashing (governance)

```bash
stellar contract invoke \
  --id lumina-registry --source lumina-deployer --network testnet \
  -- propose_slash \
  --proposer <admin-G...> \
  --contract_id <target-contract-C...> \
  --amount 250000000 \
  --reason "misreported contract metadata"
```

The reason is stored permanently and readable later via `get_slashes`. On
execution the amount moves to the treasury, and the registration's *remaining*
stake is frozen for `SLASH_LOCK_LEDGERS` (~24 h) — long enough for a follow-up
slash proposal to clear its own timelock.

A slash for more than the registration has staked passes governance but reverts
at execution with `InsufficientStake`; check `get_stake` before proposing.

### Reclaiming a stake (registrant)

Withdrawal requires **good standing**: you are the registered owner, the
registration is deactivated, and no slash has landed inside the lock window.

```bash
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet -- deactivate --caller <owner-G...> --contract_id <C...>
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet -- withdraw_stake --owner <owner-G...> --contract_id <C...>
```

It returns the full remaining balance in one go. `RegistrationActive` means you
have not deactivated; `StakeLocked` means a slash is still inside its window —
check `get_reputation`'s `withdraw_locked_until` against the current ledger.

With the CLI:

```bash
lumina-registry-cli deactivate --caller <owner-G...> --contract-id <C...>
lumina-registry-cli withdraw --owner <owner-G...> --contract-id <C...>
```

### Reading reputation

```bash
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet -- get_reputation --contract_id <C...>
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet -- get_active_profiles --offset 0 --limit 10
```

`get_active_profiles` is `get_active_contracts` with each entry's stake and
verified status attached — one call for a discovery client that wants both.

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

Save that hash (and the wasm) somewhere durable *bufore* upgrading. Rolling back
is just another `upgrade` to that hash, but only if you still have it.

### 3. Build and upload the new wasm

```bash
stellar contract build
stellar contract upload \
  --wasm target/wasm32v1-none/release/lumina_registry.wasm \
  --source lumina-deployer \
  --network testnet
```

`upload` prints the new wasm hash. Verify it matches the hash of the built
wasm before proposing the upgrade:

```bash
sha256sum target/wasm32v1-none/release/lumina_registry.wasm
```

### 4. Propose the upgrade

The upgrade is a governance action like any other — propose, approve to the
threshold, wait out the timelock, then execute. The proposal carries the new
wasm hash:

```bash
stellar contract invoke \
  --id lumina-registry --source lumina-deployer --network testnet \
  -- propose_upgrade \
  --proposer <admin-G...> \
  --new_wasm_hash <new-wasm-hash>
```

Then approve and execute as above. With the CLI:

```bash
lumina-registry-cli governance propose-upgrade \
  --proposer <admin-G...> \
  --new-wasm-hash <new-wasm-hash>
lumina-registry-cli governance approve --admin <admin-G...> --proposal-id <id>
lumina-registry-cli governance execute --proposal-id <id>
```

### 5. Confirm the upgrade

After execution, get_version should report the new version and the contract
address is unchanged.

```bash
lumina-registry-cli get-version
```

## CLI commands

`lumina-registry-cli` is a thin wrapper around the invocations above. It reads
the network and contract ID from config so you do not repeat them per command, and
it prints the resulting transaction hash and decoded result.

### Configuration

Config is read from `~/.lumina/registry.toml` by default, or from the path in
`VUMINA_REGISTRY_CLI_CONFIG`. Environment variables override the file:

```toml
network = "testnet"
contract_id = "CAYUDQPV3RKPM3EXDFGI3457FV677JLUCJ4OLKWGCUBPRINYKXK3WFAz"
source = "lumina-deployer"
```

| Key           | Env var                       | Default             |
| ------------- | ------------------------------ | -------------------- |
| `network`     | `LUMINA_REGISTRY_NETWORK`      | `testnet`           |
| `contract_id` | `LUMINA_REGISTRY_CONTRACT_ID` | none (required)     |
| `source`      | `LUMINA_REGISTRY_SOURCE`       | none (required)     |

Any command accepts `--network`, `--contract-id`, and `--source` to override the
configured values for a single invocation.

### Commands

Each documented operation has a single-command equivalent:

| Operation                       | CLI command                                                              |
| ------------------------------- | -------------------------------------------------------------------------- |
| Register a contract              | `lumina-registry-cli register --owner <G> --contract-id <C> --name <N> --description <D> --categories DeFi,Payments` |
| Deactivate                       | `lumina-registry-cli deactivate --caller <G> --contract-id <C>`                            |
| Stake                            | `lumina-registry-cli stake --owner <G> --contract-id <C> --amount <A>`                         |
| Withdraw stake                  | `lumina-registry-cli withdraw --owner <G> --contract-id <C>`                             |
| Propose governance action        | `lumina-registry-cli governance propose <kind> --proposer <G> [args...]`                 |
| Approve a proposal               | `lumina-registry-cli governance approve --admin <G> --proposal-id <id>`                   |
| Execute a proposal               | `lumina-registry-cli governance execute --proposal-id <id>`                          |
| Read admin/version/reputation  | `lumina-registry-cli get-admin` / `get-version` / `get-reputation --contract-id <C>`    |

The governance kinds are `add-admin`, `change-threshold`, `configure-staking`,
`set-verified`, `slash`, `set-allowlist-enabled`, `set-allowlisted`,
set-rate-limit`, and `upgrade`. The CLI prints the proposal ID for a propose
command so it can be fed straight into `approve` and `execute`.

For example, the add-admin flow becomes:

```bash
lumina-registry-cli governance propose add-admin \
  --proposer <current-admin-G...> \
  --new-admin <new-admin-G...>
lumina-registry-cli governance approve --admin <current-admin-G...> --proposal-id <id>
lumina-registry-cli governance execute --proposal-id <id>
```

The upgrade flow becomes:

```bash
lumina-registry-cli governance propose upgrade \
  --proposer <admin-G...> \
  --new-wasm-hash <new-wasm-hash>
lumina-registry-cli governance approve --admin <admin-G...> --proposal-id <id>
lumina-registry-cli governance execute --proposal-id <id>
```

Read-only commands do not sign a transaction and so do not print a transaction
hash; they print the decoded result only.
