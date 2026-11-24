# Governance recovery: when the admin threshold cannot be met

This document states what happens if enough admin keys are lost that no
proposal can reach the approval threshold, what can and cannot be recovered,
how to avoid it, and why the registry has no on-chain recovery path.

It describes the contract in `registry/src/lib.rs`. Where it quotes a rule, the
rule is the code, not this page.

## How governance works, in the parts that matter here

- Every privileged change is a proposal: wasm upgrade, pause and unpause,
  adding or removing admins, changing the threshold, staking and treasury
  configuration, verification, slashing, allowlist, fees and category
  migration. There is no direct admin call that bypasses this.
- Only an address in the current admin set can create or approve a proposal.
  Approvals are unique per admin, so one key cannot approve twice
  (`AlreadyApproved`).
- A proposal needs `get_threshold()` approvals. After that it waits the
  timelock (`TIMELOCK_LEDGERS`, 17,280 ledgers, about 24 hours) and can then be
  executed by anyone.
- The contract guards the admin set against being **configured** into a state
  that cannot reach the threshold: executing `RemoveAdmin` fails with
  `InvalidThreshold` if the remaining admins would be fewer than the threshold,
  and with `AdminSetTooSmall` if fewer than `MIN_ADMINS` (2) would remain.
  `ChangeThreshold` must stay between 1 and the admin count, and `initialize`
  enforces the same bounds.

## The failure mode

Those guards protect against bad proposals. They cannot protect against **lost
or unusable keys**, because the admin set still lists addresses that nobody can
sign for.

With `N` admins and a threshold of `T`, governance is frozen once fewer than `T`
of the admin keys can still sign. That happens after `N - T + 1` keys are lost,
destroyed or permanently unavailable. Examples:

| Admins (N) | Threshold (T) | Keys that can be lost safely | Keys whose loss freezes governance |
|---|---|---|---|
| 2 | 2 | 0 | 1 |
| 3 | 2 | 1 | 2 |
| 5 | 3 | 2 | 3 |
| 5 | 5 | 0 | 1 |
| 1 (constructor default) | 1 | 0 | 1 |

A freshly deployed registry has exactly one admin and a threshold of 1: the
bootstrap admin set by `__constructor`. Until more admins are added and
`initialize` or admin proposals have been used, the loss of that one key freezes
governance on day one.

The registry cannot detect this. Nothing on chain marks governance as frozen;
proposals simply never reach the threshold.

## What is and is not recoverable

### While governance is frozen, these stop working

- Upgrading the contract wasm, including shipping a fix for the freeze.
- Pausing or unpausing. **If the registry was paused when governance froze, it
  stays paused**, and `register_contract` keeps returning `Paused`.
- Adding or removing admins and changing the threshold, which is the only way to
  repair the admin set.
- Configuring staking, the treasury, the registration fee, the minimum stake,
  the allowlist and the registration rate limit.
- Slashing, verification changes, forced deactivation of someone else's
  contract, category migration and treasury withdrawals.

### These keep working

- Everything read-only (`get_contract`, `get_admins`, `get_threshold`,
  `get_proposal` and the rest).
- A proposal that **already reached the threshold** before the freeze can still
  be executed by anyone once its timelock has passed, as long as it has not
  expired. Execution is permissionless on purpose, so approved decisions cannot
  be withheld.
- Owner-initiated actions that do not need governance, such as an owner
  deactivating or transferring their own registration, and registering new
  contracts when the registry is not paused and no allowlist, fee or rate limit
  blocks the owner.

### These cannot be recovered

- The ability to change the contract code or the admin set. There is no
  backdoor, no emergency key and no admin reset.
- Stake or treasury balances that can only be released by a proposal.
- Proposals that were pending when the freeze happened and had not reached the
  threshold. They never will.

The legacy single-admin `Admin` key that `__constructor` also writes is not a
recovery path. It is only an additional authorization accepted by
`transfer_ownership` and is not consulted for proposals.

## Recommended key management

The aim is a threshold that tolerates both losing keys and having a key
stolen: losing `N - T` keys is survivable, and an attacker needs `T` keys.

1. **Do not run production on the constructor default.** Add admins and raise
   the threshold as soon as the registry is deployed, using proposals while the
   bootstrap key is still held. Keep at least `MIN_ADMINS` (2) admins, and
   prefer 5 with a threshold of 3 for anything holding stake.
2. **Never set `T = N`.** It makes every single key a point of failure.
3. **Keep slack of at least two.** `N - T >= 2` survives two simultaneous
   losses, and a rotation can still be approved while one key is missing.
4. **Use separate custody for each key.** Different people, different hardware
   wallets, different locations. No two admin keys should share a device, a
   cloud account or a backup.
5. **Back up each key independently**, offline, with a documented
   restore procedure that the key holder has actually tested.
6. **Rotate while healthy.** Replace a departing or suspect admin by proposing
   `AddAdmin` for the new key first, then `RemoveAdmin` for the old one. Both
   changes wait the timelock, so start well before the key is needed.
7. **Run a periodic liveness drill.** Every quarter, have the admins create,
   approve and execute a harmless proposal, for example `ChangeThreshold` to the
   current value. It proves that `T` keys can still sign before an emergency
   depends on it.
8. **Watch the admin events** (`admin_added`, `admin_removed`,
   `threshold_changed`) and alert on any unexpected change.
9. **Leave the registry unpaused when keys are at risk.** A frozen, paused
   registry is the worst combination.

## Decision: no on-chain recovery

**Status:** accepted.

**Decision:** the registry will not add an on-chain recovery mechanism for a
frozen admin set.

**Options considered:**

- **A recovery key or guardian that can reset the admin set.** This is a single
  address with unilateral control over code and funds, which removes the point
  of a multi-admin threshold. If it is compromised, everything is compromised.
- **A dead-man switch that lets a fallback address take over after a long period
  with no executed proposal.** An attacker, or a quiet period with nothing to
  decide, could trigger it. The registry has no activity guarantee, so "inactive"
  is not evidence that keys are lost.
- **A lower threshold that applies after a delay.** This weakens the guarantee
  at all times in order to help in a rare case, and creates a new proposal path
  that must itself be protected.
- **Social recovery by registered owners.** Owners are an open, permissionless
  set and can be created at will, so their votes are not a trustworthy signal.

**Reasoning:** each option replaces "lost keys freeze governance" with "a
compromised recovery path takes over governance". The first outcome is
permanent but bounded: owners keep their registrations and their own actions,
and only what needs a proposal stays locked, such as the treasury and any
configuration change. The second is unbounded. Key loss is also
preventable with the practices in this document, while a weakened authorization
model cannot be strengthened afterwards without a code change that the frozen
admins cannot approve.

**Operational recovery instead:** if governance freezes, deploy a new registry
with a healthy admin set and have owners re-register there. The old registry
remains readable, and its address should be marked deprecated wherever it is
published. This is disruptive, which is why prevention matters more than cure.

**When to revisit:** if the registry takes custody of material value that cannot
be withdrawn through owner-initiated actions, or if the chosen admin accounts
become contract accounts with their own recovery. Any proposal to add a recovery
path should state who can trigger it, what it can change, how long it waits, and
how an attacker would abuse it.
