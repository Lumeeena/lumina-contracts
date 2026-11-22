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

## How to Run

```sh
cd registry && cargo test
```
