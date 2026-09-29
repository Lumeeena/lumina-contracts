# Registry Self-Registration Example

A working example of a Soroban contract that registers itself with the Lumina Registry during deployment.

## Purpose

This example demonstrates:

1. **How to integrate registry registration** into your contract's deployment flow
2. **Type-safe cross-contract calls** using `RegistryInterfaceClient`
3. **A pattern other projects can copy** for automatic indexer discovery
4. **Integration testing** of the registration flow

## What This Contract Does

This is a minimal DeFi-style contract (token swap placeholder) that:

- Registers itself with the Lumina Registry during deployment via `__constructor`
- Stores the registry address and owner for future reference
- Provides example business logic (greeting, token swap)
- Allows the owner to update registration metadata
- Emits events that Lumina will index

## Why Self-Registration Matters

Without self-registration, deploying a new protocol requires two separate steps:

1. Deploy the contract
2. Manually register it with the registry (separate transaction)

With self-registration:

1. Deploy the contract → **automatically registered** ✓
2. Lumina indexers discover it immediately
3. No manual follow-up needed

## Architecture

```
┌─────────────────────────────────────┐
│ Your Contract (ExampleDeFiProtocol) │
│                                     │
│  __constructor(                     │
│    registry_address,                │
│    owner,                           │
│    name,                            │
│    description,                     │
│    categories                       │
│  )                                  │
│    │                                │
│    └──────────────┐                 │
│                   │                 │
│    RegistryInterfaceClient          │
│      .register_contract(...)        │
│                   │                 │
└───────────────────┼─────────────────┘
                    │
                    ▼
         ┌──────────────────┐
         │ Lumina Registry  │
         │                  │
         │ stores entry     │
         │ emits event      │
         └──────────────────┘
                    │
                    ▼
         ┌──────────────────┐
         │ Lumina Indexer   │
         │                  │
         │ discovers new    │
         │ contract         │
         │ starts indexing  │
         └──────────────────┘
```

## Building

From the repository root:

```bash
make build
# or
cargo build --target wasm32v1-none --release
```

The example wasm will be at:
```
target/wasm32v1-none/release/lumina_registry_registrant_example.wasm
```

## Testing

The example includes comprehensive integration tests:

```bash
cargo test -p lumina-registry-registrant-example
```

Tests cover:
- ✓ Self-registration during deployment
- ✓ Entry appears in active contracts list
- ✓ Entry appears in category listings
- ✓ Owner can update metadata
- ✓ Business logic works independently
- ✓ Multiple contracts can register
- ✓ Registration ownership is correct
- ✓ Deployment emits expected events

## Deploying

### Prerequisites

