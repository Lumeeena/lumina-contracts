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

### Upgrades

The registry is upgradeable in place, so a fix or a new entrypoint does not
orphan existing registrations at a new address:

| Method | Who can call it |
| --- | --- |
| `get_version()` | anyone — which build is live at this address |
| `get_admin()` | anyone |
| `upgrade(admin, new_wasm_hash)` | the admin only |
| `get_manager(contract_id)` | anyone — the delegated manager for a registration, if set |

`upgrade` swaps the contract's code and keeps its address and storage, so a new
version must stay compatible with the storage shapes documented on `DataKey` and
`ContractEntry` in [registry/src/lib.rs](./registry/src/lib.rs). See
[DEPLOY.md](./DEPLOY.md#upgrading-a-live-registry) for the live runbook.

### Storage keys and their lifetimes

Every key the registry writes is a `DataKey` variant. The table below lists each
one with its storage type, what it holds, and its expected lifetime, so an
operator can reason about archival without reading the enum plus every call
site.

| Key | Storage | Holds | Lifetime / TTL behaviour |
| --- | --- | --- | --- |
| `Admin` | instance | The registry admin `Address`. | Lives as long as the contract instance; set once by `initialize`, replaced only by `upgrade`-adjacent admin flows. |
| `Version` | instance | The live build's version `u32`. | Lives as long as the contract instance; rewritten on each `upgrade`. |
| `Contract(contract_id)` | persistent | The `ContractEntry` for a registration (owner, name, description, categories, `active`, verified, stake, etc.). | Lives until `deregister` deletes it. `deactivate` keeps the entry, so a deactivated registration still occupies this key. |
| `AllContracts` | persistent | Index `Vec<Address>` of every registered `contract_id` in registration order. | Lives as long as the registry; entries are appended on register and removed eagerly on `deregister`. Index — must stay consistent with `Contract` entries. |
| `OwnerContracts(owner)` | persistent | Index `Vec<Address>` of the `contract_id`s owned by `owner`, deactivated included. | Lives as long as the registry; appended on register and removed eagerly on `deregister`. Index — must stay consistent with `Contract` entries. |
| `CategoryContracts(category)` | persistent | Index `Vec<Address>` of `contract_id`s filed under `category`. | Lives as long as the registry; appended on register and removed eagerly on `deregister`. `deactivate` does not rewrite it. Index — must stay consistent with `Contract` entries. |
| `ContractCount` | instance | Live total of registrations (deactivated included, deregistered excluded). | Lives as long as the contract instance; incremented on register, decremented on `deregister`. |
| `TotalRegistered` | instance | Lifetime total of registrations ever made; never decremented. | Lives as long as the contract instance; monotonically increasing. |
| `ActiveContractCount` | instance | Currently listed (active) registration count. | Lives as long as the contract instance; adjusted on register, `deactivate`, reactivation and `deregister`. |
| `Stake(contract_id)` | persistent | The staked amount for a registration. | Lives until the entry is deregistered or the stake is fully withdrawn; slash and withdraw mutate it in place. |
| `StakeLock(contract_id)` | persistent | Ledger at which the slash lock expires for a registration. | Lives until the entry is deregistered; refreshed by each slash. |
| `Verified(contract_id)` | persistent | Whether the registration is governance-verified. | Lives until the entry is deregistered; set only through a timelocked proposal. |
| `Slashes(contract_id)` | persistent | Append-only `Vec` of slash records (amount, reason, ledger). | Kept for auditability even after `deregister`; not removed by eager cleanup. |
| `Attestations(contract_id)` | persistent | Bounded `Vec` of `(attester, label, created_at)` records. | Lives until the entry is deregistered; one per attester, revised in place on re-attest. |
| `StakingConfig` | instance | The SEP-41 token and treasury `Address` used for staking. | Lives as long as the contract instance; set by `propose_configure_staking` after the timelock. |
| `AllowlistEnabled` | instance | Whether the allowlist gate is on. | Lives as long as the contract instance; toggled through governance. |
| `Allowlisted(owner)` | persistent | Whether `owner` is on the allowlist. | Lives as long as the registry; toggled through governance. |
| `RateLimit` | instance | Per-owner registration limit and window in ledgers. | Lives as long as the contract instance; set through governance; zero limit disables it. |
| `Proposal(id)` | persistent | A governance proposal (kind, payload, execution ledger, state). | Lives until the proposal is executed or cancelled; read for the timelock check. |
| `ProposalCount` | instance | Monotonic counter used to allocate proposal IDs. | Lives as long as the contract instance; never decremented. |

Instance keys share the contract instance's TTL and are extended whenever the
instance is bumped. Persistent keys have their own TTLs and can be archived if
they are not touched; the index keys (`AllContracts`, `OwnerContracts`,
`CategoryContracts`) are the ones most likely to strand a reference, which is
what `prune_category` / `prune_all_contracts` exist to clean up. Slash records
are deliberately kept past `deregister` for auditability.

### Error codes

`RegistryError` crosses the contract boundary as a bare `u32`, so the
numeric code is the API a caller actually sees. The table below is the
reference for those codes; it is kept next to the enum in
[registry/src/lib.rs](./registry/src/lib.rs) so the two are updated together.

| Code | Name | Meaning | Usual remedy |
| --- | --- | --- | --- |
| 1 | `AlreadyInitialized` | `initialize` was called on a deployment that already has an admin set. | Do not call `initialize` again; read `get_admins` / `get_threshold` to inspect the existing configuration. |
| 2 | `Unauthorized` | The caller is not the registered owner and not permitted to perform this action. | Call from the registered owner's address, or route the action through the governance flow (`propose_*` → `approve_proposal` → `execute_proposal`). |
| 3 | `AlreadyRegistered` | A `Contract` entry already exists for this `contract_id`. | Use `update_metadata` / `set_categories` to change the existing entry, or `deregister` it first if you intend to re-register. |
| 4 | `ContractNotFound` | No `Contract` entry exists for the given `contract_id`. | Check `is_registered` before calling; register the contract first with `register_contract`. |
| 5 | `InvalidMetadata` | The supplied metadata failed validation (e.g. empty batch, batch larger than 100 entries). | Pass a non-empty batch of at most 100 entries and ensure each entry has a name and description. |
| 6 | `NotOwner` | The caller is not the `owner` recorded on the registration. | Call from the recorded owner's address, or have the current owner call `transfer_ownership` first. |
| 7 | `NotInitialized` | The registry has no admin set because `initialize` was never called. | Deploy with the `__constructor` bootstrap admin, or call `initialize` once with a non-empty admin set. |
| 8 | `ProposalNotFound` | No proposal exists for the given `proposal_id`. | Read `get_proposal` for a valid ID; IDs are assigned sequentially starting at 0. |
| 9 | `ThresholdNotMet` | The proposal has not collected enough approvals, or has not yet become ready. | Have additional admins call `approve_proposal` until `approvals.len()` reaches `get_threshold`. |
| 10 | `TimelockNotElapsed` | Fewer than `TIMELOCK_LEDGERS` ledgers have passed since the proposal became ready. | Wait until `ready_at + TIMELOCK_LEDGERS` and retry `execute_proposal`. |
| 11 | `AlreadyApproved` | This admin address has already approved this proposal. | Do not re-approve; have a different admin approve instead. |
| 12 | `NotAdmin` | The caller is not a member of the current admin set. | Call from an address returned by `get_admins`, or propose adding the caller via `propose_add_admin`. |
| 13 | `InvalidThreshold` | The admin set would be empty, or the threshold is zero or exceeds the set size. | Pass a non-empty admin set with `1 <= threshold <= admins.len()`. |
| 14 | `AlreadyExecuted` | The proposal has already been executed. | Do not retry; create a new proposal if further action is needed. |
| 15 | `StakingNotConfigured` | No stake token / treasury has been set, so staking is not open. | Have governance pass `propose_configure_staking` and execute it before staking. |
| 16 | `InvalidAmount` | A stake, slash, or fee amount was zero or negative. | Pass a strictly positive amount for `stake` / `propose_slash`, and a non-negative fee for `propose_set_registration_fee`. |
| 17 | `InsufficientStake` | The registration's staked balance is smaller than the requested amount. | Stake more first with `stake`, or reduce the requested amount to at most `get_stake`. |
| 18 | `StakeLocked` | The stake is still inside the post-slash lock window. | Wait until `get_reputation(...).withdraw_locked_until` and retry `withdraw_stake`. |
| 19 | `RegistrationActive` | The registration is still active, so it cannot be withdrawn or deregistered. | Call `deactivate` first, then retry `withdraw_stake` or `deregister`. |
| 20 | `NoCategories` | A registration or category query declared no categories. | Pass at least one `Category` (use `Category::Other` if none of the vocabulary fits). |
| 21 | `StakeNotEmpty` | The registration still holds stake, so it cannot be deregistered. | Call `withdraw_stake` until `get_stake` returns zero, then retry `deregister`. |
| 22 | `InvalidRateLimit` | The rate limit configuration is invalid (zero window with a non-zero limit, or a window larger than `max_ttl`). | Pass `window_ledgers` in `1..=max_ttl` when `limit > 0`, or set `limit = 0` to disable limiting. |
| 23 | `NotAllowlisted` | The owner is not allowlisted while permissioned registration is enabled. | Have governance execute `propose_set_allowlisted(owner, true)`, or disable the allowlist with `propose_set_allowlist_enabled(false)`. |
| 24 | `RegistrationRateLimited` | The per-owner registration rate limit has been exceeded for the current window. | Wait for the current window to elapse, or have governance raise the limit via `propose_configure_registration_rate_limit`. |
| 25 | `InsufficientFee` | The registration fee was not paid. | Ensure the owner holds at least `get_registration_fee()` of the stake token and approves the transfer before registering. |
| 26 | `InvalidTags` | The tag count exceeds 10, or a tag is longer than 16 characters. | Pass at most 10 tags, each at most 16 characters long. |
| 27 | `InvalidAttestation` | Attestation label is empty, too long, or the registration already has the maximum number of attestations. | Shorten label or prune/revoke prior attestations. |
| 28 | `AttestationNotFound` | The caller has no attestation to revoke on this registration. | Verify the attester address before calling revoke. |
| 29 | `OverlappingAddress` | The proposed treasury or stake-token address is itself a registered contract. | Use a separate, dedicated treasury and token address. |
| 30 | `AdminSetTooSmall` | The admin set would have fewer than `MIN_ADMINS` members. | Maintain at least `MIN_ADMINS` (2) admins in the multi-sig set. |
| 31 | `AlreadyAdmin` | The proposed address is already a member of the admin set. | Propose a new, unadded admin address. |
| 32 | `AdminNotFound` | The proposed address to remove is not in the admin set. | Specify an existing admin address from `get_admins`. |
| 33 | `ThresholdAlreadySet` | The proposed threshold is already the current threshold. | Propose a threshold value different from the current one. |
| 34 | `AlreadyVerified` | The proposed verification status matches the contract's current status. | Check `is_verified` before proposing a verification change. |
| 35 | `StakingAlreadyConfigured` | Staking is already configured with the proposed token and treasury. | Propose a different token or treasury to update configuration. |

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
