# Slash Response Usage Examples

## Overview
This document provides practical examples of how to use the new `respond_to_slash` functionality.

## Basic Usage

### Example 1: Owner Responds to a Slash

```rust
use soroban_sdk::{Env, Address, String};
use lumina_registry::LuminaRegistryClient;

// Assume we have:
// - env: Initialized environment
// - client: Registry client
// - owner: Contract owner address
// - contract_id: The slashed contract address

let slash_index = 0u32;  // Index of the slash to respond to (0-based)
let response = String::from_str(&env, "This slash was based on incorrect information. The contract behavior was within acceptable parameters and approved by security audit.");

// Submit the response
client.respond_to_slash(&owner, &contract_id, &slash_index, &response);

// Response is now visible to all readers
let slashes = client.get_slashes(&contract_id);
let record = slashes.get(0).unwrap();
assert!(record.response.is_some());
```

### Example 2: Checking for Existing Response Before Submitting

```rust
use lumina_registry::RegistryError;

let slash_index = 0u32;
let response = String::from_str(&env, "Our explanation...");

// Check if response already exists
let slashes = client.get_slashes(&contract_id);
if let Some(record) = slashes.get(slash_index) {
    if record.response.is_some() {
        // Response already exists, cannot modify
        panic!("This slash already has a response");
    }
}

// Safe to submit
client.respond_to_slash(&owner, &contract_id, &slash_index, &response);
```

### Example 3: Responding to Multiple Slashes

```rust
// Contract received multiple slashes, owner wants to respond to each
let slashes = client.get_slashes(&contract_id);

for i in 0..slashes.len() {
    let record = slashes.get(i).unwrap();
    
    // Only respond if no response exists yet
    if record.response.is_none() {
        let response = String::from_str(
            &env, 
            &format!("Response to slash #{}: [owner's explanation]", i)
        );
        client.respond_to_slash(&owner, &contract_id, &i, &response);
    }
}
```

## Error Handling

### Example 4: Handling All Possible Errors

```rust
use lumina_registry::RegistryError;

let result = client.try_respond_to_slash(&owner, &contract_id, &slash_index, &response);

match result {
    Ok(_) => {
        // Success - response submitted
        println!("Response submitted successfully");
    }
    Err(Ok(RegistryError::ContractNotFound)) => {
        // Contract doesn't exist
        println!("Contract not found in registry");
    }
    Err(Ok(RegistryError::NotOwner)) => {
        // Caller is not the contract owner
        println!("Only the contract owner can respond to slashes");
    }
    Err(Ok(RegistryError::SlashNotFound)) => {
        // Invalid slash index
        println!("Slash index {} is out of bounds", slash_index);
    }
    Err(Ok(RegistryError::ResponseAlreadyExists)) => {
        // Response already submitted
        println!("This slash already has a response");
    }
    Err(Ok(RegistryError::InvalidInput)) => {
        // Empty response
        println!("Response cannot be empty");
    }
    Err(e) => {
        println!("Unexpected error: {:?}", e);
    }
}
```

## Query Examples

### Example 5: Displaying Slash Records with Responses

```rust
// Retrieve all slashes for a contract
let slashes = client.get_slashes(&contract_id);

for (index, record) in slashes.iter().enumerate() {
    println!("Slash #{}", index);
    println!("  Amount: {}", record.amount);
    println!("  Reason: {}", record.reason);
    println!("  Slashed at ledger: {}", record.slashed_at);
    
    match &record.response {
        Some(response) => println!("  Owner response: {}", response),
        None => println!("  Owner response: [No response yet]"),
    }
    println!();
}
```

### Example 6: Getting Contract Profile with Slash History

```rust
// Get comprehensive contract information including slashes
let profile = client.get_contract_profile(&contract_id).unwrap();

println!("Contract: {}", profile.entry.name);
println!("Total slashed: {}", profile.reputation.slashed_total);

// Get detailed slash history
let slashes = client.get_slashes(&contract_id);
println!("Number of slashes: {}", slashes.len());

// Count responded vs unresponded slashes
let responded_count = slashes.iter()
    .filter(|s| s.response.is_some())
    .count();
    
println!("Slashes with owner response: {}", responded_count);
```

## Frontend Integration Examples

### Example 7: React Component (Conceptual)

```typescript
// Fetching slash data with responses
async function fetchSlashHistory(contractId: string) {
  const slashes = await registryClient.get_slashes({ contract_id: contractId });
  
  return slashes.map((slash, index) => ({
    index,
    amount: slash.amount,
    reason: slash.reason,
    slashedAt: slash.slashed_at,
    ownerResponse: slash.response || null,
    hasResponse: slash.response !== null
  }));
}

// UI Component
function SlashHistoryDisplay({ contractId }) {
  const [slashes, setSlashes] = useState([]);
  
  useEffect(() => {
    fetchSlashHistory(contractId).then(setSlashes);
  }, [contractId]);
  
  return (
    <div>
      {slashes.map(slash => (
        <div key={slash.index} className="slash-record">
          <h3>Slash #{slash.index}</h3>
          <p><strong>Amount:</strong> {slash.amount}</p>
          <p><strong>Governance Reason:</strong> {slash.reason}</p>
          
          {slash.hasResponse ? (
            <div className="owner-response">
              <strong>Owner Response:</strong>
              <p>{slash.ownerResponse}</p>
            </div>
          ) : (
            <p className="no-response">No owner response yet</p>
          )}
        </div>
      ))}
    </div>
  );
}
```

