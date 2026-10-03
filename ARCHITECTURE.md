# Lumina Registry architecture

This document explains how the registry is assembled and which properties its
tests protect. It is a map for contributors; the exported API and storage
types in [`registry/src/lib.rs`](./registry/src/lib.rs) remain the source of
truth.

## System role

The registry is an on-chain manifest of Soroban contracts that Lumina should
index. A project owner creates a registration, indexers page through active
registrations, and governance adds trust and policy signals around that
permissionless core.

```text
project owner
    |
    | register / update / categorize / stake / deactivate
    v
+------------------------- LuminaRegistry -----------------------------+
| registration core  <---- categories and owner indexes             |
|        |                                                            |
|        +---- reputation: stake + verification + slash history   |
|        |                                                            |
|        +---- governance: propose -> approve -> timelock -> apply |
+------------------------------------------------------------------+
    |                                      |
    | paginated views                      | events
    v                                       v
Lumina indexer and frontend           history consumers
```

The contract deliberately separates four concerns:

- `ContractEntry` stores stable registration metadata and the active flag.
- Secondary indexes make registrations discoverable globally, by owner, and
  by category without embedding those collections in each entry.
- Reputation state lives beside a registration so it can evolve without
  changing the upgrade-sensitive `ContractEntry` encoding.
- Governance controls privileged actions and policy, while ordinary owners
  retain direct control of their own metadata, categories, stake, and exit.

## Storage model

Soroban instance storage holds registry-wide configuration, counters, the
global ordered index, and governance proposals. Persistent storage holds
state keyed by a registration, owner, or category. The contract does not use
temporary storage.

### Instance storage

| Keys | Stored value | Purpose |
| --- | --- | --- |
| `Admins`, `Threshold` | admin addresses and approval threshold | Current multisig configuration. |
| `ProposalCount`, `ProposalData(id)` | next ID and complete `Proposal` records | Governance queue and execution history. |
| `ContractCount`, `TotalRegistered` | live and lifetime registration counts | `ContractCount` decreases on deregistration; `TotalRegistered` does not. |
| `AllContracts` | insertion-ordered contract addresses | Source index for global pagination and active-profile views. |
| `StakeToken`, `Treasury`, `MinimumStake` | staking configuration | Set only through governance. |
| `AllowlistEnabled`, `RegistrationRateLimit`, `RegistrationRateWindow` | registration policy | Optional admission and fixed-window controls. |
| `RegistrationFee` | fee denominated in the stake token | Zero keeps registration free. |
| `TotalStaked`, `VerifiedCount` | maintained aggregate counters | Support constant-cost statistics. |
| `Admin` | original single-admin address | Deprecated compatibility slot. No longer written by `initialize` / `__constructor` and **not consulted for authorization**; `get_admin` reads it only as a fallback for pre-multisig deployments. |

### Persistent storage

| Key | Stored value | Lifecycle |
| --- | --- | --- |
| `Contract(contract_id)` | `ContractEntry` | Created on registration; active may be cleared; removed on deregistration. |
| `OwnerContracts(owner)` | ordered contract addresses | Updated on registration, ownership transfer, and deregistration. |
| `Categories(contract_id)` | deduplicated `Category` values | Updated by the owner; removed on deregistration. |
| `ByCategory(category)` | ordered contract addresses | Maintained as a secondary index; stale archived references can be pruned permissionlessly. |
| `Stake(contract_id)` | current collateral | Increased by deposits; decreased by slashes or withdrawal. |
| `Verified(contract_id)` | governance trust signal | Changed only by an executed proposal. |
| `Slashes(contract_id)` | ordered `SlashRecord` history | Appended on slash and deliberately retained after deregistration. |
| `WithdrawLockedUntil(contract_id)` | ledger sequence | Prevents immediate withdrawal of remaining collateral after a slash. |
| `UnbondingUntil(contract_id)` | ledger sequence | Set by `request_unbond`; `withdraw_stake` refuses until it elapses. |
| `Allowlisted(owner)` | admission flag | Consulted only when allowlist mode is enabled. |
| `RegistrationWindow(owner)` | window start and count | Fixed-window registration rate accounting; its TTL is extended to the configured window. |
| `Tags(contract_id)` | bounded normalized tags | Owner-managed discovery metadata. |
| `Attestations(contract_id)` | bounded third-party claims | Separate from governance verification; removed on deregistration. |
| `NameIndex(prefix)` | ordered contract addresses | Name-prefix discovery index keyed on the normalised name prefix; maintained on registration, metadata update, and deregistration. |

