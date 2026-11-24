# Governance Playbook for Admins

This playbook is the operational companion to `DEPLOY.md`. `DEPLOY.md`
documents the *commands*; this document documents the *practice* — the
diligence an admin is expected to exercise, the clocks they are on, and the
consequences of inaction.

Resolves #240.

---

## 1. The admin role in one page

A Lumina admin is a signer on a multi-sig governance set that controls
every change to the registry except owner-initiated instant actions
(`deactivate`, `deregister`, metadata updates for one's own registration).
Every state-changing admin action moves through the same pipeline:

```
propose_* → approve_proposal (× threshold) → ready → wait TIMELOCK_LEDGERS → execute_proposal
```

The admin set, threshold and per-admin weights are themselves governed by
this same pipeline: nothing an admin can do skips it.

### Responsibilities

- **Hold your signing key safely.** See §5 — admins who lose key custody
  put the registry at risk until their address is removed.
- **Read every open proposal before the ready transition.** Once the
  threshold clears you lose the ability to withhold approval, and the
  only remaining stop is a counter-proposal (see §3.4).
- **Approve or publicly object within the diligence window** (§3.3). An
  admin who goes silent during a live proposal is, by default, counted as
  tacit non-objection — the threshold is reached without them. If you
  need more time, say so on the proposal thread so others can wait.
- **Rotate your key on the schedule in §5.3.** Even without a
  suspected compromise, admin keys with no end-of-life plan are a slow
  leak of governance security.

### What inaction means

- You are **not** required to approve every proposal. Threshold is the
  *minimum* summed weight; many proposals pass without unanimous approval.
- Inaction on a proposal you *disagree* with still allows it to pass once
  the threshold clears. To stop a proposal you disagree with, either (a)
  persuade enough admins to not approve, or (b) submit a counter-proposal
  (§3.4) **before** the current one executes.
- Inaction across many proposals in a row is grounds for removal via
  `propose_remove_admin`. Weighted voting (#247) makes the cost of a
  silent admin explicit — their weight is committed capital that nobody
  is spending.

---

## 2. Proposal types and how to evaluate them

Every proposal type is defined by a `ProposalAction` variant. This
section groups them by diligence profile.

### 2.1 Low-risk, mechanical (minutes to hours of diligence)

| Variant | What to check |
|---|---|
| `SetAllowlistEnabled(bool)` | Does the operator need the allowlist on/off, and is a window stated for the change? |
| `SetAllowlisted(addr, bool)` | Is the owner's identity known on an off-chain channel, and does the ticket state the reason for the entry? |
| `SetPaused(bool)` | On-chain pause is a blunt tool; confirm there is a public comms plan before approving a `true` pause. |
| `ConfigureRegistrationRateLimit(limit, window)` | The limits must not be smaller than the operator's own traffic baseline — grep the status dashboard's `registrations_per_window` panel first. |
| `SetRegistrationFee(i128)` | Fees are quoted in the stake token; confirm the token decimals before approving an absolute number. |

Timeline expectation: the diligence fits inside one business day. Delaying
past three business days is itself a signal — raise an objection or a
counter-proposal.

### 2.2 Medium-risk, policy (hours of diligence)

| Variant | What to check |
|---|---|
| `AddAdmin(addr)` | Is the new admin KYC'd to the operator's bar, do you have an out-of-band contact, and is the initial weight appropriate (default 1 after this proposal; a weight bump is a separate proposal)? |
| `RemoveAdmin(addr)` | Is the removal being proposed because the admin is compromised, inactive, or departed — and is the remaining summed weight still comfortably above the current threshold? |
| `ChangeThreshold(u32)` | After this change, is any single admin's **weight** less than `total_weight - new_threshold`? If yes, that admin is now a single-handed blocker. See §4. |
| `SetAdminWeight(addr, u32)` (#247) | A bumped weight shifts the balance in every future vote; the governance thread should name the counter-balancing weight change in the same batch, or justify the asymmetry. |
| `MigrateCategory(from, to)` | Dry-run `migration_cursor` on a scratch deployment; confirm no active entry uses `from` in a way the new `to` cannot represent. |

Timeline expectation: three business days. Longer than that and you are
fighting the proposer's calendar; ask for a counter-proposal rather than
sitting on the approval button.

### 2.3 High-risk, consequential (days of diligence)

| Variant | What to check |
|---|---|
| `Upgrade(hash)` | Is the hash produced by a reproducible build from a signed tag? Did the **testnet upgrade simulation** (#243, `scripts/testnet_upgrade_sim.sh`) pass against *this exact hash*, with the state snapshot your operator keeps as the "representative state" fixture? |
| `ConfigureStaking(token, treasury)` | Is the staking token's `decimals` call non-reverting, is `treasury` **not** the contract's own address, and is the proposed treasury multi-sig itself as well-governed as this registry? |
| `ConfigureMinimumStake(i128)` | Confirm the figure is denominated in the stake token's smallest unit; a wrong-decimal minimum stake has excluded registrations in every other deployment that has made that mistake. |
| `WithdrawFromTreasury(i128)` | Is the withdrawal destination on the public treasury-recipient allowlist? If no public allowlist exists yet, insist on one first. |
| `SetVerified(addr, bool)` | Verified status is a public claim by the admin set about an off-chain party. Confirm the attestation document is in the ticket. |
| `Slash(addr, amount, reason)` | Read the `SLASH_RESPONSE_EXAMPLES.md` playbook; confirm the evidence is attached and the amount matches the public slash-schedule. |

Timeline expectation: five business days minimum, except for a `SetPaused(true)`
emergency which is designed to be fast. Any high-risk proposal moving
faster than this should be explicitly justified on the proposal thread.

### 2.4 Batches

`Batch(actions)` executes `1..=MAX_BATCH_ACTIONS` actions atomically.
Diligence equals the maximum diligence across the included actions, *not*
the average. A single `Upgrade` inside a batch of low-risk actions
inherits the five-business-day clock.

---

## 3. Timelines, clocks and what the numbers mean

### 3.1 The two clocks

- **Approval clock** — from `propose_*` to the moment the summed
  approval weight reaches the threshold. No on-chain deadline; it ends
  only when enough admins approve.
- **Timelock clock** — from the ready ledger to `ready_at +
  TIMELOCK_LEDGERS`. At ~6 s per ledger, this is roughly the horizon
  documented alongside the constant in `registry/src/lib.rs`. An admin's
  withdrawal of objection must land before this clock expires.

### 3.2 Reading a proposal's state

`get_proposal(proposal_id)` returns a `Proposal` with:

- `approvals: Vec<Address>` — unique approving addresses.
- `ready_at: u32` — ledger at which threshold was first reached, or
  `u32::MAX` if not yet reached. `TIMELOCK_LEDGERS` is added to this.
- `executed: bool`, `executor: Option<Address>` — final state.

The approval count on the `proposal_approved` event is the **summed
weight** under weighted voting (#247), not the number of approving
admins. Dashboards that still show a count should be updated to display
weight.

### 3.3 Diligence windows (operator convention)

| Risk tier | Expected diligence window |
|---|---|
| Low (§2.1) | 1 business day |
| Medium (§2.2) | 3 business days |
| High (§2.3) | 5 business days |
| Emergency `SetPaused(true)` | Hours |

These are operator conventions, not on-chain rules. The chain accepts an
approval at any time.

### 3.4 Counter-proposals

If a proposal is dangerous but you cannot stop its approvals, the
remaining tool is a counter-proposal: a new proposal (often `SetPaused`
or `RemoveAdmin`) that executes faster, can clear its own threshold,
and renders the original proposal's action moot. A counter-proposal must
land and complete execution **before** the original's
`ready_at + TIMELOCK_LEDGERS` — plan your clocks accordingly.

---

## 4. Weighted voting (#247)

See `GOVERNANCE_PLAYBOOK.md` §2.2 for the proposal-level check. The two
operator-level invariants are:

1. **No single admin should be able to block a proposal alone unless
   you intend them to be a veto admin.** An admin's weight is a blocking
   weight when `weight > total_weight - threshold`; audit after every
   `SetAdminWeight` or `ChangeThreshold` execution.
2. **Preserve a functioning quorum if the heaviest admin is offline.**
   Run the hypothetical: with the top-weighted admin absent, is the
   remaining summed weight still above the threshold? If not, you are
   one key loss from a frozen registry.

Default weight for every admin is `1`. A deployment that has never
executed `SetAdminWeight` is semantically identical to the pre-weighting
model; migration is a no-op.

---

## 5. Key handling and rotation

### 5.1 Hardware isolation

Admin keys SHOULD live in hardware wallets or an HSM. A plain-text or
disk-backed key for a Lumina admin is the single biggest governance
risk the operator can introduce.

### 5.2 Compromise response (triage)

If you believe your key is compromised:

1. **Immediately** call `propose_remove_admin(your_admin, your_admin)`
   from a different signer in the admin set. If you still have signing
   capacity with the compromised key, approve it yourself to accelerate
   the timelock clock.
2. In parallel, propose a `SetPaused(true)` if you believe the
   compromise is being actively exploited.
3. Post an advisory on the operator's public channel naming the
   proposal ID. Admins reading this playbook should treat
   compromise-triggered proposals as §2.1 low-friction approvals.

### 5.3 Rotation schedule

Even without suspected compromise:

- Admin keys SHOULD rotate every 12 months.
- Rotation is a `RemoveAdmin(old) → AddAdmin(new)` pair, ideally batched
  so there is no window with `total_weight < threshold`.
- A `SetAdminWeight(new, old_weight)` follow-up restores the previous
  balance; `AddAdmin` always places the new admin at the default weight
  of `1`.

### 5.4 Multi-admin loss

If `total_weight < threshold` at any point, the registry is
governance-locked: no proposal can execute. The only recovery is a
`GOVERNANCE_RECOVERY.md` procedure, which requires off-chain action.
Avoid this state at all costs by keeping the rotation batches atomic.

---

## 6. Where to look when things go wrong

| Symptom | First-line reference |
|---|---|
| A proposal won't execute | `registry/src/lib.rs` → `execute_proposal` error codes; `ERROR_PATH_TESTS.md`. |
| Monitoring is quiet / loud | `EVENTS.md` → the event name you expect; cross-check against the status dashboard. |
| An upgrade is about to ship | #243 testnet upgrade simulation; `scripts/testnet_upgrade_sim.sh`; `DEPLOY.md` release-gate section. |
| A slash is in progress | `SLASH_RESPONSE_EXAMPLES.md` + `SLASH_RESPONSE_IMPLEMENTATION.md`. |
| Governance is frozen | `GOVERNANCE_RECOVERY.md`. |
| The registry has multiple pending proposals that conflict | §3.4 counter-proposals. |

---

*This playbook is binding on the admin set alongside `DEPLOY.md` and
`GOVERNANCE_RECOVERY.md`. Material changes require a governance
proposal. A new admin should be able to participate correctly from this
page alone — if you find a gap during an incident, add it here before
the ticket closes.*
