# Stake token risks for registrants

If you post a stake, the token you use to stake is **not** the registry. It
is a separate Stellar asset contract, controlled by its own issuer. The
registry inherits whatever behaviour that issuer has configured. Before you
stake, know what that behaviour is, and what the registry does — and cannot
do — when it misbehaves.

## What can go wrong

### The issuer freezes the registry's trustline

Stellar assets support authorization flags. The issuer can flip the
registry's trustline from authorized to unauthorized, which makes every
transfer **out** of the registry fail. Once that happens:

- `withdraw_stake` fails with a token error. Your stake is recorded in the
  registry's books, but the token refuses to move it back to you.
- `execute_proposal` on a passed `Slash` fails for the same reason. The
  slash does not land until the trustline is re-authorized.
- `execute_proposal` on `WithdrawFromTreasury` fails.

The registry's own invariants hold: `failed_stake_transfer_records_no_stake`
and `failed_withdraw_transfer_keeps_the_stake_recorded` in `registry/src/lib.rs`
assert that **no bookkeeping change survives a failed transfer**. Your stake
is not lost — it is stuck — and if the issuer re-authorizes the trustline a
retry works, as `failed_slash_transfer_leaves_stake_history_and_proposal_untouched`
demonstrates.

### The issuer freezes your trustline

Same mechanism, but for your address. `stake` fails because the token will
not debit you. You keep your balance; the registry records no stake.
Nothing to recover; you simply cannot open a new stake until the issuer
re-authorizes you.

### The issuer clawbacks the registry's balance

If the asset is clawback-enabled, the issuer can pull tokens directly from
the registry's balance. This is not something the registry can prevent or
even observe synchronously. The accounting check at the start of
`execute_proposal::Slash` (`ContractBalanceInsufficient`) catches it the
next time the registry tries to move stake: the registry's recorded total
no longer matches what the token reports, and slashes are refused rather
than compound the gap.

### The token is fee-on-transfer

Fee-on-transfer tokens charge a percentage on every transfer, so the
registry receives less than the amount it debits. The registry's accounting
is based on the amount **asked for**, not the amount **received**, so a
fee-on-transfer token causes drift that `ContractBalanceInsufficient`
eventually catches but that cannot be unwound retroactively.

**Fee-on-transfer tokens are not supported as stake tokens.** Governance is
expected to choose a plain SEP-41 token at `propose_configure_staking`
time, and the solvency check in `execute_proposal::Slash` is a late backstop
against someone doing it anyway.

### The token issuer is compromised

Stake trust floors out at the issuer. A compromised issuer can freeze every
trustline on the asset at once, which freezes every stake in the registry
at once. Recovery is whatever the asset's issuance policy allows (often
nothing). This is a known limit; staking only makes sense against an asset
whose issuance you trust.

## What the registry does **not** do about this

- There is no circuit breaker that pauses the staking entrypoints if the
  trustline is unauthorized. Each call fails cleanly from the token; no
  further state changes.
- The admin cannot forcibly move your stake out of a frozen balance. The
  stake is accounted against the registry's own balance; it moves only
  via a token transfer the asset's authorization system has to allow.
- There is no swap path to migrate stake from one token to another. If
  governance reconfigures the stake token after you have staked, your
  old stake stays in the old token and must be withdrawn in that token
  first. `PreviousStakeToken` / `PreviousTreasury` keys preserve the
  reference across a reconfiguration, but withdrawing still requires the
  old token to permit the transfer.

## Before you stake

1. Check the asset's authorization flags. Issuers that hold
   `AUTH_REVOCABLE_FLAG` can un-authorize a trustline at any time.
2. Check the asset's clawback flag. `AUTH_CLAWBACK_ENABLED_FLAG` means
   the issuer can pull tokens directly, which the registry's accounting
   cannot catch synchronously.
3. Check `get_staking_config` so you know which token the registry is
   actually accepting today. Reconfigurations change the token prospectively
   for new stakes.
4. Only stake what you are willing to see frozen in the worst case. The
   registry's behaviour is predictable (clean failures, no accounting
   drift), but a frozen stake is still an unusable stake.

## For governance

Choosing the stake token is a one-time decision with permanent
consequences. `propose_configure_staking` lands under a timelock to give
registrants time to object. The checklist for reviewers:

- The token's issuer is a party registrants can reasonably rely on.
- The issuer's authorization policy is documented somewhere persistent.
- The token is not fee-on-transfer (SEP-41 does not require this; test
  with a small transfer in and out).
- The registry's address holds the trustline before staking opens, or
  the SAC auto-creates one on first use (XLM via the native SAC does).

## Tests that pin these invariants

Inside `registry/src/lib.rs` under `#[cfg(test)]`:

- `failed_stake_transfer_records_no_stake`
- `failed_top_up_leaves_the_existing_stake_untouched`
- `failed_withdraw_transfer_keeps_the_stake_recorded`
- `failed_slash_transfer_leaves_stake_history_and_proposal_untouched`
- `a_failed_transfer_on_one_registration_does_not_disturb_another`

And in `registry/tests/`:

- `frozen_stake_token.rs` — issue #253; the same guarantees exercised
  through the public client with an inline failing token.