`ContractEntry` is intentionally small and stable: contract address, owner,
name, description, registration ledger, and active flag. Reputation is joined
at read time by `get_contract_profile` and `get_active_profiles`. This avoids a
storage migration whenever reputation gains a new field.

Reputation is decayed at read time rather than by writing to every entry. The
decay factor is a function of the ledgers elapsed since a registration's last
activity, so a long-inactive registration reports a lower score than an active
one with an identical history. The curve and its parameters are documented in
the reputation section below. Because decay is applied on read, it introduces
no write amplification and no per-entry storage migration.

The indexes contain addresses rather than copies of `ContractEntry`.
`get_active_contracts`, `get_active_contract_ids`, the active contract/profile
page methods, `get_active_profiles`, and `get_active_contracts_by_category`
apply `offset` to their underlying global or category index. They skip inactive
or missing entries without consuming the result `limit`, continuing until the
result is full or the raw index ends. `get_contracts_by_categories` differs: it
first builds a deduplicated union of active entries and then applies `offset`
and `limit` to that filtered union. `get_contracts_by_owner` also differs: it
resolves the owner's ordered index without active filtering and therefore
includes inactive registrations.

`find_by_name_prefix(prefix, limit)` resolves the normalised prefix against
`NameIndex` and returns matching entries. Matching is case-insensitive because
both the stored key and the query are normalised (trimmed and lowercased)
before lookup. An unmatched prefix yields an empty list rather than an error.
On-chain prefix matching is deliberately limited to a single normalised
prefix: richer name search, ranking, and fuzzy matching belong in the indexer,
which can build a full-text index off the registration events.

The cursor variants — `get_active_contracts_after`,
`get_contracts_by_category_after` and `get_contracts_by_owner_after` — walk the
same indexes but resume from the id of the last entry returned instead of a
numeric offset. They are the recommended way to page a whole list: an offset
walk re-reads everything before its position on every page, and an insertion
mid-walk shifts every later page, whereas a cursor is anchored to a
registration, so entries added while walking are appended and never duplicate
or skip one already returned. The offset entrypoints are retained for one
release and documented as deprecated.

## Registration lifecycle

1. `register_contract` authenticates the owner, applies the optional
   allowlist, rate-limit, and fee policies, rejects duplicate addresses, and
   deduplicates the non-empty category list.
2. It writes the `ContractEntry`, appends the address to `AllContracts`, the
   owner's index, and each category index, then advances the live and lifetime
   counters.
3. The owner may update metadata, tags, and categories. Ownership transfer can
   be authorized by the current owner or a current multisig admin; it moves the
   address from the previous owner's index to the new owner's index. The legacy
   single-admin slot carries no authority and is not accepted here.
4. `deactivate` is an immediate owner action. It clears only `active`; listing
   views filter the entry out while its metadata, reputation, and history
   remain available. Governance may deactivate somebody else's registration
   only through a proposal.
5. `deregister` is the destructive exit. It requires the owner, an inactive
   entry, and zero remaining stake. It removes live metadata and index
   references but preserves slash history for auditability. The address may
   then be registered again as a fresh entry.

Metadata updates that change the name move the address between `NameIndex`
buckets so prefix lookups stay consistent with the current `ContractEntry`.

`register_contracts` performs a bounded batch in one atomic Soroban invocation,
so a validation or token-transfer failure leaves no partial batch behind. Its
preflight rejects addresses that are already stored and validates every
category list, but it does not deduplicate contract IDs repeated within the
same input batch. Callers must therefore supply unique contract IDs.

`prune_all_contracts` and `prune_category` cover a different failure mode:
storage archival may make a persistent `Contract` entry unavailable without
running deregistration. Both functions are permissionless and idempotently
remove those dead index references.

## Governance flow

Privileged changes share one state machine:

```text
admin proposes
      |
      v
Pending (ready_at = u32::MAX)
      |
      | unique current-admin approvals reach Threshold
      v
Ready (ready_at = current ledger)
      |
      | TIMELOCK_LEDGERS elapse
      v
Anyone executes ----> Executed (cannot execute twice)
      |
      | PROPOSAL_EXPIRY_LEDGERS elapse without execution
      v
Expired (must be re-proposed)
```

