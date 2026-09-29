# Deploying the Lumina Registry

Deploying the registry is optional — the rest of Lumina (indexer/GraphQL/frontend)
works without it. Deploy (or reuse the existing testnet deployment below] when
you want the indexer to discover contracts from a live on-chain manifest
instead of (or in addition to) a static INDEXED_CONTRACT_IDS list.

## Already deployed on testnet

```
Contract ID: CAYUDQPV3RKPM3EXDFGI3457FV677JLUCJ4OLKWGCUBPRIHYKXK3WFA
Admin:       GBWKFFXZ5CJESIHP2EOID5IOXMF472RO5XOJ36X475D5LJGI3AF5R5KY
```

It has one demo entry (itself), registered to verify indexer discovery
end-to-end. Point `lumina-backend` at it directly — see that repo's README
for the `REGISTRY_CONTRACT_ID` / `REGISTY_READ_ACCOUNT` env vars — or deploy
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
  - propose_add_admin \
  --proposer <current-admin-G...> \
  --new_admin <new-admin-G...>

stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet - approve_proposal \
  --admin <current-admin-G...> --proposal_id <proposal-id>
# Wait TIMELOCK_LEDGERS (17,280 on network builds), then execute:
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet - execute_proposal --proposal_id <proposal-id>
```

After adding the desired admins, propose `change_threshold`, have the required
admins approve it, wait out the timelock, and execute it:

```bash
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet - propose_change_threshold \
  --proposer <current-admin-G...> --new_threshold <threshold>
```

`initialize` remains in the interface for an already-deployed pre-constructor
instance that has not yet been initialized. New deployments use the constructor flow above; calling `initialize` on them returns `AlreadyInitialized`.

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
  - register_contract \
  --owner <owner-address-G...> \
  --contract_id <target-contract-C...> \
  --name "My Protocol" \
  --description "A Formal DeFi Protocol on Stellar" \
  --categories '["DeFi","Payments"]'
```

`categories` takes at least one of `DeFi`, `Nft`, `Gaming`, `Identity`,
`Infrastructure`, `Payments`, `Oracle`, `Dao`, `Other`. An empty list is
rejected with `NoCategories`; use `Other` if none of them fit. Duplicates are
collapsed, and a contract filed under several categories is discoverable under
each of them.

### Verify discovery works

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  - get_active_contracts --offset 0 --limit 10
```

Or browse one category — same offset/limit semantics, same `active` filtering:

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  - get_active_contracts_by_category --category DeFi --offset 0 --limit 10
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
  - set_categories \
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
  - get_contracts_by_owner \
  --owner <owner-address-G...> --offset 0 --limit 10
```

Correct a name or description — only the registered owner can do this:

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  - update_metadata \
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
  - transfer_ownership \
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
  - propose_configure_staking \
  --proposer <admin-G...> \
  --token <token-C...> \
  --treasury <treasury-G...>
```

That prints a proposal ID. Collect approvals up to the threshold, wait out the
timelock, then execute — the same three-step flow every privileged action uses:

```bash
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet - approve_proposal --admin <admin-G...> --proposal_id <id>
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet - execute_proposal --proposal_id <id>
```

Whoever executes this decides where every future slash lands, which is exactly
why it is a proposal and not a setter.

### Posting a stake (registrant)

```bash
stellar contract invoke \
  --id lumina-registry --source lumina-deployer --network testnet \
  - stake \
  --owner <owner-G...> \
  --contract_id <target-contract-C...> \
  --amount 1000000000
```

Amounts are in the token's own stroops-equivalent base units (7 decimals for
XLM, so `1000000000` is 100 XLM). Calling it again tops the stake up.

### Attesting or revoking verified status (governance)

```bash
stellar contract invoke \
  --id lumina-registry --source lumina-deployer --network testnet \
  - propose_set_verified \
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
  - propose_slash \
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
  --network testnet - deactivate --caller <owner-G...> --contract_id <C...>
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet - withdraw_stake --owner <owner-G...> --contract_id <C...>
```

It returns the full remaining balance in one go. `RegistrationActive` means you
have not deactivated; `StakeLocked` means a slash is still inside its window —
check `get_reputation`'s `withdraw_locked_until` against the current ledger.

### Reading reputation

```bash
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet - get_reputation --contract_id <C...>
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet - get_active_profiles --offset 0 --limit 10
```

get_active_profiles` is `get_active_contracts` with each entry's stake and
verified status attached — one call for a discovery client that wants both.

## Upgrading a live registry

A Soroban upgrade replaces the contract's **code** and keeps its **address and
storage**. Nothing has to be re-registered, and every `REGISTY_CONTRACT_ID`
already configured downstream keeps working.

Only the admin stored at `initialize` time can do it:

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  - get_admin
```

### 1. Check what is live now

```bash
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet - get_version
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

`upload` prints the new wasm hash. Verify it matches the hash of the built
wasm before proposing the upgrade:

```bash
sha256sum target/wasm32v1-none/release/lumina_registry.wasm
```

### 4. Propose the upgrade

