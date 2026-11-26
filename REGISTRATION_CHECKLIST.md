# Integration checklist for projects registering

This is a decision aid, not a tutorial. `DEPLOY.md` covers the commands. Read
through this before registering so you understand what you are committing to.

## What you get from registering

- **A canonical on-chain entry** mapping your contract address to a name,
  description, categories, and owner key. Readers (indexers, aggregators,
  wallets) discover you via `get_active_contracts_after`,
  `get_contracts_by_category_after`, `get_contracts_by_owner`.
- **Ownership-gated metadata updates.** You, and only you, can call
  `update_metadata` and `set_categories` for your entry.
- **Standing in the registry's reputation system.** Optional. You can stake,
  be verified, and build on-chain history that readers can weigh.
- **Compatibility with downstream tooling.** `lumina-backend` and consumers
  that index from the registry start seeing you automatically.

## What registration does not get you

- It is not an audit. Verification (if you ever earn it) is a governance
  statement, not an engineering review.
- It is not an endorsement. Readers are told your manifest, your stake, and
  your history; they draw their own conclusions.
- It is not an uptime guarantee. If the indexer is down, your registration is
  still readable directly from the contract but no downstream view will show
  it.

## What it costs

- **Registration fee.** Governance can set a one-off fee
  (`propose_set_registration_fee`). Today it starts at zero. Check with
  `get_registration_fee` before you register.
- **Stake, if you post one.** Stake is denominated in the configured token
  (XLM by default on most deployments). Governance may configure a minimum
  (`propose_configure_minimum_stake`). A registration without stake works
  but will show a zero reputation line.
- **Rent.** Soroban charges persistent-entry rent. For a single registration
  with a short name and description the per-ledger cost is small but not zero
  — plan for the entry's TTL to be bumped periodically. If the entry's TTL
  expires, discovery stops until it is restored (`stellar contract extend`).
- **Transaction fees.** The usual Stellar base fee for each call you make
  (register, update, stake, withdraw).

## Ongoing obligations

- **Keep your contact point current.** The registered owner key is the only
  one that can call `update_metadata`, `set_categories`,
  `respond_to_slash`, and `withdraw_stake`. If you lose it, see
  "Transferring ownership" below.
- **Keep your metadata accurate.** Governance can slash for misrepresented
  metadata. If your contract's behaviour changes materially, call
  `update_metadata` the same day.
- **Keep the entry rented.** Monitor its TTL; bump it well before expiry.
- **If you staked, watch slash events.** The registry emits events (see
  `EVENTS.md`); subscribe through an indexer, or poll `get_slashes`
  periodically. A slash within the lock window freezes withdrawal.

## What governance can do to a registration

Every one of these goes through propose / approve / timelock / execute, not
by a single admin:

- **Verify or un-verify** your registration (`propose_set_verified`).
- **Slash** some of your stake and mark a `SlashRecord` against you
  (`propose_slash`). This is permanent. See `SLASH_PROJECT_RESPONSE.md`.
- **Deactivate** your entry (`propose_deactivate`). Stops discovery; you can
  re-register after resolving.
- **Pause the whole registry** (`propose_set_paused`). Everything is
  read-only until unpaused.
- **Change the registration fee, minimum stake, or allowlist policy**
  (`propose_set_registration_fee`, `propose_configure_minimum_stake`,
  `propose_set_allowlist_enabled`). This changes the terms for *future*
  actions, not entries already in place.

Governance cannot:

- Rewrite your metadata. It can `propose_deactivate` if the metadata is
  misleading, but it does not edit your fields.
- Transfer your ownership to another key without a `transfer_ownership`
  call signed by you (or by the admin, which is documented and auditable).
- Drain your stake directly. A slash is a bounded action with a stored
  `reason`; it does not unlock the whole balance.

## Transferring ownership

If you ever need to move a registration to another key — rotating a signer,
handing off a project — call `transfer_ownership` as the current owner (or
have the admin call it, which is public and auditable). There is no recovery
if you lose the key and the admin is unavailable.

## How to leave

Registration is reversible. To exit:

1. `deactivate --caller <owner> --contract_id <C...>` — stops discovery
   immediately.
2. If you staked, wait for any slash lock to clear
   (`get_reputation` → `withdraw_locked_until` against the current ledger),
   then `withdraw_stake`.
3. Let the persistent entry's TTL lapse. Nothing on-chain enforces this;
   you simply stop paying rent.

An entry stays in `get_contracts_by_owner` (with its deactivated flag) even
after you leave, so your on-chain history is preserved for anyone reading
later.

## Checklist before you call `register_contract`

- [ ] Owner key is backed up, under multi-sig if you can.
- [ ] The categories you plan to use are a fair description of what your
      contract does.
- [ ] The name and description would read correctly without context — a
      reader finding you through category discovery has no other signal.
- [ ] You know the current registration fee and allowlist state
      (`get_registration_fee`, `get_allowlist_enabled`).
- [ ] If you intend to stake, you know the current minimum and have enough
      balance in the configured token (`get_configured_staking_token`,
      `get_minimum_stake`).
- [ ] You have read `SLASH_PROJECT_RESPONSE.md` so you know what happens if
      things go wrong.
- [ ] You can commit to monitoring slash events and keeping metadata and TTL
      current for as long as the registration is live.

## Related

- `DEPLOY.md` — the commands referenced above.
- `SLASH_PROJECT_RESPONSE.md` — if your registration is ever slashed.
- `GOVERNANCE_PLAYBOOK.md` — how governance decides.
- `EVENTS.md` — the events your project can subscribe to.