Only an address in `Admins` can create or approve a proposal. Approvals are
stored as addresses and duplicate approval is rejected. Reaching the threshold
sets `ready_at` once; it does not execute the action. After the timelock,
`execute_proposal` is permissionless so execution cannot be witheld by the
admin set after it has approved the action.

The timelock is per-action rather than a single constant. Each `ProposalAction`
maps to a duration in `TIMELOCK_LEDGERS`, so a proposal's `ready_at` is set to
`current ledger + TIMELOCK_LEDGERS(action)` when the threshold is reached.
`get_proposal` exposes the action's timelock so a UI can show the wait.

Proposal actions cover:

- deactivation and wasm upgrade;
- adding or removing admins and changing the threshold;
- staking token and treasury configuration;
- verification and slashing;
- allowlist, registration rate limit, registration fee, and minimum stake;
- treasury withdrawal.

The chosen durations are constants, not magic numbers inline, and are grouped
by risk:

- `Upgrade` and admin-set changes (`AddAdmin`, `RemoveAdmin`,
  `ChangeThreshold`) keep the long window (`LONG_TIMELOCK_LEDGERS`, 17,280
  ledgers, approximately 28.8 hours at six seconds per ledger). These change
  the contract's code or who controls it, so they are the most dangerous
  actions and must wait the longest.
- `SetStakeToken`, `SetTreasury`, `SetMinimumStake`, `SetAllowlist`,
  `SetRegistrationRateLimit`, and `SetRegistrationFee` use a medium window
  (`MEDIUM_TIMECLOCK_LEDGERS`, 5,760 ledgers, approximately 9.6 hours). They
  change policy or configuration but not code or control, so a shorter wait is
  safe.
- `Deactivate`, `SetVerified`, `Slash`, and `WithdrawTreasury` use a short
  window (`SHORT_TIMELOCK_LEDGERS`, 1,440 ledgers, approximately 2.4 hours).
  They are routine governance operations with bounded, reversible, or
  already-constrained effects.

Two proposals of different kinds therefore become executable at different
times.

Execution checks the threshold and timelock again, marks the proposal executed
before applying external effects, and relies on Soroban transaction atomicity:
if an action or token transfer fails, the executed flag and every other write
from that invocation roll back. Admin-removal and threshold actions also
validate that the resulting threshold remains satisfiable.

The production timelocks are the per-action constants above. Tests use 10
ledgers so they can exercise boundaries without archiving fixture storage.

The production expiry window is 518,400 ledgers (approximately 36 days at six
seconds per ledger), measured from `ready_at`. It is deliberately far longer
than the timelock: the timelock protects against haste, while the expiry
protects against staleness. A proposal that reaches threshold but is never
executed within this window becomes invalid and must be re-proposed, so a
decision cannot be executed against an admin set or policy context that has
since changed. `execute_proposal` refuses an expired proposal with a named
error, and `get_proposal` exposes the expiry so a UI can surface it. Tests use
a short window so they can exercise the boundary without archiving fixture
storage.

## Staking, verification, and slashing

Staking is unavailable until governance configures a SEP-41 token and a
treasury. It is an optional reputation layer: registration never requires
staked collateral. Governance can separately gate registration with the
allowlist or rate limit, and can charge a registration fee in the configured
token; that fee goes to the treasury and is not credited as stake. The governed
`MinimumStake` value likewise does not gate registration: it only causes
threshold-crossing events when stake, slash, or withdrawal changes a recorded
stake balance.

```text
owner -- stake --> registry token balance
                    |          |
                    |          +-- governance SetVerified --> trust flag
                    |
                    +-- governanc
---

Reputation is a read-time join of stake, verification, slash history, and a
decay factor derived from the ledgers elapsed since the registration's last
activity. The decay curve is exponential with a half-life expressed in ledgers
(`REPUTATION_HALF_LIFE_LEDGERS`), so the effective signal for an inactive
registration falls by half every half-life of inactivity and approaches but never
reaches zero. Activity (staking, updates, attestations, and governance actions)
refreshes the reference ledger, so a registration that is used regularly reports a
score close to its undecayed value. The decay factor is applied in
`get_contract_profile` and `get_active_profiles` without writing to storage, so
there is no write amplification and no migration when the curve or its
parameters change.
