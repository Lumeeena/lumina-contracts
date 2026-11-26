# Entrypoint authorization matrix

Every public method on `LuminaRegistry` is callable on-chain. This file lists
each one with its authorization requirement, so the model as a whole is
reviewable in one place rather than reconstructed from 70+ doc comments.

Lives beside `registry/src/lib.rs`. Update it whenever an entrypoint is
added, removed, or has its caller requirement changed. Issue #255 pins the
exported function list; this file pins what each of those functions requires.

## Classes

- **Governance proposal.** Caller must be a current admin and `require_auth`
  as one. The change does not land until the proposal is approved to
  threshold, waits out the timelock, and is executed. Tested against
  `assert_is_admin` and guarded by the `Paused` flag where relevant.
- **Admin direct.** Caller must be an admin and `require_auth`. Takes effect
  immediately; no proposal needed. There are only two: `approve_proposal`
  (recording an approval) and nothing else — every other admin power goes
  through a proposal.
- **Owner.** Caller must be the registered owner of the contract that is
  being changed, and `require_auth` as that address. "Owner" means the
  address recorded as `owner` on the `ContractEntry`.
- **Staker.** Caller must `require_auth` as the address they are staking or
  withdrawing from. A staker does not have to own the registration they
  stake against.
- **Attester.** Caller must `require_auth` as the address recording or
  revoking an attestation. An attester does not have to own the
  registration they attest to.
- **Permissionless.** No authorization; anyone can call. Reads are all in
  this class, plus a handful of writes whose only effect is advancing
  something that any party is free to advance (ledger extensions, pruning
  expired entries, executing a proposal that already passed).

The pause flag (`Paused`) blocks every write entrypoint when set. It does
**not** block reads or `execute_proposal`.

## Governance proposals

Every one of these requires the proposer to be an admin. Each creates a
proposal that must be approved to threshold and timelocked before it
executes. Exceptions are called out in the "Notes" column.

| Entrypoint | Caller | Notes |
| --- | --- | --- |
| `propose_batch` | admin | Up to `MAX_BATCH_ACTIONS` actions in one proposal; nested batches rejected. |
| `propose_deactivate` | admin | Admins deactivating someone else's registration. An owner deactivating *their own* uses `deactivate` directly. |
| `propose_add_admin` | admin | |
| `propose_remove_admin` | admin | Rejected if it would make the threshold unsatisfiable or drop below `MIN_ADMINS`. |
| `propose_change_threshold` | admin | |
| `propose_upgrade` | admin | Longer timelock on upgrade actions; see `UPGRADE_CANARY.md`. |
| `propose_configure_staking` | admin | Rejected if proposed treasury or token overlaps an existing registration. |
| `propose_set_verified` | admin | |
| `propose_slash` | admin | Rejected if amount is zero or negative. Slash itself freezes withdraw for `SLASH_LOCK_LEDGERS`. |
| `propose_set_allowlist_enabled` | admin | |
| `propose_set_allowlisted` | admin | |
| `propose_set_rate_limit` | admin | |
| `propose_set_registration_fee` | admin | Negative fee rejected; zero disables. |
| `propose_configure_minimum_stake` | admin | Negative minimum rejected; zero disables. |
| `propose_withdraw_from_treasury` | admin | |
| `propose_set_paused` | admin | Pause blocks every writer; `execute_proposal` and reads keep working. |
| `propose_set_admin_weight` | admin | Weight of 0 rejected — remove the admin instead. |

## Admin direct

| Entrypoint | Caller | Notes |
| --- | --- | --- |
| `approve_proposal` | admin | Each admin may approve a given proposal at most once. |

## Permissionless writes

| Entrypoint | Caller | Notes |
| --- | --- | --- |
| `execute_proposal` | anyone | Caller only has to `require_auth` so their fees are charged. The proposal must already have reached threshold and cleared timelock. |
| `renew` | anyone | Pays rent to extend a registration's TTL. |
| `prune_category` | anyone | Compacts an expired slot out of a category index. |
| `prune_all_contracts` | anyone | Same, for the global list. |

