# Lumina Contracts

> Soroban smart contracts for Lumina, an open-source event indexer and GraphQL data layer for the Stellar network.

Part of the Lumina project, split across three repos:

- [lumina-frontend](https://github.com/Lumeeena/lumina-frontend) — Next.js explorer UI
- [lumina-backend](https://github.com/Lumeeena/lumina-backend) — indexer + GraphQL API + PostgreSQL schema
- [lumina-contracts](https://github.com/Lumeeena/lumina-contracts) — this repo

For a contributor-oriented map of storage, governance, registration, staking,
slashing, and the invariants protected by the test suite, see
[ARCHITECTURE.md](./ARCHITECTURE.md).

## Where the registry fits

Lumina indexes Soroban contract events, but an indexer has to know *which*
contracts to watch. Without this contract, that list is a static
`INDEXED_CONTRACT_IDS` env var that an operator edits by hand. The registry
replaces the hand-edited list with an on-chain one. A project lists itself by
calling `register_contract`, and every Lumina indexer pointed at the registry
starts indexing that project's events on its next poll. No operator needs to
act and no one needs to redeploy. Discovery is the reason the contract exists.
Categories, staking and governance all exist to make that list worth trusting.

```
 project ──register_contract──▶ ┌──────────┐ ◀──get_active_contracts── indexer ──getEvents──▶ project's events
                                │ registry │                              │
 frontend ──read views─────────▶└──────────┘                              ▼
    │                                │ emits contract_registered, …    PostgreSQL / GraphQL
    └───────────── history view ◀────┴──────── indexed like any other contract's events
```

### How the indexer discovers contracts

[lumina-backend](https://github.com/Lumeeena/lumina-backend)'s indexer
(`indexer/src/registry.ts`) turns discovery on when `REGISTRY_CONTRACT_ID` and
`REGISTRY_READ_ACCOUNT` are set. On a timer it then:

1. Calls `get_active_contracts(offset, 50)` through `simulateTransaction`. This
   is a read-only call, so the read account needs no key and pays no fee. It
   keeps paging until a page comes back with fewer than 50 entries, with a
   cap of 20 pages.
2. Takes `contract_id` from each `ContractEntry`. It drops any address that is
   not a `C…` contract address, because the registry accepts any `Address` and
   one account address in the list would make the whole `getEvents` filter fail.
3. Merges those IDs with the static list and indexes their events.

Deactivating a registration therefore stops the indexer from polling that
contract. Already-indexed events stay in the database.

A note on paging semantics: `offset` is a position in *registration order*,
counting deactivated entries, not a count of active ones. A page can come back
with fewer than `limit` entries even when more active registrations follow,
because the page skipped over deactivated entries. The same holds for
`get_active_profiles` and `get_active_contracts_by_category`.

### What the frontend reads

[lumina-frontend](https://github.com/Lumeeena/lumina-frontend)'s `/registry`
page (`lib/registry.ts`) reads the contract directly over Soroban RPC with the
same simulate-only pattern. It uses `get_active_contracts`,
`get_active_profiles`, `get_active_contracts_by_category`, `get_categories`,
`get_contracts_by_owner`, `get_reputation` and `get_slashes`.

The contract stores only current state. `ContractEntry.active` is a boolean,
not a log, so "when was this deactivated, and by whom?" cannot be read from
storage. The registration history view (`lib/registryHistory.ts`) rebuilds
that history from the registry's **own events**. The indexer stores them
because the registry is itself a registered contract, and the frontend queries
them from the GraphQL API filtered to `contractId = <registry>`:

| Event topic | Data tuple | History row |
| --- | --- | --- |
| `contract_registered` | `(contract_id, owner, name, categories)` | Registered |
| `contract_deactivated` | `(contract_id, caller)`, or `(contract_id, "governance")` when deactivated by proposal | Deactivated |
| `metadata_updated` | `(contract_id, owner, name)` | Metadata updated |
| `ownership_transferred` | `(contract_id, previous_owner, new_owner)` | Ownership transferred |

The history view matches on the **first topic** and treats the **first
element of the data tuple** as the registration the event concerns. Other
events (`categories_updated`, `stake_*`, `proposal_*`, `registry_upgraded`, …)
still appear in the history, shown as a generic "Registry event" row.

### What breaks downstream when the interface changes

Neither sibling repo generates bindings from this contract. Both call methods by
name, with arguments built by hand, and decode results as plain JS objects. A
change here does not fail their builds. It fails at runtime, often quietly:

| Change here | Effect downstream |
| --- | --- |
| Rename or remove `get_active_contracts`, or change its arguments | Indexer discovery fails every poll. Registered contracts stop being indexed, and the static list keeps working, which hides the failure. |
| Rename a `ContractEntry` field (e.g. `contract_id`) | The indexer reads `undefined` IDs, filters them all out, and discovers nothing. The frontend renders blank rows. |
| Change `offset`/`limit` semantics or the page cap | The indexer and the frontend stop paging too early or too late, so contracts are silently missed or duplicated. |
| Change the arguments of `register_contract` | Every registrant's scripts and bindings break. This happened when `categories` was added. |
| Rename an event topic, or move `contract_id` out of the first data slot | History rows turn into "Registry event" rows or lose their subject, so the per-contract history is empty. |
| Add or reorder `Category` variants | The frontend's `CATEGORIES` list no longer matches, and category filters drop unknown values. |

`registry/tests/interface.rs` guards the function and type half of this list.
See [Interface snapshot](#interface-snapshot). Event topics and payloads are
not part of the contract spec, so review changes to `env.events().publish`
calls against the table above.

## Lumina Registry

`registry/` — an on-chain manifest of Soroban contracts registered for Lumina indexing. Any project can call `register_contract()` to add their contract; [lumina-backend](https://github.com/Lumeeena/lumina-backend)'s indexer can then discover and index their events.

```rust
registry.register_contract(owner, contract_id, "My Protocol", "A DeFi protocol on Stellar", vec![Category::DeFi])
```

`get_active_contracts_after(cursor, limit)` walks the active registrations for discovery. Pass the `contract_id` of the last entry the previous call returned (`None` to start) and repeat until the page is empty. The cursor is anchored to a registration, so entries added mid-walk are neither duplicated nor skipped. The older `get_active_contracts(offset, limit)` is retained for one release but **deprecated**: it re-reads the index up to `offset` on every page, and a registration inserted mid-walk shifts every later page.

**Example**: See [examples/registry-registrant](./examples/registry-registrant/) for a complete working contract that registers itself during deployment. The example demonstrates integration patterns and includes tests you can copy to your own project.

### Categories

Every registration declares at least one category, so the Registry supports
browsing rather than only a flat list:

`DeFi` · `Nft` · `Gaming` · `Identity` · `Infrastructure` · `Payments` ·
`Oracle` · `Dao` · `Other`

| Method | Who can call it |
| --- | --- |
| `get_contracts_by_category_after(category, cursor, limit)` | anyone — cursor over one category; preferred over the offset form |
| `get_active_contracts_by_category(category, offset, limit)` | anyone — deprecated offset form, same paging semantics as `get_active_contracts` |
| `get_categories(contract_id)` | anyone |
| `set_categories(owner, contract_id, categories)` | the registered owner only |
| `prune_category(category)` | anyone — removes dead index references, returns the count removed |
| `prune_all_contracts()` | anyone — same, for the global `AllContracts` index |

A contract can be filed under several categories and is discoverable under each.
Duplicates are collapsed, so passing a category twice indexes it once.

The vocabulary is a fixed enum rather than free-form tags because the point is
browsing, and free-form tags fragment it immediately — `DeFi`, `defi` and `De-Fi`
become three categories each holding part of the answer. Adding a category is a
contract upgrade; `Other` is the escape hatch until then.

`deactivate` does not rewrite category indices. `get_active_contracts_by_category`
filters on `active`, exactly as the global listing does, which is what keeps a
deactivated registration out of browsing.

`deregister` is the opposite: it deletes the entry itself (owner only, must
already be deactivated and fully unstaked) and eagerly removes it from the
global, owner, and every category index, so a deregistered contract leaves no
index reference. Slash records are kept for auditability. Storage archival can
still strand a reference the eager paths never saw — `prune_category` /
`prune_all_contracts` cover that case. They are permissionless and idempotent
(safe to call repeatedly; a second call removes nothing and returns 0), so an
indexer or a cron-like caller can pay for the cleanup on a schedule.

Registrations are also manageable after the fact:

| Method | Who can call it |
| --- | --- |
| `get_contracts_by_owner_after(owner, cursor, limit)` | anyone — cursor form, includes the owner's deactivated entries |
| `get_contracts_by_owner(owner, offset, limit)` | anyone — deprecated offset form, includes the owner's deactivated entries |
| `update_metadata(owner, contract_id, name, description)` | the registered owner only |
| `set_manager(owner, contract_id, manager)` | the registered owner only — grants the manager a subset of rights |
| `transfer_ownership(caller, contract_id, new_owner)` | the current owner or the admin |
| `deactivate(caller, contract_id)` | the current owner or the admin |
| `deregister(owner, contract_id)` | the registered owner only — entry must be deactivated and fully unstaked |
| `stake(staker, contract_id, amount)` | anyone — a third party may stake on a registration's behalf |
| `withdraw_stake(staker, contract_id, amount)` | the staker only — each staker withdraws only their own stake |
| `get_stake(contract_id)` | anyone — total staked across all stakers |
| `get_stake_of(contract_id, staker)` | anyone — the amount a single staker has on a registration |

### Staking

Stake is tracked per `(registration, staker)` rather than per registration
alone, so a backer who wants to vouch for a project can do so without owning
it. The total reported for a registration (`get_stake`) is the sum of every
staker's balance.

Slashing policy: when a registration is slashed, the penalty is applied
**pro-rata across all stakers** — each staker loses the same fraction of their
stake, so no staker is preferred over another and the relative weights of the
backers are preserved. The slash record stores the total amount taken; the
per-staker reductions are reflected in each staker's balance, and each staker
can still withdraw whatever remains of their own contribution.

Counters: `get_contract_count` is the live total (deactivated included,
deregistered excluded), `get_total_registered` is the lifetime total
(never decremented), and `get_active_contract_count` is the currently listed
figure. The frontend stats page should read `get_active_contract_count`.

### Delegated management

Teams often operate from a multisig or a deliberately cold deployer key.
Requiring that key for routine metadata edits means either using it too often
or not editing at all. An owner can therefore delegate registration management
to a manager address:

| Method | Who can call it |
| --- | --- |
| `set_manager(owner, contract_id, manager)` | the registered owner only — sets or replaces the manager |
| `clear_manager(owner, contract_id)` | the registered owner only — revokes immediately |
| `get_manager(contract_id)` | anyone — the current manager, if any |

A manager may:

- `update_metadata(manager, contract_id, name, description)`
- `set_categories(manager, contract_id, categories)`
- `deactivate(manager, contract_id)`

A manager may **not** transfer ownership or withdraw stake — the two actions
that move value. Those remain owner-only (or admin, for `transfer_ownership`
and `deactivate`). Revocation via `clear_manager` is immediate: the next call
from the former manager fails with `Unauthorized`.

### Batched governance actions

`propose_batch(proposer, actions)` creates one proposal containing 1–10
`ProposalAction` values. Only an authenticated admin may propose it. Nested
`ProposalAction::Batch` values are rejected. Approval and timelock requirements
are the same as for individual proposals.

Actions execute in their supplied order. For example, add two replacement admins
before removing the old admin, then change the threshold. Each action validates
against the state produced by earlier actions. If any action fails, all storage
changes, events and token transfers revert, including the proposal's execution
marker; governance can retry the proposal once the cause is resolved.

The new `Batch` union variant preserves the encoding of existing individual
actions and adds no storage keys or fields to stored structs. Clients submitting
batches need bindings that include `Batch` and `propose_batch`.

### Upgrades

The registry is upgradeable in place, so a fix or a new entrypoint does not
orphan existing registrations at a new address:

| Method | Who can call it |
| --- | --- |
| `get_version()` | anyone — which build is live at this address |
| `get_admin()` / `get_admins()` | anyone |
| `propose_upgrade(proposer, new_wasm_hash)` | any admin — opens an upgrade proposal |
| `approve_proposal(admin, proposal_id)` | any admin — counts toward the threshold |
| `execute_proposal(proposal_id)` | anyone, once threshold **and** timelock are met |
| `get_manager(contract_id)` | anyone — the delegated manager for a registration, if set |

There is **no single-signer upgrade path**. Changing the code is governance-only:
`propose_upgrade` by an admin, enough `approve_proposal` calls to reach the
threshold, then `execute_proposal` after the timelock. The swap keeps the
contract's address and storage, so a new version must stay compatible with the
storage shapes documented on `DataKey` and `ContractEntry` in
[registry/src/lib.rs](./registry/src/lib.rs). See
[DEPLOY.md](./DEPLOY.md#upgrading-a-live-registry) for the live runbook.

### Slashing and staker rewards

A slash does not send the whole stake to the treasury. `slash` splits the
slashed amount by a governance-set proportion: one part goes to the treasury
address, the rest is credited to a staker reward pool. The split is configured
with `set_slash_split(admin, treasury_bps)`, where `treasury_bps` is the
treasury's share in basis points (0–10000). The remainder, `10000 -
treasury_bps`, is the stakers' share. `get_slash_split()` returns the current
value. With `treasury_bps = 10000` the behaviour matches the old contract
exactly: the treasury receives the full slashed amount and the reward pool
stays empty.

Distribution is a **claim**, not a push. A slash credits the reward pool and
records the slashed amount; it does not iterate over stakers. A push would have
to loop over every honest staker on every slash, so the cost of a slash would
scale with the number of stakers — an attacker could make slashing unaffordable
by staking from many addresses, and a slash against a popular registration
could exceed the transaction's resource limits. A claim inverts that: the slash
is O(1), and each staker pays the cost of their own withdrawal when they choose
to collect.

| Method | Who can call it |
| --- | --- |
| `set_slash_split(admin, treasury_bps)` | the admin only — `treasury_bps` must be ≤ 10000 |
| `get_slash_split()` | anyone — the current treasury share in basis points |
| `claim_staker_reward(staker)` | a staker with an unclaimed share |
| `get_claimable_reward(staker)` | anyone — the staker's unclaimed share |

A staker's share is proportional to their stake on registrations other than
the slashed one, so staking becomes a judgement about which registrations are
honest rather than a lottery. Claiming transfers the staker's share and zeroes
it; a second claim returns nothing. The reward pool is funded only by slashes,
so a registry with no slashes has nothing to claim.

### Error codes

`RegistryError` crosses the contract boundary as a bare `u32`, so the
numeric code is the API a caller actually sees. The table below is the
reference for those codes; it is kept next to the enum in
[registry/src/lib.rs](./registry/src/lib.rs) so the two are updated together.

| Code | Name | Meaning | Usual remedy |
| --- | --- | --- | --- |
| 1 | `AlreadyInitialized` | `initialize` was called on a registry that already has an admin. | Do not call `initialize` again; read `get_admin()` to confirm the live admin, and use `upgrade` for code changes. |
| 2 | `NotInitialized` | A method that needs an admin ran before `initialize`. | Call `initialize(admin)` once, then retry the original call. |
| 3 | `Unauthorized` | The caller is not the admin or the registered owner for this action. | Re-sign the transaction with the admin key or the entry's current owner; check `get_contracts_by_owner` if the owner is unclear. |
| 4 | `ContractNotFound` | No registration exists for the given `contract_id`. | Verify the ID against `get_active_contracts` / `get_contracts_by_owner`; register it first if it was never listed. |
| 5 | `ContractAlreadyRegistered` | The `contract_id` is already in the registry. | Use `update_metadata` or `set_categories` to change the existing entry instead of registering again. |
| 6 | `ContractNotActive` | The entry exists but is deactivated, so the action requires an active registration. | Reactivate by re-registering, or pick a different contract; `get_active_contracts` lists only active entries. |
| 7 | `ContractStillActive` | `deregister` was called on an entry that is still active. | Call `deactivate(caller, contract_id)` first, then `deregister`. |
| 8 | `InvalidName` | The supplied name is empty or exceeds the length limit. | Pass a non-empty name within the documented byte limit. |
| 9 | `InvalidDescription` | The supplied description exceeds the length limit. | Shorten the description to fit the limit. |
| 10 | `InvalidCategory` | The category list is empty or contains a value outside the `Category` enum. | Pass at least one valid `Category` variant; see the Categories section for the current vocabulary. |
| 11 | `TooManyCategories` | More categories were supplied than the entry allows. | Trim the list to the maximum number of categories per registration. |
| 12 | `StakeNotFound` | `withdraw_stake` was called for an entry with no stake. | Stake first with `stake(owner, contract_id, amount)`, or skip the withdrawal. |
| 13 | `InsufficientStake` | The requested slash or withdrawal exceeds the staked amount. | Lower the amount to at most `get_stake(contract_id)`, or have the owner top up the stake. |
| 14 | `StakeLocked` | The stake is still locked, so it cannot be withdrawn yet. | Wait until the lock expiry reported by `get_reputation(contract_id)` has passed, then retry. |
| 15 | `NotVerified` | The action requires a verified registration, but the entry is not verified. | Have an admin run `propose_set_verified(proposer, contract_id, true)` and wait out the timelock. |
| 16 | `AlreadyVerified` | `propose_set_verified` was called with the value the entry already has. | Skip the proposal; read `is_verified(contract_id)` before proposing. |
| 17 | `ProposalNotFound` | No governance proposal exists for the given ID. | List proposals and retry with a valid ID; the proposal may have already been executed or cancelled. |
| 18 | `ProposalNotReady` | The proposal exists but its timelock has not elapsed. | Wait until the proposal's execution ledger, then call `execute_proposal` again. |
| 19 | `ProposalAlreadyExecuted` | The proposal was already executed or cancelled. | Do not re-execute; read the proposal's final state to confirm the outcome. |
| 20 | `RegistrationLimitReached` | The per-owner registration limit is enabled and this owner has hit it. | Deregister an unused entry, or have an admin raise the limit via `propose_configure_registration_rate_limit`. |
| 21 | `OwnerContractLimitReached` | The owner already has the maximum number of registrations in their contract index. | Deregister an unused entry owned by this address, then retry the registration. |

### Staking & reputation

Registration stays free and permissionless by default — anyone can list a
contract for indexing. Governance can optionally enable an allowlist or a
per-owner registration limit for curated deployments. On top of that, a
registrant can post collateral, and governance can attest or penalise, so
consumers of the Registry can tell a well-run project apart from a name that
was typed into a form:

| Method | Who can call it |
| --- | --- |
| `stake(owner, contract_id, amount)` | the registered owner — additive, tops up an existing stake |
| `withdraw_stake(owner, contract_id)` | the registered owner, in good standing (see below) |
| `propose_set_verified(proposer, contract_id, verified)` | an admin — takes effect only after approval + timelock |
| `propose_slash(proposer, contract_id, amount, reason)` | an admin — same |
| `propose_configure_staking(proposer, token, treasury)` | an admin — same |
| `propose_set_allowlist_enabled(proposer, enabled)` | an admin — same |
| `propose_set_allowlisted(proposer, owner, allowed)` | an admin — same |
| `propose_set_rate_limit(proposer, limit, window_ledgers)` | an admin — same; zero limit disables it |
| `get_reputation(contract_id)` | anyone — stake, verified, lifetime slashed, lock expiry |
| `get_contract_profile(contract_id)` | anyone — the entry and its reputation in one call |
| `get_active_profiles(offset, limit)` | anyone — `get_active_contracts` with reputation attached |
| `get_stake` / `is_verified` / `get_slashes` / `get_staking_config` | anyone |

### Per-owner registration limit

Each owner's contract index (`DataKey::OwnerContracts(Address)`) is capped at
`MAX_OWNER_CONTRACTS` entries. Registering past the cap fails with
`OwnerContractLimitReached` (code 21) instead of an opaque storage failure, so
an owner at the limit gets a clear error. Registrations below the cap are
unaffected. Deregistering an entry frees a slot in the index.

Verified status has no non-governance path: a registrant cannot verify their own
contract, which is the entire value of the signal. (Permissionless third-party
`attest` exists and is documented below, but it records a separate, weaker claim
and cannot reach `Verified`.) Slashes move stake to the
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

**Token compatibility note:** the registry tracks every deposited stake exactly
and expects the contract's real token balance to match the sum of all individual
stakes at all times.  **Fee-on-transfer tokens are not supported**: because the
registry credits the full transfer `amount` while the contract receives
`amount - fee`, the two figures diverge immediately, and any subsequent slash
will fail with `ContractBalanceInsufficient` (error 29).  Use only standard
SEP-41 tokens where `transfer(from, to, amount)` delivers exactly `amount` to
the recipient.  If this invariant is ever violated for any other reason (rounding
bug in a custom token, tokens sent directly out of the contract), the same
`ContractBalanceInsufficient` error is raised before the slash transfer, making
the discrepancy diagnosable rather than causing an opaque panic deep inside the
token contract.

### Third-party attestations

Any address can vouch for a registration with a short, bounded label. This is a
transparency feature rather than a trust signal:

| Method | Who can call it |
| --- | --- |
| `attest(attester, contract_id, label)` | anyone, including the registration's own owner |
| `revoke_attestation(attester, contract_id)` | the attester, and only for their own attestation |
| `get_attestations(contract_id)` | anyone — `(attester, label, created_at)`, oldest first |

Two properties are deliberate. The attester's address is recorded on-chain, so a
claim is attributable rather than anonymous, and the attester can withdraw it
themselves without asking anyone. And `revoke_attestation` is scoped to the
caller's own record: no admin, and not even the registration's owner, can remove
another party's attestation, because a claim should last exactly as long as the
party making it stands behind it.

Attestations are **not** verification and never feed into it. `Verified` remains
governance-only, set through a threshold-and-timelocked proposal, and there is no
counter or path by which attaching many attestations could substitute for that —
so nobody can inflate the verified signal by attaching cheap labels. Consumers
that want to weight the two differently can, and can surface the attester either
way.

One attestation per attester per registration: re-attesting revises the existing
label instead of appending, so a stale claim cannot be left behind. Labels are
bounded to 64 bytes and non-empty, and a registration holds at most 20
attestations, so the cost of reading a registration's attestations is a property
of the contract rather than of how many parties choose to speak up.

## Build & Test

Install GNU Make, the Rust stable toolchain, and the wasm targets:

```bash
rustup target add wasm32v1-none wasm32-unknown-unknown
rustup component add rustfmt clippy
```

Run the same full check used by CI, or run individual targets:

```bash
make check
make build
make test
make fmt
make clippy
make wasm-both
```

`make test` builds the release wasm for the workspace before running tests. The
upgrade tests deploy the registry from its compiled wasm — the only form a
Soroban upgrade can be performed on — and upgrade it to `registry-v2/`, a
deliberately minimal second version that exists only as that test's upgrade
target and is never deployed. `make check` runs formatting and clippy checks
before the build-and-test sequence.

Ship `wasm32v1-none`, not `wasm32-unknown-unknown`; on current Rust the latter
emits the reference-types proposal, which the Soroban host refuses to load.
Both targets are built anyway — `make wasm-both`, which `make test` runs — so
that the host-compatibility test in
[registry/tests/wasm_targets.rs](./registry/tests/wasm_targets.rs) has the
artifacts of both to load: the ones we ship have to be accepted, and the other
ones have to be refused for the documented reason, so that neither claim can go
stale unnoticed. CI builds both targets before running the suite.

### Upgrading the Rust Toolchain

The project pins its Rust compiler version using a `rust-toolchain.toml` file to ensure that CI and local builds compile with the exact same compiler. A floating toolchain can cause unexpected breakages (such as the reference-types proposal being emitted by newer Rust versions on `wasm32-unknown-unknown`).

To upgrade the compiler version:
1. Update the `channel` value in `rust-toolchain.toml` to the new stable version.
2. Ensure `targets = ["wasm32v1-none", "wasm32-unknown-unknown"]` remains present in the file: the first is what ships, the second is what CI checks the host's verdict on.
3. Re-run `make check` locally to verify the new compiler version doesn't introduce any new build errors, warnings or wasm the Soroban host refuses to load.
4. Commit the updated `rust-toolchain.toml` file and open a PR. CI will automatically honor the newly pinned version instead of defaulting to `stable`.

### Interface snapshot

[registry/interface.snap](./registry/interface.snap) is the registry's exported
interface as read from the built wasm's contract spec: every function signature,
struct, union, enum and error code, one per line and without doc comments.
`make test` compares the current build against it, so CI fails on any change
nobody reviewed, and the failure message lists the lines that changed.

To accept an intended change, run one line after the wasm build and commit the
updated snapshot along with the change:

```bash
make build
UPDATE_INTERFACE_SNAPSHOT=1 cargo test --test interface
```

The snapshot diff in the PR is the review. A line that is only added is usually
safe. A line that changes or disappears breaks the consumers described in
[What breaks downstream](#what-breaks-downstream-when-the-interface-changes).

### Upgrade fixture

`registry-v2/` is a hand-maintained copy of the registry's storage types, kept
byte-compatible so the upgrade tests prove that independently written v2 types
decode v1 storage. `cargo test` compares its type definitions field-for-field
against `registry/src/lib.rs` and fails CI when they diverge.

## Deploying

Deployed on **testnet** at:

```
CAYUDQPV3RKPM3EXDFGI3457FV677JLUCJ4OLKWGCUBPRIHYKXK3WFAZ
```

**Automated deployment**: Use [scripts/deploy.sh](./scripts/deploy.sh) to deploy or upgrade the registry with automatic wasm hash tracking and rollback capability. See [scripts/README.md](./scripts/README.md) for usage.

**TypeScript bindings**: Generate type-safe client bindings with [scripts/generate-bindings.sh](./scripts/generate-bindings.sh) to eliminate hand-written clients and prevent silent breakage when the interface changes.

When a storage type changes, update `registry-v2/` in the same PR so the fixture
keeps mirroring the real types, then re-run `cargo test`. If you changed
`ContractEntry` without updating the fixture, CI fails and the message names the
divergent fields and points back here.

## Dependencies & Supply Chain Review

This repository maintains a minimal dependency surface to minimize attack vectors, ensure strict `no_std` compliance, and keep compiled WebAssembly contract sizes small.

### Direct Dependencies

- **`soroban-sdk` (v22.0.0, workspace)**:
  - **Why needed**: Core Soroban framework providing smart contract host abstractions, env bindings (`Env`, `Address`, `Vec`, `String`, `BytesN`, `Symbol`), token client bindings (`soroban_sdk::token::Client`), contract macros (`#[contract]`, `#[contractimpl]`, `#[contracttype]`, `#[contracterror]`), and storage access APIs.
  - **Features**: Enabled with `alloc` feature for linear memory allocations in `no_std` WebAssembly runtime.
- **`soroban-sdk` with `testutils` (dev-dependencies)**:
  - **Why needed**: In-memory test environment, mock authorizations (`mock_all_auths`, `MockAuth`), and contract client test generation.

### Supply Chain & `no_std` Guarantees

- **`no_std` Contract Execution**: Smart contracts in this workspace are strictly `#![no_std]`. They do not link the standard library or depend on OS-level system calls.
- **Pinned `ed25519-dalek`**: `ed25519-dalek` is pinned (v2.2.0) via `soroban-env-host` for cryptographic Ed25519 signature checks in off-chain host and test simulation environments (`testutils`). It is an off-chain/host dependency and is **never** linked into the deployed wasm bytecode on-chain (where cryptographic operations are provided natively by Soroban host functions).
- **Automated Security Audits**: CI runs `cargo audit` against the RustSec Advisory Database on every pull request and push to main to detect known vulnerabilities.

## License

MIT
