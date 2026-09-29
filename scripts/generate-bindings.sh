#!/usr/bin/env bash
# Copyright (c) Lumina contributors
# SPDX-License-Identifier: MIT
#
# Generate TypeScript bindings for the Lumina Registry contract.
#
# This script generates type-safe TypeScript bindings from the registry wasm,
# eliminating the need for hand-written clients and preventing silent breakage
# when the contract interface changes.
#
# Usage:
#   ./scripts/generate-bindings.sh [output-dir]
#
# Examples:
#   ./scripts/generate-bindings.sh                    # outputs to ./bindings/typescript/
#   ./scripts/generate-bindings.sh ../lumina-frontend/src/registry-client

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WASM_PATH="$PROJECT_ROOT/target/wasm32v1-none/release/lumina_registry.wasm"
DEFAULT_OUTPUT_DIR="$PROJECT_ROOT/bindings/typescript"
OUTPUT_DIR="${1:-$DEFAULT_OUTPUT_DIR}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Logging helpers
log_info() {
    echo -e "${BLUE}[INFO]${NC} $*"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*"
}

# Check required commands
check_dependencies() {
    if ! command -v stellar &> /dev/null; then
        log_error "stellar CLI not found"
        log_info "Install: https://developers.stellar.org/docs/tools/stellar-cli"
        exit 1
    fi
    
    # Check if stellar CLI supports bindings typescript
    if ! stellar contract bindings typescript --help &> /dev/null; then
        log_error "stellar CLI doesn't support 'contract bindings typescript'"
        log_info "Update stellar CLI to the latest version"
        exit 1
    fi
}

# Build the contract if needed
ensure_wasm_exists() {
    if [ ! -f "$WASM_PATH" ]; then
        log_warn "Registry wasm not found, building..."
        cd "$PROJECT_ROOT"
        if ! stellar contract build 2>&1 | tail -5; then
            log_error "Build failed"
            exit 1
        fi
    fi
    
    if [ ! -f "$WASM_PATH" ]; then
        log_error "Build completed but wasm not found at $WASM_PATH"
        exit 1
    fi
    
    log_success "Using wasm: $WASM_PATH"
}

# Generate TypeScript bindings
generate_bindings() {
    log_info "Generating TypeScript bindings..."
    
    mkdir -p "$OUTPUT_DIR"
    
    # Generate bindings from the wasm
    if ! stellar contract bindings typescript \
        --wasm "$WASM_PATH" \
        --output-dir "$OUTPUT_DIR" \
        --overwrite 2>&1; then
        log_error "Failed to generate bindings"
        exit 1
    fi
    
    log_success "Generated TypeScript bindings to: $OUTPUT_DIR"
}

# Create a package.json if one doesn't exist
create_package_json() {
    local package_json="$OUTPUT_DIR/package.json"
    
    if [ -f "$package_json" ]; then
        log_info "package.json already exists, skipping creation"
        return
    fi
    
    log_info "Creating package.json..."
    
    cat > "$package_json" << 'EOF'
{
  "name": "@lumina/registry-client",
  "version": "0.1.0",
  "description": "TypeScript client for the Lumina Registry contract",
  "main": "index.ts",
  "types": "index.ts",
  "scripts": {
    "build": "tsc",
    "regenerate": "../../scripts/generate-bindings.sh"
  },
  "keywords": [
    "lumina",
    "stellar",
    "soroban",
    "registry"
  ],
  "license": "MIT",
  "peerDependencies": {
    "@stellar/stellar-sdk": "^12.0.0"
  },
  "devDependencies": {
    "@stellar/stellar-sdk": "^12.0.0",
    "typescript": "^5.0.0"
  }
}
EOF
    
    log_success "Created package.json"
}

