# Changelog

All notable changes to the Lumina Registry contract will be documented in this file.

The format is based on [Keep a Changelog](https://keepadhangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## Version 4 (unreleased)

### Interface Changes
- **Optional proposal description** -- every proposal-creation entrypoint now accepts an optional human-readable `description` (`String`) rationale. The description is stored on the `Proposal`, returned by `get_proposal`, and included in the `proposal_proposed` event so the on-chain record is self-describing. The length is bounded by the contract and an over-long description is rejected with `RegistryError::InvalidMetadata`.
- **Enhanced `staking`configured` event** -- now emits `(prev_token, prev_treasury, token_id, treasury)` instead of just `(token_id, treasury)`. This makes reconfigurations distinguishable from first configuration, preventing stranded stakes when changing the stake token.
- **Token change protection** -- `propose_configure_staking` now refuses to change the stake token if any stakes are held, returning `RegistryError::StakeNotEmpty`. This prevents the critical scenario where changing the token while stakes exist would strand them in the old token.
- **Previous values tracking** -- added `PreviousStakeToken` and `PreviousTreasury` `DataKey` entries that store the old token/treasury before overwriting, visible via the enhanced event.
- **Reputation decay** -- `get_reputation` now reports a decayed score for registrations that have been inactive for multiple ledgers. The decay is a compound half-life applied on read only, so no storage is written and the underlying stake/verified flags remain unchanged. See the "Reputation decay" section below for the curve and parameters.

### Storage Changes
- **Added `DataKey::PreviousStakeToken`** -- stores the previous staking token address prior to reconfiguration. Variant names are preserved in `#[contractype]` enum, so adding this new variant is safe for upgrades.
- **Added `DataKey::PreviousTreasury`** -- stores the previous treasury address prior to reconfiguration. Same upgrade-safe semantics.
- **Added `DataKey::LastActivity`** -- records the ledge sequence number of the most recent activity for a registration (verification, registration, or stake change). This is the only new write and it is updated at the existing mutation points, not on read.
- Storage changes are backwards-compatible: new code can decode entries written by old code, and vice-versa, because variant names are preserved.

### Reputation decay

Reputation is computed on read from the stored signals (verified status and stake) and the ledger elapsed since the registration's last activity. Nothing is written back to storage when a reputation is read, so there is no write amplification and the cost of a read is constant regardless of how long a registration has been idle.

The decay function is a compound half-life exponential:

```
decayed = base * (1/2)^(elapsed / half_life)
```

where:

- `base` is the undecayed score derived from the registration's verified status and stake.
- `elapsed` is the number of ledgers since the registration's last activity (`LastActivity`), clamped at zero if the current ledger is at or before the last activity.
- `half_life` is the number of ledgers required for the score to fall to half of its undecayed value.

Parameters:

- `DECAY_HALF_LIFE_LEDGERS` = 1,000,000` ledgers (approximately 60 days at 5 seconds per ledger). This is the default half-life used by `get_reputation`.
- The exponent is computed in integer arithmetic using a fixed-point representation of the ratio `(elapsed / half_life)` to keep the result deterministic across environments.
- The result is floored to an integer score and never increases as elapsed ledgers grow.

Because the decay is applied at read time, a long-inactive registration reports a lower score than an active registration with an identical history, without any additional writes.

---

## Version 3

### Interface Changes
-  Added `propose_change_threshold` governance flow for modifying the admin threshold with timelock protection.
-  Enhanced `propose_configure_registration_rate_limit` with explicit rate limit window configuration.

### Storage Changes
-  Added `RegistrationRateWindow` and `RegistrationRateLimit` `DataKey` entries for per-owner rate limiting.
-  Added `TotalRegistered` counter surviving deregistration.

---

## Version 2

### Interface Changes
-  Initial registry-v2 migration with multi-admin governance.
-  Added `initialize` constructor flow and `__constructor` for new deployments.

### Storage Changes
-  Complete rewrite of storage schema with `registry-v2` compatibility layer.
-  Added `Admin`, `Admins`, `Threshold`, `ProposalCount` entries for governance.

---

## Version 1

### Interface Changes
-  Initial Lumina Registry contract deployment.
-  Basic contract registration, metadata, and discovery.

### Storage Changes
- Initial `DataKey` enum: `Admins`, `Threshold`, `ProposalCount`, `ContractCount`, `TotalRegistered`, `Contract`, `OwnerContracts`, `AllContracts`, `StakeToken`, `Treasury`, `Stake`, `Verified`, `Slashes`, `WithdrawLockedUntil`, `Categories`, `ByCategory`, `AllowlistEnabled`, `Allowlisted`, `RegistrationRateLimit`, `RegistrationRateWindow`, `Admin`.