- [Stellar CLI](https://developers.stellar.org/docs/tools/stellar-cli)
- A deployed Lumina Registry (see `DEPLOY.md` in the repo root)
- A funded deployer identity

### Deploy Command

```bash
stellar contract deploy \
  --wasm target/wasm32v1-none/release/lumina_registry_registrant_example.wasm \
  --source your-deployer-key \
  --network testnet \
  -- \
  --registry_address CAYUDQPV3RKPM3EXDFGI3457FV677JLUCJ4OLKWGCUBPRIHYKXK3WFAZ \
  --owner your-deployer-key \
  --name "My Protocol" \
  --description "A DeFi protocol on Stellar" \
  --categories '["DeFi", "Payments"]'
```

Replace:
- `your-deployer-key` with your Stellar identity
- The registry address with your registry (or use the testnet one above)
- `name`, `description`, and `categories` with your protocol's details

### Available Categories

Choose from:
- `DeFi` - Decentralized finance protocols
- `Nft` - NFT marketplaces and collections
- `Gaming` - Gaming and metaverse projects
- `Identity` - Identity and authentication
- `Infrastructure` - Core infrastructure and tooling
- `Payments` - Payment processors and wallets
- `Oracle` - Oracle and data feed services
- `Dao` - Governance and DAO platforms
- `Other` - Projects that don't fit other categories

You can specify multiple categories. At least one is required.

## Verifying Registration

After deployment, verify your contract was registered:

```bash
# Check if the contract knows it's registered
stellar contract invoke \
  --id <your-contract-id> \
  --source your-key \
  --network testnet \
  -- is_registered

# Check the registry directly
stellar contract invoke \
  --id CAYUDQPV3RKPM3EXDFGI3457FV677JLUCJ4OLKWGCUBPRIHYKXK3WFAZ \
  --source your-key \
  --network testnet \
  -- get_active_contracts --offset 0 --limit 10
```

Your contract should appear in the active contracts list.

## Adapting for Your Project

To add registry self-registration to your own contract:

### 1. Add the dependency

In your `Cargo.toml`:

```toml
[dependencies]
soroban-sdk = "22.0.0"
lumina-registry-interface = { git = "https://github.com/Lumeeena/lumina-contracts", branch = "main" }
```

Or use a local path if you've cloned the repo:

```toml
lumina-registry-interface = { path = "../path/to/lumina-contracts/registry-interface" }
```

### 2. Import the client

```rust
use lumina_registry_interface::{Category, RegistryInterfaceClient};
```

### 3. Add registration to your constructor

```rust
#[contract]
pub struct YourContract;

#[contractimpl]
impl YourContract {
    pub fn __constructor(
        env: Env,
        registry_address: Address,
        owner: Address,
        name: String,
        description: String,
        categories: Vec<Category>,
    ) {
        // Your existing initialization...
        
        // Register with Lumina
        let self_address = env.current_contract_address();
        let registry = RegistryInterfaceClient::new(&env, &registry_address);
        registry.register_contract(
            &owner,
            &self_address,
            &name,
            &description,
            &categories,
        );
        
        // Store registry info for later
        env.storage().instance().set(&DataKey::Registry, &registry_address);
    }
}
```

### 4. (Optional) Add metadata update function

Allow your owner to keep the registration current:

```rust
pub fn update_registry_info(
    env: Env,
    owner: Address,
    new_name: String,
    new_description: String,
) -> Result<(), Error> {
    owner.require_auth();
    
    let registry_address: Address = env.storage()
        .instance()
        .get(&DataKey::Registry)
        .ok_or(Error::NotRegistered)?;
    
    let registry = RegistryInterfaceClient::new(&env, &registry_address);
    let self_address = env.current_contract_address();
    
    registry.update_metadata(&owner, &self_address, &new_name, &new_description);
    Ok(())
}
```

## Key Points

### Constructor Authorization

The deployer must authorize the `owner` address used in `register_contract`. During deployment, the deployer typically authorizes this automatically. In tests, use `env.mock_all_auths()`.

### Storage Considerations

Storing the registry address and owner allows your contract to:
- Update its own metadata later
- Deactivate/reactivate its registration
- Verify it's properly registered

### Error Handling

In production contracts, consider:
- Wrapping `register_contract` in a try/catch if registration is optional
- Emitting events when registration succeeds or fails
- Storing registration status for queries

### Categories

Choose categories that accurately describe your protocol:
- They affect discoverability in the Lumina frontend
- They influence which indexers might prioritize your events
- You can update them later via `set_categories`

## CI Integration

Add to your CI to ensure the example stays buildable:

```yaml
- name: Build example
  run: cargo build -p lumina-registry-registrant-example --target wasm32v1-none --release

- name: Test example
  run: cargo test -p lumina-registry-registrant-example
```

## Further Reading

- [Lumina Registry README](../../registry/README.md) - Full registry documentation
- [DEPLOY.md](../../DEPLOY.md) - How to deploy your own registry
- [RegistryInterface](../../registry-interface/src/lib.rs) - Typed cross-contract interface
- [Consumer Example](../registry-consumer/) - Example of reading from the registry

## License

MIT
