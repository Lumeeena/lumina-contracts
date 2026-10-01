# Changelog

All notable changes to the Lumina Registry contract will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## Version 4 (unreleased)

### Interface Changes
- **Optional proposal description** -- every proposal-creation entrypoint now accepts an optional human-readable `description` (`String`) rationale. The description is stored on the `Proposal`, returned by `get_proposal`, and included in the `proposal_proposed` event so the on-chain record is self-describing. The length is bounded by the contract and an over-long description is rejected with `RegistryError::InvalidMetadata`.
- **Enhanced `staking_configured` event** — now emits `(prev_token, prev_treasury, token_id, treasury)`, instead of just `(token_id, treasury)`. This makes reconfigurations distinguishable from first configuration, preventing stranded stakes when changing the stake token.
- **Token change protection** — `propose_configure_staking` now refuses to change the stake token if any stakes are held, returning `RegistryError::StakeNotEmpty`. This prevents the critical scenario where changing the token while stakes exist would strand them in the old token.
- **Previous values tracking** — added `PreviousStakeToken` and `PreviousTreasury` `DataKey` entries that store the old token/treasury before overwriting, visible via the enhanced event.
- **Added `cancel_proposal`** — callable by the proposer or by a threshold of admins, allowing a mistaken or superseded proposal to be withdrawn before execution. A cancelled proposal cannot be approved or executed, and emits `proposal_cancelled`.

### Storage Changes
- **Added `DataKey::PreviousStakeToken`** -- stores the previous staking token address prior to reconfiguration. Variant names are preserved in `#[contractype]` enum, so adding this new variant is safe for upgrades.
- **Added `DataKey::PreviousTreasury`** -- stores the previous treasury address prior to reconfiguration. Same upgrade-safe semantics.
- Storage changes are backwards-compatible: new code can decode entries written by old code, and vice-versa, because variant names are preserved.

---

## Version 3

### Interface Changes
- Added `propose_change_threshold` governance flow for modifying the admin threshold with timelock protection.
- Enhanced `propose_configure_registration_rate_limit` with explicit rate limit window configuration.

### Storage Changes
- Added `RegistrationRateWindow` and `RegistrationRateLimit` `DataKey` entries for per-owner rate limiting.
- Added `TotalRegistered` counter surviving deregistration.

---

## Version 2

### Interface Changes
- Initial registry-v2 migration with multi-admin governance.
- Added `initialize` constructor flow and `__constructor` for new deployments.

### Storage Changes
- Complete rewrite of storage schema with `registry-v2` compatibility layer.
- Added `Admin`, `Admins`, `Threshold`, `ProposalCount` entries for governance.

---

## Version 1

### Interface Changes
- Initial Lumina Registry contract deployment.
- Basic contract registration, metadata, and discovery.

### Storage Changes
- Initial `DataKey` enum: `Admins`, `Threshold`, `ProposalCount`, `ContractCount`, `TotalRegistered`, `Contract`, `OwnerContracts`, `AllContracts`, `StakeToken`, `Treasury`, `Stake`, `Verified`, `Slashes`, `WithdrawLockedUntil`, `Categories`, `ByCategory`, `AllowlistEnabled`, `Allowlisted`, `RegistrationRateLimit`, `RegistrationRateWindow`, `Admin`.