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
+------------------------- LuminaRegistry --------------------------+
| registration core  <---- categories and owner indexes             |
|        |                                                        |
|        +---- reputation: stake + verification + slash history   |
|        |                                                        |
|        +---- governance: propose -> approve -> timelock -> apply |
+------------------------------------------------------------------+
    |                                      |
    | paginated views                      | events
    v                                      v
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
| `Admin` | original single-admin address | Retained solely for storage and upgrade compatibility. |

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
| `Allowlisted(owner)` | admission flag | Consulted only when allowlist mode is enabled. |
| `RegistrationWindow(owner)` | window start and count | Fixed-window registration rate accounting; its TTL is extended to the configured window. |
| `Tags(contract_id)` | bounded normalized tags | Owner-managed discovery metadata. |
| `Attestations(contract_id)` | bounded third-party claims | Separate from governance verification; removed on deregistration. |
| `NameIndex(prefix)` | ordered contract addresses | Name-prefix discovery index keyed on the normalised name prefix; maintained on registration, metadata update, and deregistration. |

`ContractEntry` is intentionally small and stable: contract address, owner,
name, description, registration ledger, and active flag. Reputation is joined
at read time by `get_contract_profile` and `get_active_profiles`. This avoids a
storage migration whenever reputation gains a new field.

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

## Registration lifecycle

1. `register_contract` authenticates the owner, applies the optional
   allowlist, rate-limit, and fee policies, rejects duplicate addresses, and
   deduplicates the non-empty category list.
2. It writes the `ContractEntry`, appends the address to `AllContracts`, the
   owner's index, and each category index, then advances the live and lifetime
   counters.
3. The owner may update metadata, tags, and categories. Ownership transfer can
   be authorized by the current owner, a current multisig admin, or the legacy
   single admin retained for upgrade compatibility; it moves the address from
   the previous owner's index to the new owner's index.
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
```

Only an address in `Admins` can create or approve a proposal. Approvals are
stored as addresses and duplicate approval is rejected. Reaching the threshold
sets `ready_at` once; it does not execute the action. After the timelock,
`execute_proposal` is permissionless so execution cannot be withheld by the
admin set after it has approved the action.

Proposal actions cover:

- deactivation and wasm upgrade;
- adding or removing admins and changing the threshold;
- staking token and treasury configuration;
- verification and slashing;
- allowlist, registration rate limit, registration fee, and minimum stake;
- treasury withdrawal.

Execution checks the threshold and timelock again, marks the proposal executed
before applying external effects, and relies on Soroban transaction atomicity:
if an action or token transfer fails, the executed flag and every other write
from that invocation roll back. Admin-removal and threshold actions also
validate that the resulting threshold remains satisfiable.

The production timelock is 17,280 ledgers (approximately 28.8 hours at six
seconds per ledger). Tests use 10 ledgers so they can exercise boundaries
without archiving fixture storage.

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
                    +-- governance Slash --> treasury
                    |                         + slash history
                    |                         + withdrawal lock
                    |
inactive + unlocked + owner -- withdraw --> owner
```

- `stake` authenticates the registered owner, transfers tokens into the
  registry, and only then increases the per-registration and aggregate stored
  balances. Repeated calls top up the position.
- `Verified` is independent of stake and third-party attestations. Only an
  executed `SetVerified` proposal can change it.
- `Slash` is proposed by governance. Execution transfers the requested amount
  from the registry to the treasury, decreases tracked stake, appends a reason
  and ledger to slash history, and locks the remainder for
  `SLASH_LOCK_LEDGERS`.
- `withdraw_stake` returns the entire remainder only to the owner, only after
  deactivation, and only after the post-slash lock expires.

The internal bookkeeping invariant across staking transitions is:

```text
TotalStaked == sum(Stake(contract_id)) across tracked registrations
```

In the isolated staking flows exercised by the tests, the registry's token
balance also equals that tracked stake. That balance equality is conditional,
not a general ledger invariant: unsolicited token transfers and the governed
treasury-withdrawal action can place tokens in the registry without crediting
any registration or `TotalStaked`.

