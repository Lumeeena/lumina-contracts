# Lumina Contracts Scripts

This directory contains automation scripts for deploying and maintaining the Lumina Registry.

## Scripts

- **[deploy.sh](#deploysh)** - Automated deployment with wasm hash tracking and rollback capability
- **[generate-bindings.sh](#generate-bindingssh)** - Generate TypeScript bindings from the registry contract

---

## deploy.sh

Automated deployment script for the Lumina Registry that ensures safe upgrades by automatically tracking wasm hashes.

### Features

- **Automatic hash tracking**: Records current wasm hash before every upgrade
- **Rollback safety**: Saves rollback wasm automatically for quick recovery
- **Deployment history**: Maintains an audit log of all deployments per network
- **Pre-flight checks**: Validates hashes match before upgrading
- **Refuse-to-proceed guarantee**: Won't upgrade without recording the previous hash

### Usage

#### Deploy a new contract

```bash
./scripts/deploy.sh deploy <network> <source-identity> <bootstrap-admin>
```

Example:
```bash
./scripts/deploy.sh deploy testnet lumina-deployer lumina-deployer
```

#### Upgrade an existing contract

```bash
./scripts/deploy.sh upgrade <network> <source-identity> <contract-id> <admin>
```

Example:
```bash
./scripts/deploy.sh upgrade testnet lumina-deployer CAYUDQPV3RKPM3EXDFGI3457FV677JLUCJ4OLKWGCUBPRIHYKXK3WFAZ GBWKFFXZ5CJESIHP2EOID5IOXMF472RO5XOJ36X475D5LJGI3AF5R5KY
```

The script will:
1. Fetch the currently deployed wasm and compute its hash
2. Save the wasm to `.deployment-history/<network>.rollback.wasm`
3. Build the new version
4. Upload and upgrade to the new version
5. Record all hashes in the deployment history

#### Rollback to previous version

```bash
./scripts/deploy.sh rollback <network> <source-identity> <contract-id> <admin>
```

Example:
```bash
./scripts/deploy.sh rollback testnet lumina-deployer CAYUDQPV3RKPM3EXDFGI3457FV677JLUCJ4OLKWGCUBPRIHYKXK3WFAZ GBWKFFXZ5CJESIHP2EOID5IOXMF472RO5XOJ36X475D5LJGI3AF5R5KY
```

The script will:
1. Look up the previous wasm hash from deployment history
2. Verify the rollback wasm file exists and matches the expected hash
3. Upload and upgrade to the previous version

#### View deployment history

```bash
./scripts/deploy.sh history [network]
```

Example:
```bash
./scripts/deploy.sh history testnet
```

Shows all deployment events for the specified network (defaults to testnet).

### Deployment History

The script maintains deployment history in `.deployment-history/` (gitignored):

- `<network>.log` - Full deployment log with timestamps, actions, and hashes
- `<network>.current` - Current deployed wasm hash
- `<network>.contract_id` - Current contract ID
- `<network>.rollback.wasm` - Last deployed wasm (for rollback)

### Safety Features

1. **Pre-upgrade snapshot**: Always fetches and saves current wasm before upgrading
2. **Hash verification**: Compares local build hash with uploaded hash
3. **History tracking**: Records every deployment event with timestamp
4. **Rollback validation**: Verifies rollback wasm hash before executing
5. **No-skip guarantee**: Refuses to upgrade if previous hash wasn't recorded

### Requirements

- [Stellar CLI](https://developers.stellar.org/docs/tools/stellar-cli) (`stellar` command)
- `sha256sum` (Linux) or `shasum` (macOS)
- Bash 4.0 or later

### Error Handling

The script exits immediately on any error (set -e) and provides clear error messages:

- **Missing dependencies**: Checks for required commands before proceeding
- **Build failures**: Exits if contract build fails
- **Fetch failures**: Won't upgrade if current wasm can't be fetched
- **Hash mismatches**: Aborts if local and uploaded hashes don't match
- **Missing rollback wasm**: Won't rollback if the previous wasm file is missing

### Integration with CI/CD

The script is designed to be CI/CD friendly:

- Exit codes: 0 for success, non-zero for failure
- No interactive prompts
- Clear structured output
- Idempotent operations (safe to re-run)

Example GitHub Actions workflow:

```yaml
- name: Deploy to testnet
  run: |
    ./scripts/deploy.sh upgrade testnet ci-deployer $CONTRACT_ID $ADMIN_KEY
  env:
    CONTRACT_ID: ${{ secrets.TESTNET_CONTRACT_ID }}
    ADMIN_KEY: ${{ secrets.TESTNET_ADMIN }}
```

### Troubleshooting

**"No previous deployment found for rollback"**
- The deployment history is empty or doesn't contain enough entries
- Solution: You need at least 2 deployments to rollback

**"Rollback wasm not found"**
- The `.deployment-history/<network>.rollback.wasm` file is missing
- Solution: Re-run the upgrade to generate a new rollback snapshot

**"Hash mismatch! Local: ..., Uploaded: ..."**
- The local build produced a different hash than what was uploaded
- This should never happen and indicates a serious issue
- Solution: Investigate why the hashes differ (rebuild, check target, etc.)

**"Failed to fetch current wasm"**
- The contract ID is invalid or network is unreachable
- Solution: Verify the contract ID and network connectivity



---

## generate-bindings.sh

Generates type-safe TypeScript bindings from the registry wasm to eliminate hand-written clients and prevent runtime breakage.

### Features

- **Type safety**: Full TypeScript types for all contract functions
- **Auto-generated**: Single source of truth from the compiled contract
- **Compile-time validation**: Interface changes become TypeScript errors
- **Ready to use**: Complete client with types, enums, and structs
- **Documentation**: Includes package.json, README, and usage examples

### Usage

#### Generate bindings to default location

```bash
./scripts/generate-bindings.sh
```

Outputs to `bindings/typescript/`

#### Generate bindings to custom location

```bash
./scripts/generate-bindings.sh ../lumina-frontend/src/registry-client
```

Useful for generating directly into consuming projects.

### What Gets Generated

The script creates:

- **index.ts** - Complete TypeScript client with all types
- **package.json** - npm package configuration
- **README.md** - Usage documentation and integration guide
- **.gitignore** - Git ignore rules for the bindings directory

### Integration Example

Before (hand-written, prone to breakage):

```typescript
import { Contract, nativeToScVal } from '@stellar/stellar-sdk';

const contract = new Contract(contractId);
const result = await contract.call('register_contract',
  nativeToScVal(owner, { type: 'address' }),
  nativeToScVal(contractId, { type: 'address' }),
  nativeToScVal(name, { type: 'string' }),
  // Missing 'categories' parameter - runtime error!
);
```

After (generated, type-safe):

```typescript
import { Contract as LuminaRegistry } from '@lumina/registry-client';

const registry = new LuminaRegistry({ contractId, rpc });
const result = await registry.register_contract({
  owner,
  contract_id: contractId,
  name,
  description,
  categories,  // TypeScript error if missing!
});
```

### When to Regenerate

Regenerate bindings whenever:

- The registry contract is deployed/upgraded
- Function signatures change (new parameters, renamed fields)
- New functions are added
- Enum variants or struct definitions change
- Before releasing a new version of lumina-frontend or lumina-backend

### CI Integration

Add to CI to ensure bindings stay current:

```yaml
- name: Generate bindings
  run: ./scripts/generate-bindings.sh

- name: Verify bindings are committed
  run: |
    git diff --exit-code bindings/
    # Fails if bindings changed (meaning they were out of date)
```

Or check bindings are up-to-date:

```yaml
- name: Check bindings freshness
  run: |
    ./scripts/generate-bindings.sh
    if ! git diff --quiet bindings/; then
      echo "❌ TypeScript bindings are out of date"
      echo "Run './scripts/generate-bindings.sh' and commit the changes"
      exit 1
    fi
```

### Consuming in Projects

#### lumina-frontend

```bash
cd lumina-contracts
./scripts/generate-bindings.sh ../lumina-frontend/src/registry-client
```

Then in your frontend code:

```typescript
import { Contract as LuminaRegistry } from '@/registry-client';

const registry = new LuminaRegistry({
  contractId: process.env.NEXT_PUBLIC_REGISTRY_CONTRACT_ID!,
  rpc,
});

const profiles = await registry.get_active_profiles({ offset: 0, limit: 10 });
```

#### lumina-backend

```bash
cd lumina-contracts
./scripts/generate-bindings.sh ../lumina-backend/src/registry-client
```

Then in your indexer:

```typescript
import { Contract as LuminaRegistry } from './registry-client';
import type { ContractEntry } from './registry-client';

const entries: ContractEntry[] = await registry.get_active_contracts({
  offset,
  limit: 50,
});

// Full type safety on all fields
const contractIds = entries
  .filter(e => e.active)
  .map(e => e.contract_id);
```

### Requirements

- [Stellar CLI](https://developers.stellar.org/docs/tools/stellar-cli) with `contract bindings typescript` support
- The registry wasm must be built (script will build if missing)

### Troubleshooting

**"stellar CLI doesn't support 'contract bindings typescript'"**
- Update to the latest Stellar CLI version
- Solution: `stellar self update` or reinstall from https://developers.stellar.org/docs/tools/stellar-cli

**"Registry wasm not found, building..."**
- The script automatically builds if the wasm is missing
- If build fails, check that your Rust toolchain is properly configured

**"Failed to generate bindings"**
- Ensure the wasm file is valid and not corrupted
- Try rebuilding: `stellar contract build`
- Check that your Stellar CLI is up-to-date