## Owner

Each of these is signed by the registration's current owner (the address on
`ContractEntry.owner`). The only exception is `transfer_ownership`, which
also accepts the admin — called out below.

| Entrypoint | Caller | Notes |
| --- | --- | --- |
| `register_contract` | owner | The caller becomes the owner of the new entry. |
| `register_contracts` | owner | Batched form of `register_contract`. The owner owns every entry in the batch. |
| `deactivate` | owner | Instant for the owner; admins go through `propose_deactivate`. |
| `deregister` | owner | The registration must be deactivated and hold no stake first. |
| `set_categories` | owner | |
| `set_tags` | owner | |
| `set_superseded_by` | owner | Flags an entry as superseded by another; the referenced entry must exist. |
| `update_metadata` | owner | |
| `update_metadata_uri` | owner | |
| `respond_to_slash` | owner | One response per slash record. See `SLASH_PROJECT_RESPONSE.md`. |
| `transfer_ownership` | current owner **or** admin | The admin branch is here on purpose: it is auditable and governance can argue over its use, but it exists so a lost owner key does not permanently freeze a registration. |

## Staker

| Entrypoint | Caller | Notes |
| --- | --- | --- |
| `stake` | the staker | The staker does not have to be the registration's owner. Rejected if amount ≤ 0. |
| `withdraw_stake` | the staker | Registration must be deactivated and the post-slash lock must have elapsed. Partial withdrawals are not supported. |

## Attester

| Entrypoint | Caller | Notes |
| --- | --- | --- |
| `attest` | the attester | The attester does not have to own the registration being attested to. |
| `revoke_attestation` | the attester | An attester can only revoke their own attestations. |

## Setup

| Entrypoint | Caller | Notes |
| --- | --- | --- |
| `__constructor` | anyone (once) | Called exactly once at deploy time; `require_auth` on the bootstrap admin. |
| `initialize` | anyone (once) | Only reachable when the contract was deployed from a wasm that did not run `__constructor`. `require_auth` on every passed admin, so a migration must be countersigned by all of them. |

## Reads

Every function whose name starts with `get_`, `is_`, `can_` (and
`get_version`) is a permissionless read. None takes a caller argument and
none calls `require_auth`. They are not listed individually here —
`registry/interface.snap` has the full list and the interface test in
`registry/tests/interface.rs` keeps it honest.

## Exceptions to the general pattern

- `execute_proposal` is permissionless, not admin-only. It has no
  decision power — it just moves a proposal that has already passed
  through to its side effects.
- `transfer_ownership` accepts the admin as caller, not just the current
  owner. This is intentional: without it a lost owner key is a permanent
  freeze. The alternative (an admin recovery proposal) exists downstream
  via `propose_deactivate` and `register_contract`, but it costs the
  registration's history.
- `deactivate` is **owner only**, not admin. Admins deactivating someone
  else's registration go through `propose_deactivate`, which is why the
  two live side by side.
- `renew` is permissionless. Anyone paying to extend a registration's
  TTL is a help, not a privilege.
- `__constructor` and `initialize` have no caller check beyond their
  signatures. They become unreachable after the first successful run
  (`AlreadyInitialized` thereafter), so their window is one call.

## Keeping this file honest

If this file drifts from the code, the first place it will be noticed is
during a security review — exactly the review where the matrix most needs
to be trustworthy. Two guards keep it in sync:

1. `registry/interface.snap` pins the full exported-function list; a new
   entrypoint added without updating that file fails the interface test.
2. Issue #254 is closed by this document. A later change to any
   entrypoint's caller requirement should update the matching row here
   in the same PR.

If you find a row you cannot reconcile with the code, open an issue —
one of them is wrong.