Token transfers occur before the corresponding bookkeeping changes. If a
token refuses a deposit, withdrawal, or slash transfer, Soroban rolls back the
whole invocation. This prevents recorded collateral from diverging from the
tokens the registry actually controls.

## Invariants pinned by tests

The main suite is colocated with the implementation in
[`registry/src/lib.rs`](./registry/src/lib.rs). These are the architectural
properties it checks, with representative test names for quick navigation:

| Invariant | Representative tests |
| --- | --- |
| A proposal needs enough unique admin approvals, the full timelock, and at most one successful execution. | `proposal_cannot_execute_below_threshold`, `double_approval_does_not_count_toward_threshold`, `proposal_executes_exactly_at_timelock_boundary`, `executed_proposal_cannot_execute_again` |
| Governance cannot create an impossible admin threshold. | `remove_admin_that_would_violate_threshold_fails`, `change_threshold_via_governance` |
| Metadata and immediate deactivation require the owner; ownership transfer accepts the owner or an admin override and moves the owner index. | `deactivate_by_non_owner_is_rejected`, `update_metadata_rejects_non_owner`, `transfer_ownership_moves_entry_between_owner_indices`, `transfer_ownership_by_admin_succeeds` |
| Active listings and category listings agree on filtering, order, and pagination semantics. | `get_active_contracts_excludes_deactivated`, `category_pagination_matches_the_global_listing`, `category_pages_are_in_registration_order` |
| Category membership is non-empty and deduplicated, and category changes do not affect reputation. | `registration_requires_at_least_one_category`, `duplicate_categories_are_collapsed`, `categories_and_reputation_are_independent` |
| In isolated staking flows, tracked stake equals the registry token balance through deposits, slashes, withdrawals, and transfer failures. | `stake_moves_real_tokens_into_the_registry`, `full_stake_verify_slash_withdraw_lifecycle`, `failed_stake_transfer_records_no_stake`, `failed_slash_transfer_leaves_stake_history_and_proposal_untouched` |
| Verification is governance-only and independent from self-service attestations. | `a_registrant_cannot_verify_their_own_contract`, `attesting_does_not_affect_governance_only_verification`, `attesting_does_not_grant_verification_or_privilege_to_the_attester` |
| Name-prefix search is case-insensitive, returns every matching registration, and returns an empty list for an unmatched prefix. | `find_by_name_prefix_is_case_insensitive`, `find_by_name_prefix_returns_all_matches`, `find_by_name_prefix_unmatched_returns_empty` |
| Deregistration removes live indexes and state only after safe exit, while retaining slash history and lifetime totals. | `deregister_requires_deactivated_and_unstaked`, `deregister_removes_every_index_reference_and_decrements_the_live_count`, `deregister_keeps_slash_history_for_audit`, `contract_count_is_live_and_total_registered_is_lifetime` |
| Code upgrades preserve compatible storage and authentication. | `upgrade_swaps_code_and_preserves_registrations`, `upgrade_carries_admin_across_swap`, `upgraded_registry_can_be_rolled_back` |

Additional interface tests in
[`registry-interface/tests/interface_matches_registry.rs`](./registry-interface/tests/interface_matches_registry.rs)
compare the compiled contract specification with the published Rust interface.
The independent implementation in [`registry-v2`](./registry-v2) exercises
storage compatibility across a wasm upgrade.

## Upgrade boundaries

`upgrade` swaps the wasm while preserving the contract address and storage.
That makes storage encoding part of the long-lived protocol:

- Adding a new `DataKey` variant is compatible; renaming or repurposing an
  existing variant is not.
- Adding, removing, renaming, or changing the type of a field in a stored
  contract type requires an explicit migration.
- Reputation and other extensions should prefer new adjacent keys and
  read-time composition over modifying `ContractEntry`.
- `CONTRACT_VERSION` must change with exported-interface or storage-shape
  changes.

Before changing the architecture, run `make check`. For changes to exported
functions or contract types, also update the interface snapshot and verify the
consumer-facing assumptions documented in [`README.md`](./README.md) and
[`EVENTS.md`](./EVENTS.md).