# Create README for the bindings
create_readme() {
    local readme="$OUTPUT_DIR/README.md"
    
    if [ -f "$readme" ]; then
        log_info "README.md already exists, skipping creation"
        return
    fi
    
    log_info "Creating README.md..."
    
    cat > "$readme" << 'EOF'
# Lumina Registry TypeScript Client

Auto-generated TypeScript bindings for the Lumina Registry smart contract.

## ⚠️ Do Not Edit Manually

This directory contains **generated code**. Any manual edits will be overwritten the next time bindings are regenerated.

To update these bindings:

```bash
cd lumina-contracts
./scripts/generate-bindings.sh
```

Or from this directory:

```bash
npm run regenerate
```

## Usage

```typescript
import { Contract as LuminaRegistry } from '@lumina/registry-client';
import { SorobanRpc, TransactionBuilder } from '@stellar/stellar-sdk';

const rpc = new SorobanRpc.Server('https://soroban-testnet.stellar.org');
const registry = new LuminaRegistry({
  contractId: 'CAYUDQPV3RKPM3EXDFGI3457FV677JLUCJ4OLKWGCUBPRIHYKXK3WFAZ',
  rpc,
});

// Read-only calls (simulation)
const activeContracts = await registry.get_active_contracts({
  offset: 0,
  limit: 10,
});

// Write calls (require signing)
const tx = await registry.register_contract({
  owner: ownerAddress,
  contract_id: contractAddress,
  name: 'My Protocol',
  description: 'A DeFi protocol on Stellar',
  categories: ['DeFi', 'Payments'],
});
```

## Type Safety

These bindings provide full TypeScript type checking:

- Function signatures match the deployed contract exactly
- Enum values are typed (e.g., `Category.DeFi`)
- Struct fields are validated at compile time
- Return types are inferred automatically

## When to Regenerate

Regenerate bindings whenever:

- The registry contract is upgraded with interface changes
- A function signature changes (new parameters, renamed fields)
- New functions are added to the contract
- Enum variants or struct definitions change

## Integration

### In lumina-frontend

```typescript
// Old hand-written client (prone to silent breakage)
import { Contract, nativeToScVal } from '@stellar/stellar-sdk';

const result = await contract.call('register_contract', 
  nativeToScVal(owner, { type: 'address' }),
  nativeToScVal(contractId, { type: 'address' }),
  nativeToScVal(name, { type: 'string' }),
  // Oops, forgot the new 'categories' parameter!
);

// New generated client (compile-time safety)
import { Contract as LuminaRegistry } from '@lumina/registry-client';

const result = await registry.register_contract({
  owner,
  contract_id: contractId,
  name,
  description, // TypeScript error if missing
  categories,  // TypeScript error if missing or wrong type
});
```

### In lumina-backend

The indexer's registry discovery (`indexer/src/registry.ts`) can use these bindings to ensure it reads the correct contract structure:

```typescript
import { Contract as LuminaRegistry } from '@lumina/registry-client';
import type { ContractEntry } from '@lumina/registry-client';

const registry = new LuminaRegistry({ contractId, rpc });
const entries: ContractEntry[] = await registry.get_active_contracts({
  offset: 0,
  limit: 50,
});

// TypeScript knows the exact shape of ContractEntry
entries.forEach(entry => {
  console.log(entry.contract_id);  // ✓ Typed
  console.log(entry.owner);        // ✓ Typed
  console.log(entry.active);       // ✓ Typed
  console.log(entry.made_up_field); // ✗ TypeScript error
});
```

## Release Process

When releasing a new registry version:

1. Update the registry contract
2. Regenerate bindings: `./scripts/generate-bindings.sh`
3. Commit the generated files
4. Bump the version in package.json
5. Publish to npm (if desired) or consume locally

## CI Integration

Add to your CI pipeline to ensure bindings stay fresh:

```yaml
- name: Verify bindings are up-to-date
  run: |
    ./scripts/generate-bindings.sh
    git diff --exit-code bindings/
```

This fails CI if someone updates the contract without regenerating bindings.
EOF
    
    log_success "Created README.md"
}

# Create .gitignore for the bindings directory
create_gitignore() {
    local gitignore="$OUTPUT_DIR/.gitignore"
    
    log_info "Creating .gitignore..."
    
    cat > "$gitignore" << 'EOF'
# Dependencies
node_modules/
package-lock.json
yarn.lock

# Build output
dist/
*.js
*.js.map
*.d.ts

# Keep the generated TypeScript source
!index.ts
EOF
    
    log_success "Created .gitignore"
}

# Main execution
main() {
    log_info "Lumina Registry TypeScript Bindings Generator"
    echo ""
    
    check_dependencies
    ensure_wasm_exists
    generate_bindings
    create_package_json
    create_readme
    create_gitignore
    
    echo ""
    log_success "✓ Bindings generation complete!"
    echo ""
    echo "Output directory: $OUTPUT_DIR"
    echo ""
    echo "Next steps:"
    echo "  1. Review the generated bindings in $OUTPUT_DIR"
    echo "  2. Integrate into lumina-frontend and lumina-backend"
    echo "  3. Replace hand-written Contract/nativeToScVal calls with the typed client"
    echo "  4. Add binding regeneration to your release checklist"
    echo ""
    echo "To regenerate after contract changes:"
    echo "  ./scripts/generate-bindings.sh"
}

main