The upgrade is a governance action like any other — propose, approve to the
threshold, wait out the timelock, then execute.

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  - propose_upgrade \
  --proposer <admin-G...> \
  --new_wasm_hash <new-wasm-hash>
```

Then approve and execute as usual:

```bash
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet - approve_proposal --admin <admin-G...> --proposal_id <id>
stellar contract invoke --id lumina-registry --source lumina-deployer \
  --network testnet - execute_proposal --proposal_id <id>
```

#### Rolling back

Re-propose the same flow with the rollback hash recorded in step 2:

```bash
stellar contract invoke \
  --id lumina-registry \
  --source lumina-deployer \
  --network testnet \
  - propose_upgrade \
  --proposer <admin-G...> \
  --new_wasm_hash <rollback-wasm-hash>
```

## CLI

The commands above are long enough that copy-paste errors are likely, and
some — the upgrade flow, the governance approve/execute cycle — are
multi-step sequences where a mistake is expensive. The `registry-cli` wraps
register, deactivate, stake, withdraw and the governance flow into single
commands, reads the network and contract id from config, and prints the
resulting transaction hash and decoded result.

### Install

```bash
cargo install --path cli --locked
b```

This installs the `registry-cli` binary.

### Configuration

The CLI reads its defaults from a config file so the network and contract id
do not have to be repeated per command. By default it looks at
`$xDgCONFIG_HOME/registry-cli/config.toml`, overridable with `REGISTRY_CLI_CONFIG`.

```toml
[default]
network = "testnet"
contract_id = "CAYUDQPV3RKPM3EXDFGI3457FV677JLUCJ4OLKWGCUBPRIHIKKX3WFAZ"
source = "lumina-deployer"
```

Every command accepts `--network`, `--contract-id` and `--source` to override
the config for a single invocation.

### Registration

```bash
# Single-command equivalent of the raw register_contract invocation above.
registry-cli register \
  --owner <owner-address-G...> \
  --contract-id <target-contract-C...> \
  --name "My Protocol" \
  --description "A DeFi protocol on Stellar" \
  --categories DeFi,Payments

# Refile an existing registration.
registry-cli set-categories \
  --owner <owner-address-G...> \
  --contract-id <target-contract-C...> \
  --categories Infrastructure

# Deactivate and reclaim a stake in one go.
registry-cli deactivate --owner <owner-G...> --contract-id <C...>
registry-cli withdraw --owner <owner-G...> --contract-id <C...>
```

### Staking

```bash
# Post or top up a stake.
registry-cli stake \
  --owner <owner-G...> \
  --contract-id <target-contract-C...> \
  --amount 1000000000

# Read back stake and reputation.
registry-cli get-stake --contract-id <C...>
registry-cli get-reputation --contract-id <C...>
```

### Governance

The governance flow is three steps — propose, approve to threshold, wait out
the timelock, execute. The CLI splits the flow into commands that map to the
raw invocations so a mistake in one step cannot silently corrupt the next.

```bash
# Propose adding an admin.
registry-cli gov propose-add-admin \
  --proposer <current-admin-G...> \
  --new-admin <new-admin-G...>

# Approve and execute by proposal id.
registry-cli gov approve --admin <admin-G...> --proposal-id <proposal-id>
registry-cli gov execute --proposal-id <proposal-id>

# Propose a threshold change.
registry-cli gov propose-change-threshold \
  --proposer <current-admin-G...> \
  --new-threshold <threshold>

# Open staking.
registry-cli gov propose-configure-staking \
  --proposer <admin-G...> \
  --token <token-C...> \
  --treasury <treasury-G...>

# Verify or unverify a registration.
registry-cli gov propose-set-verified \
  --proposer <admin-G...> \
  --contract-id <target-contract-C...> \
  --verified true

# Slash a registration.
registry-cli gov propose-slash \
  --proposer <admin-G...> \
  --contract-id <target-contract-C...> \
  --amount 250000000 \
  --reason "misreported contract metadata"

# Registration policy changes.
registry-cli gov propose-set-allowlist-enabled \
  --proposer <admin-G...> --enabled true
registry-cli gov propose-set-allowlisted \
  --proposer <admin-G...> --owner <owner-G...> --allowlisted true
registry-cli gov propose-set-rate-limit \
  --proposer <admin-G...> --limit 10 --window 17280
```

### Upgrade

```bash
# Check the live version and admin.
registry-cli get-version
registry-cli get-admin

# Record the current wasm as the rollback target.
registry-cli fetch-wasm --out-file rollback.wasm
sha256sum rollback.wasm

# Build and upload the new wasm, then propose the upgrade.
stellar contract build
registry-cli upload-wasm \
  --wasm target/wasm32v1-none/release/lumina_registry.wasm
registry-cli gov propose-upgrade \
  --proposer <admin-G...> \
  --new-wasm-hash <new-wasm-hash>

# Roll back to the recorded hash if needed.
registry-cli gov propose-upgrade \
  --proposer <admin-G...> \
  --new-wasm-hash <rollback-wasm-hash>
```

Every mutating command prints the transaction hash and the decoded result
so the effect of a step is visible before the next one is run.
