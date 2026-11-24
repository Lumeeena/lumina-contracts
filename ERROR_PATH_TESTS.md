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

### #259 `ContractNotFound` (variant 4), #262 `NotInitialized` (variant 7), #266 `AlreadyApproved` (variant 11), #267 `NotAdmin` (variant 12)
- **Tests**: `registry/tests/registry_error_paths_259_262_266_267.rs`, each asserting the exact `Err(Ok(RegistryError::...))`.
- **#259**: `deactivate` on an address that was never registered returns `ContractNotFound`.
- **#262**: `__constructor` always installs a bootstrap admin, so `NotInitialized` is not reachable through the public API of a normally deployed registry. The test removes the `Admins`, `Admin` and `Threshold` instance keys with `env.as_contract` to model a deployment whose governance state is missing, then asserts `get_admins`, `get_admin`, `get_threshold` and `propose_deactivate` return it.
- **#266**: the same admin approving the same proposal twice returns `AlreadyApproved`; approving a different proposal is still allowed.
- **#267**: a stranger calling `propose_deactivate` or `approve_proposal` returns `NotAdmin`.
- **Status**: ✅ Covered by dedicated test.

## How to Run

```sh
cd registry && cargo test
```
