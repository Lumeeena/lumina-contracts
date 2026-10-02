# Changelog

All notable changes to the Lumina Registry contract will be documented in this file.

The format is based on [Keep a Changelog](https://keepadhangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## Version 4 (unreleased)

### Interface Changes
- **Optional proposal description** -- every proposal-creation entrypoint now accepts an optional human-readable `description` (`String`) rationale. The description is stored on the `Proposal`, returned by `get_proposal`, and included in the `proposal_proposed` event so the on-chain record is self-describing. The length is bounded by the contract and an over-long description is rejected with `RegistryError::InvalidMetadata`.
- **Enhanced `staking`configured` event** — now emits `(prev_token, prev_treasury, token_id, treasury)` of just `(token_id, treasury)`. This makes reconfigurations distinguishable from first configuration, preventing stranded stakes when changing the stake token.
- **Token change protection** — `propose_configure_staking` now refuses to change the stake token if any stakes are held, returning `RegistryError::StakeNotEmpty`. This prevents the critical scenario where changing the token while stakes exist would strand them in the old token.
- **Previous values tracking** — added `PreviousStakeToken` and `PreviousTreasury` `DataKey` entries that store the old token/treasury before overwriting, visible via the enhanced event.
- **Governance-set `MinimumStake`** — added a governance-controlled minimum stake that contracts must hold to register. Defaults. to zero, so existing deployments behave exactly as before until governance configures it.
- **`register_contract` enforces the minimum** — when a non-zero minimum is configured, registering without staking at least the minimum is refused with `RegistryError::InsufficientStake`. The stake must be provided in the same call.

### Storage Changes
- **Added `DataKey::PreviousStakeToken`** — stores the previous staking token address prior to reconfiguration. Variant names are preserved in `\#contracttype]` enum, so adding this new variant is safe for upgrades.
- **Added `DataKey::PreviousTreasury`** — stores the previous treasury address prior to reconfiguration. Same upgrade-safe semantics.
- **Added `DataKey::MinimumStake`** — stores the governance-set minimum stake required to register a contract. Absent means zero (no minimum).
- Storage changes are backwards-compatible: new code can decode entries written by old code, and vice-versa, because variant names are preserved.

### Behaviour Notes
- **Withdrawing below the minimum** — `withdraw_stake` refuses to reduce a contract's stake below the configured `MinimumStake` while the contract remains active. To withdraw below the minimum, the owner must first deregister the contract, which deactivates the listing rather than silently leaving an under-collateralised entry in the registry. This interaction is tested for both the default-zero and non-zero minimum cases.

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
-  Initial `DataKey` enum: `Admins`, `Threshold`, `ProposalCount`, `ContractCount`, `TotalRegistered`, `Contract`, `OwnerContracts`, `AllContracts`, `StakeToken`, `Treasury`, `Stake`, `Verified`, `Slashes`, `WithdrawLockedUntil`, `Categories`, `ByCategory`, `AllowlistEnabled`, `Allowlisted`, `RegistrationRateLimit`, `RegistrationRateWindow`, `Admin`.