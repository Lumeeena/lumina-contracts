# Registry Error Path Tests

This documents the dedicated tests added for each `RegistryError` variant (#276–#283).

## Tests Added

### #276 `StakeNotEmpty` (variant 21)
- **Condition**: `deregister` is called while the registration still holds stake.
- **Test**: `deregister_requires_deactivated_and_unstaked` already asserts this at the point where `deactivate` is called but stake has not been withdrawn.
- **Status**: ✅ Covered by existing test.

### #277 `InvalidRateLimit` (variant 22)
- **Condition**: Rate limit config has zero `window_ledgers` with non-zero `limit`, or window exceeds `max_ttl`.
- **Test**: `test_invalid_rate_limit_zero_window` — configures a rate limit with `window_ledgers = 0` and `limit > 0`.
- **Status**: ✅ Covered by new test.

### #278 `NotAllowlisted` (variant 23)
- **Condition**: Owner is not on the allowlist while permissioned registration is enabled.
- **Test**: `test_not_allowlisted_blocked` — enables the allowlist, then attempts to register as a non-allowlisted owner.
- **Status**: ✅ Covered by new test.

### #279 `RegistrationRateLimited` (variant 24)
- **Condition**: Per-owner registration rate limit exceeded for the current window.
- **Test**: `test_registration_rate_limited` — sets a rate limit of 1 per window, registers once, then attempts a second registration within the same window.
- **Status**: ✅ Covered by new test.

### #268 `InvalidThreshold` (variant 13)
- **Condition**: Threshold is zero or exceeds the admin count, or an admin removal would leave the threshold unsatisfiable.
- **Test**: `registry/tests/invalid_threshold.rs` — covers rejection at proposal time and at execution time for both `ChangeThreshold` and `RemoveAdmin` actions.
- **Status**: ✅ Covered by dedicated test.

### #269 `AlreadyExecuted` (variant 14)
- **Condition**: `execute_proposal` or `approve_proposal` is called on a proposal that has already been executed.
- **Test**: `registry/tests/already_executed.rs` — asserts both re-execution and late approval return `AlreadyExecuted`, and that a fresh proposal still executes.
- **Status**: ✅ Covered by dedicated test.

### #261 `NotOwner` (variant 6)
- **Condition**: a caller who is not the recorded owner calls `deregister` or `update_metadata`.
- **Test**: `registry/tests/registry_error_paths_261_265.rs` — `deregister_by_a_non_owner_returns_not_owner` and `update_metadata_by_a_non_owner_returns_not_owner`.
- **Status**: ✅ Covered by dedicated test.

### #263 `ProposalNotFound` (variant 8)
- **Condition**: `approve_proposal` or `execute_proposal` is called with a proposal id that was never created.
- **Test**: `approving_an_unknown_proposal_returns_proposal_not_found` and `executing_an_unknown_proposal_returns_proposal_not_found` in `registry/tests/registry_error_paths_261_265.rs`.
- **Status**: ✅ Covered by dedicated test.

### #264 `ThresholdNotMet` (variant 9)
- **Condition**: `execute_proposal` is called on a proposal with fewer approvals than the threshold.
- **Test**: `execute_below_threshold_returns_threshold_not_met` (threshold 2, one approval, timelock passed) and `execute_with_no_approvals_returns_threshold_not_met`.
- **Status**: ✅ Covered by dedicated test.

### #265 `TimelockNotElapsed` (variant 10)
- **Condition**: `execute_proposal` is called before `TIMELOCK_LEDGERS` ledgers have passed since the proposal became ready.
- **Test**: `execute_before_the_timelock_returns_timelock_not_elapsed` — fails straight after approval and one ledger short, succeeds at the boundary.
- **Status**: ✅ Covered by dedicated test.

## How to Run

```sh
cd registry && cargo test
```
