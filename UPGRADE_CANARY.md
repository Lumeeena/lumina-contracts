# Canary deployment for registry upgrades

A canary is a parallel copy of the registry running the *new* wasm against a
*copy* of the live state. You exercise it, watch it, and only after it looks
right do you propose the swap on the real registry.

This document is the end-to-end runbook. It is linked from step 4 of the
"Upgrading a live registry" section in `DEPLOY.md`.

## Why bother

Rollback exists. Rollback also requires **noticing the problem first**. A
canary lets you notice it before the live contract is ever touched, when
rolling back is free — tear the canary down, nothing changed.

Common classes of bug a canary catches that CI does not:

- Storage layout drift: a deserialise against real stored values that a unit
  test with synthetic values glides past.
- Interaction with the live token: fee-on-transfer surprises, allowance
  behaviour under real balances, SAC quirks.
- Event payload changes that a downstream indexer silently drops.
- Resource usage regressions that only show up at production scale.

## Prerequisites

- The new wasm is built and uploaded (step 3 of the upgrade runbook). You
  have its hash from `stellar contract upload`.
- You have the live contract id (`LIVE_REGISTRY`) and admin identity.
- You have a scratch deployer identity funded on the target network. Use a
  separate key from the live admin so the canary cannot be mistaken for an
  authoritative instance.
- The live registry is not paused (a paused source makes seeding harder).

## 1. Deploy a fresh instance from the new wasm

```bash
stellar contract deploy \
  --wasm-hash <new-wasm-hash> \
  --source lumina-deployer-canary \
  --network testnet
```

Save the returned contract id as `CANARY_REGISTRY`. The two instances share
no state; the canary has its own admins, its own staking config, its own
proposals.

## 2. Initialise the canary

Initialise exactly as you would a fresh registry — the same admin set and
threshold as the live one, so proposals behave the same under test.

```bash
stellar contract invoke --id $CANARY_REGISTRY --source lumina-deployer-canary \
  --network testnet -- initialize \
  --admins '["<admin-G...>", "<admin-G...>", "..."]' \
  --threshold <same-as-live>
```

If staking is configured on live, open it on the canary too with
`propose_configure_staking` (same token, scratch treasury), and run the
proposal through to execution. A canary that does not share staking config
is a weaker test.

## 3. Seed with representative state

The goal is state the new code has to deserialise and traverse, not state
that passes every assertion. A good subset is:

- 20–50 registrations sampled across categories via
  `get_active_contracts_after`.
- At least one registration per staking state: unstaked, staked, verified,
  slashed-with-response, within a slash lock window.
- A few deactivated entries so reactivation and listing paths are covered.
- At least one historical proposal of each `ProposalAction` kind that the
  upgrade might change the handling of.

Use `scripts/upgrade_canary_seed.sh <live_id> <canary_id>` if present.
Otherwise seed by calling `register_contract`, `stake`, `set_categories`
etc. against `$CANARY_REGISTRY` as the scratch owner, mirroring the
metadata read from live.

Document what you seeded in the upgrade ticket. You cannot prove the canary
was representative later unless you record it now.

## 4. Exercise the new behaviour

Run a smoke pass that covers at least:

- `get_version` returns the new version string.
- `get_active_contracts_after` paginates stably over the seeded set.
- Register a new contract, update its metadata, set categories, deactivate.
- Post a stake, read reputation, propose+approve+execute a slash, call
  `respond_to_slash`, withdraw after the lock clears.
- Open one proposal of each `ProposalAction` kind, approve to threshold,
  wait out the timelock, execute. Observe events and compare their fields
  to `EVENTS.md`.
- The failure paths the change touches: feed them bad input and confirm
  the error codes still match `RegistryError`.

If a smoke script exists in `scripts/`, use it. If not, write the
assertions you make into the ticket so the next upgrade can reuse them.

## 5. Observe

Leave the canary running for at least the timelock interval you plan to
wait on the live upgrade. During that window:

- Point a copy of `lumina-backend` at `$CANARY_REGISTRY` and let it index.
  Confirm every event the indexer cares about still parses.
- If any downstream consumer is affected (frontend, GraphQL schema), hit
  the canary from it in a staging profile.

Record what you saw. "No regressions observed over N ledgers against the
seeded subset" is a specific, verifiable statement; "looked fine" is not.

## 6. Decide

- **Canary passed, no regressions.** Proceed to step 5 of the upgrade
  runbook (propose, approve, execute) against the live registry. Include
  the canary's contract id and the summary of your smoke pass in the
  proposal metadata or ticket.
- **Canary surfaced a regression.** Do not propose the live upgrade. Fix
  the code, rebuild (step 3 of the upgrade runbook), and re-run the
  canary from a fresh deploy. The old canary stays around as a reference
  but is not reused — a fresh wasm gets a fresh canary.
- **Canary surfaced something ambiguous.** Treat it as a failure until
  you can explain it. Governance proposals cannot be recalled.

## 7. Tear down

Once the live upgrade is in and verified, the canary is no longer useful.

```bash
# Optional: pause the canary so no one mistakes it for authoritative
stellar contract invoke --id $CANARY_REGISTRY --source lumina-deployer-canary \
  --network testnet -- propose_set_paused --proposer <admin-G...> --paused true
# approve + execute as usual

# Let its TTL lapse by not extending it.
```

You do not need to delete the canary — on Soroban you cannot, and letting
the entry expire is the cheapest path.

## Keeping the runbook current

A canary step that is skipped once becomes a canary step that is skipped
every time. If the rollout team diverges from this file for good reason,
amend the file in the same PR that changes the practice. The upgrade
ticket should be able to cite the file verbatim; "we did the canary" is a
checklist item, "we did the canary as documented in `UPGRADE_CANARY.md`
at commit `<sha>`" is an audit trail.