### Example 8: Submitting Response from Frontend

```typescript
// Function to submit a slash response
async function submitSlashResponse(
  contractId: string,
  slashIndex: number,
  response: string
) {
  try {
    // Validate input
    if (!response.trim()) {
      throw new Error("Response cannot be empty");
    }
    
    // Submit to blockchain
    await registryClient.respond_to_slash({
      owner: userAddress,
      contract_id: contractId,
      slash_index: slashIndex,
      response: response
    });
    
    console.log("Response submitted successfully");
    return { success: true };
    
  } catch (error) {
    if (error.code === 30) {
      return { success: false, error: "Response already exists" };
    } else if (error.code === 6) {
      return { success: false, error: "You are not the contract owner" };
    } else if (error.code === 29) {
      return { success: false, error: "Invalid slash index" };
    }
    
    return { success: false, error: error.message };
  }
}
```

## Backend/Indexer Examples

### Example 9: Indexing Slash Response Events

```typescript
// Event handler for slash_response_added events
async function handleSlashResponseEvent(event: Event) {
  const [contractId, slashIndex, owner] = event.data;
  
  // Fetch the updated slash record
  const slashes = await registryClient.get_slashes({ contract_id: contractId });
  const updatedSlash = slashes[slashIndex];
  
  // Update database
  await db.slashRecords.update({
    where: {
      contractId: contractId,
      index: slashIndex
    },
    data: {
      response: updatedSlash.response,
      respondedAt: new Date(),
      respondedBy: owner
    }
  });
  
  // Notify subscribers
  await notifySubscribers(contractId, {
    type: 'slash_response_added',
    slashIndex,
    response: updatedSlash.response
  });
}
```

### Example 10: GraphQL Query Example

```graphql
# Query for fetching slash records with responses
query GetContractSlashHistory($contractId: String!) {
  contract(id: $contractId) {
    id
    name
    reputation {
      slashedTotal
    }
    slashes {
      index
      amount
      reason
      slashedAt
      response
      hasResponse
    }
  }
}
```

## Testing Examples

### Example 11: Integration Test

```rust
#[test]
fn full_slash_response_workflow() {
    // Setup
    let (env, client, admin, token_id, treasury) = setup_staking();
    let (owner, target) = register_and_stake(&env, &client, &token_id, 1_000);
    
    // 1. Governance slashes the contract
    let reason = String::from_str(&env, "violated platform policy");
    let pid = client.propose_slash(&admin, &target, &200, &reason);
    pass_proposal(&env, &client, &admin, pid);
    
    // 2. Verify slash was recorded
    let slashes = client.get_slashes(&target);
    assert_eq!(slashes.len(), 1);
    assert_eq!(slashes.get(0).unwrap().response, None);
    
    // 3. Owner submits response
    let response = String::from_str(&env, "This was an accidental bug, not malicious");
    client.respond_to_slash(&owner, &target, &0, &response);
    
    // 4. Verify response is visible
    let slashes = client.get_slashes(&target);
    let record = slashes.get(0).unwrap();
    assert_eq!(record.response, Some(response.clone()));
    
    // 5. Verify immutability - cannot change response
    let new_response = String::from_str(&env, "Changed my mind");
    assert_eq!(
        client.try_respond_to_slash(&owner, &target, &0, &new_response),
        Err(Ok(RegistryError::ResponseAlreadyExists))
    );
    
    // 6. Response persists even after deregistration
    client.deactivate(&owner, &target);
    advance_ledger(&env, SLASH_LOCK_LEDGERS);
    client.withdraw_stake(&owner, &target);
    client.deregister(&owner, &target);
    
    let slashes = client.get_slashes(&target);
    assert_eq!(slashes.get(0).unwrap().response, Some(response));
}
```

## Best Practices

### 1. Response Content Guidelines
- Be professional and factual
- Provide evidence or references when possible
- Keep responses concise but informative
- Avoid inflammatory language
- Address the specific slash reason

### 2. Timing Considerations
- Respond promptly to maintain credibility
- Consider governance's perspective
- Gather facts before responding
- Don't rush - responses are immutable

### 3. Security Considerations
- Only contract owner can respond
- Responses are public and permanent
- Cannot be edited or deleted
- Consider impact on reputation before submitting

### 4. Integration Considerations
- Handle `None` responses gracefully in UI
- Cache slash data to reduce RPC calls
- Listen for `slash_response_added` events
- Provide clear UI for response submission
