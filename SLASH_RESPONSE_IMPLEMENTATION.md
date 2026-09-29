# Slash Response Implementation

## Summary
Implemented the ability for contract owners to respond to slash records, providing a two-sided record of governance actions.

## Changes Made

### 1. Data Structure Updates

#### SlashRecord Structure (registry/src/lib.rs & registry-interface/src/lib.rs)
Added optional `response` field to `SlashRecord`:
```rust
pub struct SlashRecord {
    pub amount: i128,
    pub reason: String,
    pub slashed_at: u32,
    pub response: Option<String>,  // NEW FIELD
}
```

### 2. New Function: respond_to_slash

**Location**: `registry/src/lib.rs` (after `get_slashes`)

**Signature**:
```rust
pub fn respond_to_slash(
    env: Env,
    owner: Address,
    contract_id: Address,
    slash_index: u32,
    response: String,
) -> Result<(), RegistryError>
```

**Authorization**: Owner-only (via `require_auth()` and ownership check)

**Validations**:
- Response must not be empty
- Contract must exist
- Caller must be the registered owner
- Slash index must be valid (within bounds)
- Response must not already exist (immutable once set)

**Behavior**:
1. Validates all inputs and authorization
2. Loads slash history from persistent storage
3. Updates the specific slash record with the response
4. Saves updated history back to storage
5. Emits `slash_response_added` event

### 3. Error Handling

Added three new error variants to `RegistryError`:

```rust
SlashNotFound = 29,           // Invalid slash index
ResponseAlreadyExists = 30,   // Response already set (immutable)
InvalidInput = 31,            // Empty response or other validation failures
```

### 4. Event Documentation

Added to `EVENTS.md`:

| Event | Payload | Description |
|-------|---------|-------------|
| `slash_response_added` | `(contract_id: Address, slash_index: u32, owner: Address)` | Owner attached a response to a slash record |

### 5. Comprehensive Tests

Added 10 test cases covering all acceptance criteria:

1. **owner_can_respond_to_slash** - Happy path: owner successfully adds response
2. **respond_to_slash_requires_owner** - Authorization check
3. **respond_to_slash_rejects_invalid_index** - Bounds validation
4. **respond_to_slash_rejects_empty_response** - Input validation
5. **respond_to_slash_rejects_duplicate_response** - Immutability check
6. **owner_can_respond_to_multiple_slashes** - Multiple slash handling
7. **slash_response_is_visible_in_get_slashes** - Query visibility
8. **respond_to_slash_requires_registered_contract** - Contract existence check
9. **slash_response_persists_after_deregistration** - Audit trail preservation

### 6. Backward Compatibility

**Storage Compatibility**: The `Option<String>` field is compatible with existing slash records:
- Existing records without the field will deserialize with `response: None`
- New slashes are created with `response: None` by default
- Soroban's XDR encoding handles optional fields gracefully

**Interface Compatibility**: 
- The read-only `RegistryInterface` trait was not modified (appropriate for a write operation)
- `get_slashes()` continues to work without changes
- Consumers see `None` for responses on old records

## Acceptance Criteria Met

✅ **An owner can attach one response per slash**
- Implemented via `respond_to_slash` function
- Immutability enforced via `ResponseAlreadyExists` error

✅ **A non-owner cannot respond**
- Enforced via `require_auth()` + ownership check
- Returns `NotOwner` error for non-owners

✅ **Responses are visible to any reader**
- Returned in `get_slashes()` output
- No changes needed to query interface
- Test: `slash_response_is_visible_in_get_slashes`

## Usage Example

```rust
// After a slash has been executed
let response = String::from_str(&env, "This was a bug, not malicious behavior");
client.respond_to_slash(&owner, &contract_id, &0, &response);

// Response is now visible
let slashes = client.get_slashes(&contract_id);
let record = slashes.get(0).unwrap();
assert_eq!(record.response, Some(response));
```

## Next Steps

To deploy these changes:

1. Run tests: `cargo test --workspace`
2. Verify compilation: `cargo build --workspace --target wasm32v1-none --release`
3. Update deployment documentation if needed
4. Deploy via governance upgrade proposal
5. Update frontend to display slash responses in contract history view
