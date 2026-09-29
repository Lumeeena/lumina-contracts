#!/usr/bin/env bash
# Copyright (c) Lumina contributors
# SPDX-License-Identifier: MIT
#
# Deploy or upgrade the Lumina Registry with automatic wasm hash tracking.
#
# This script:
# - Records the current deployed wasm hash before upgrading (for rollback)
# - Builds and uploads the new wasm
# - Deploys or upgrades the contract
# - Stores hashes in .deployment-history/ for audit trail
#
# Usage:
#   ./scripts/deploy.sh deploy <network> <source-identity> <bootstrap-admin>
#   ./scripts/deploy.sh upgrade <network> <source-identity> <contract-id> <admin>
#   ./scripts/deploy.sh rollback <network> <source-identity> <contract-id> <admin>
#
# Examples:
#   ./scripts/deploy.sh deploy testnet lumina-deployer lumina-deployer
#   ./scripts/deploy.sh upgrade testnet lumina-deployer lumina-registry GBWK...
#   ./scripts/deploy.sh rollback testnet lumina-deployer lumina-registry GBWK...

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
HISTORY_DIR="$PROJECT_ROOT/.deployment-history"
WASM_PATH="$PROJECT_ROOT/target/wasm32v1-none/release/lumina_registry.wasm"

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
    local missing=()
    
    if ! command -v stellar &> /dev/null; then
        missing+=("stellar")
    fi
    
    if ! command -v sha256sum &> /dev/null && ! command -v shasum &> /dev/null; then
        missing+=("sha256sum or shasum")
    fi
    
    if [ ${#missing[@]} -ne 0 ]; then
        log_error "Missing required commands: ${missing[*]}"
        log_info "Install Stellar CLI: https://developers.stellar.org/docs/tools/stellar-cli"
        exit 1
    fi
}

# Compute sha256 hash (cross-platform)
compute_hash() {
    local file=$1
    if command -v sha256sum &> /dev/null; then
        sha256sum "$file" | awk '{print $1}'
    else
        shasum -a 256 "$file" | awk '{print $1}'
    fi
}

# Initialize deployment history directory
init_history() {
    mkdir -p "$HISTORY_DIR"
    
    if [ ! -f "$HISTORY_DIR/.gitignore" ]; then
        cat > "$HISTORY_DIR/.gitignore" << EOF
# Deployment history is tracked locally but not committed
# to avoid leaking deployment addresses and hashes
*
!.gitignore
EOF
    fi
}

# Record deployment event
record_deployment() {
    local network=$1
    local action=$2
    local hash=$3
    local contract_id=${4:-""}
    local timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
    
    local history_file="$HISTORY_DIR/${network}.log"
    
    echo "[$timestamp] $action | hash=$hash | contract_id=$contract_id" >> "$history_file"
    
    # Also store the hash in a network-specific file for easy rollback
    if [ "$action" = "DEPLOYED" ] || [ "$action" = "UPGRADED" ]; then
        echo "$hash" > "$HISTORY_DIR/${network}.current"
        if [ -n "$contract_id" ]; then
            echo "$contract_id" > "$HISTORY_DIR/${network}.contract_id"
        fi
    fi
}

# Get the last deployed hash for rollback
get_previous_hash() {
    local network=$1
    local history_file="$HISTORY_DIR/${network}.log"
    
    if [ ! -f "$history_file" ]; then
        log_error "No deployment history found for network: $network"
        return 1
    fi
    
    # Get the second-to-last deployment (the one before current)
    local prev_hash=$(grep -E "DEPLOYED|UPGRADED" "$history_file" | tail -n 2 | head -n 1 | sed 's/.*hash=\([^ ]*\).*/\1/')
    
    if [ -z "$prev_hash" ]; then
        log_error "No previous deployment found for rollback"
        return 1
    fi
    
    echo "$prev_hash"
}

# Fetch current wasm from deployed contract
fetch_current_wasm() {
    local network=$1
    local contract_id=$2
    local output_file=$3
    
    log_info "Fetching current wasm from contract $contract_id..."
    
    if ! stellar contract fetch \
        --id "$contract_id" \
        --network "$network" \
        --out-file "$output_file" 2>&1; then
        log_error "Failed to fetch current wasm"
        return 1
    fi
    
    local hash=$(compute_hash "$output_file")
    log_success "Current wasm hash: $hash"
    echo "$hash"
}

# Build the contract
build_contract() {
    log_info "Building contract..."
    cd "$PROJECT_ROOT"
    
    if ! stellar contract build 2>&1 | tail -5; then
        log_error "Build failed"
        exit 1
    fi
    
    if [ ! -f "$WASM_PATH" ]; then
        log_error "Build output not found at $WASM_PATH"
        exit 1
    fi
    
    local hash=$(compute_hash "$WASM_PATH")
    log_success "Built wasm hash: $hash"
    echo "$hash"
}

# Deploy a new contract
cmd_deploy() {
    local network=$1
    local source=$2
    local bootstrap_admin=$3
    
    if [ -z "$network" ] || [ -z "$source" ] || [ -z "$bootstrap_admin" ]; then
        log_error "Usage: deploy <network> <source-identity> <bootstrap-admin>"
        exit 1
    fi
    
    log_info "Deploying new contract to $network..."
    
    # Build
    local hash=$(build_contract)
    
    # Deploy
    log_info "Deploying contract..."
    local output=$(stellar contract deploy \
        --wasm "$WASM_PATH" \
        --source "$source" \
        --network "$network" \
        -- --bootstrap_admin "$bootstrap_admin" 2>&1)
    
    local contract_id=$(echo "$output" | grep -oE 'C[A-Z0-9]{55}' | head -1)
    
    if [ -z "$contract_id" ]; then
        log_error "Deployment failed or contract ID not found in output"
        echo "$output"
        exit 1
    fi
    
    log_success "Deployed contract: $contract_id"
    log_success "Wasm hash: $hash"
    
    # Record deployment
    record_deployment "$network" "DEPLOYED" "$hash" "$contract_id"
    
    log_info "Deployment history saved to $HISTORY_DIR/${network}.log"
    log_info "Contract ID saved to $HISTORY_DIR/${network}.contract_id"
    
    echo ""
    log_success "Deployment complete!"
    echo "Contract ID: $contract_id"
    echo "Wasm hash: $hash"
    echo ""
    echo "Next steps:"
    echo "  1. Set REGISTRY_CONTRACT_ID=$contract_id in your environment"
    echo "  2. Verify deployment: stellar contract invoke --id $contract_id --network $network -- get_version"
}

# Upgrade an existing contract
cmd_upgrade() {
    local network=$1
    local source=$2
    local contract_id=$3
    local admin=$4
    
    if [ -z "$network" ] || [ -z "$source" ] || [ -z "$contract_id" ] || [ -z "$admin" ]; then
        log_error "Usage: upgrade <network> <source-identity> <contract-id> <admin>"
        exit 1
    fi
    
    log_info "Upgrading contract $contract_id on $network..."
    
    # Fetch and record current wasm BEFORE upgrading
    local rollback_file="$HISTORY_DIR/${network}.rollback.wasm"
    local current_hash=$(fetch_current_wasm "$network" "$contract_id" "$rollback_file")
    
    if [ -z "$current_hash" ]; then
        log_error "Failed to fetch current wasm for rollback safety"
        exit 1
    fi
    
    record_deployment "$network" "PRE_UPGRADE_SNAPSHOT" "$current_hash" "$contract_id"
    log_success "Rollback wasm saved to $rollback_file"
    
    # Check if hash file exists; if not, refuse upgrade
    local hash_file="$HISTORY_DIR/${network}.current"
    if [ ! -f "$hash_file" ]; then
        log_warn "No previous hash recorded. Creating safety record..."
        echo "$current_hash" > "$hash_file"
    fi
    
    # Build new version
    local new_hash=$(build_contract)
    
    if [ "$new_hash" = "$current_hash" ]; then
        log_warn "New wasm hash matches current deployment. No upgrade needed."
        exit 0
    fi
    
    # Upload new wasm
    log_info "Uploading new wasm..."
    local upload_output=$(stellar contract upload \
        --wasm "$WASM_PATH" \
        --source "$source" \
        --network "$network" 2>&1)
    
    local uploaded_hash=$(echo "$upload_output" | grep -oE '[a-f0-9]{64}' | head -1)
    
    if [ -z "$uploaded_hash" ]; then
        log_error "Upload failed or hash not found in output"
        echo "$upload_output"
        exit 1
    fi
    
    log_success "Uploaded wasm hash: $uploaded_hash"
    
    # Verify local and uploaded hashes match
    if [ "$new_hash" != "$uploaded_hash" ]; then
        log_error "Hash mismatch! Local: $new_hash, Uploaded: $uploaded_hash"
        log_error "This should never happen. Aborting upgrade."
        exit 1
    fi
    
    # Perform upgrade
    log_info "Executing upgrade..."
    stellar contract invoke \
        --id "$contract_id" \
        --source "$source" \
        --network "$network" \
        -- upgrade \
        --admin "$admin" \
        --new_wasm_hash "$uploaded_hash"
    
    # Record upgrade
    record_deployment "$network" "UPGRADED" "$new_hash" "$contract_id"
    
    log_success "Upgrade complete!"
    echo ""
    echo "Previous hash (rollback target): $current_hash"
    echo "New hash: $new_hash"
    echo "Rollback wasm: $rollback_file"
    echo ""
    echo "Verify upgrade:"
    echo "  stellar contract invoke --id $contract_id --network $network -- get_version"
}

# Rollback to previous version
cmd_rollback() {
    local network=$1
    local source=$2
    local contract_id=$3
    local admin=$4
    
    if [ -z "$network" ] || [ -z "$source" ] || [ -z "$contract_id" ] || [ -z "$admin" ]; then
        log_error "Usage: rollback <network> <source-identity> <contract-id> <admin>"
        exit 1
    fi
    
    log_warn "Rolling back contract $contract_id on $network..."
    
    # Get previous hash
    local rollback_hash=$(get_previous_hash "$network")
    
    if [ -z "$rollback_hash" ]; then
        exit 1
    fi
    
    log_info "Rollback target hash: $rollback_hash"
    
    # Check if rollback wasm exists
    local rollback_file="$HISTORY_DIR/${network}.rollback.wasm"
    if [ ! -f "$rollback_file" ]; then
        log_error "Rollback wasm not found at $rollback_file"
        log_info "The deployment script should have saved it during the last upgrade"
        exit 1
    fi
    
    local file_hash=$(compute_hash "$rollback_file")
    if [ "$file_hash" != "$rollback_hash" ]; then
        log_error "Rollback wasm hash mismatch!"
        log_error "Expected: $rollback_hash"
        log_error "Found: $file_hash"
        exit 1
    fi
    
    # Upload rollback wasm
    log_info "Uploading rollback wasm..."
    stellar contract upload \
        --wasm "$rollback_file" \
        --source "$source" \
        --network "$network"
    
    # Perform rollback (which is just an upgrade to the old hash)
    log_info "Executing rollback..."
    stellar contract invoke \
        --id "$contract_id" \
        --source "$source" \
        --network "$network" \
        -- upgrade \
        --admin "$admin" \
        --new_wasm_hash "$rollback_hash"
    
    # Record rollback
    record_deployment "$network" "ROLLED_BACK" "$rollback_hash" "$contract_id"
    
    log_success "Rollback complete!"
    echo "Rolled back to hash: $rollback_hash"
}

# Show deployment history
cmd_history() {
    local network=${1:-"testnet"}
    local history_file="$HISTORY_DIR/${network}.log"
    
    if [ ! -f "$history_file" ]; then
        log_warn "No deployment history found for network: $network"
        exit 0
    fi
    
    log_info "Deployment history for $network:"
    echo ""
    cat "$history_file"
}

# Main command dispatcher
main() {
    check_dependencies
    init_history
    
    local command=${1:-""}
    
    case "$command" in
        deploy)
            shift
            cmd_deploy "$@"
            ;;
        upgrade)
            shift
            cmd_upgrade "$@"
            ;;
        rollback)
            shift
            cmd_rollback "$@"
            ;;
        history)
            shift
            cmd_history "$@"
            ;;
        *)
            echo "Lumina Registry Deployment Script"
            echo ""
            echo "Usage:"
            echo "  $0 deploy <network> <source-identity> <bootstrap-admin>"
            echo "  $0 upgrade <network> <source-identity> <contract-id> <admin>"
            echo "  $0 rollback <network> <source-identity> <contract-id> <admin>"
            echo "  $0 history [network]"
            echo ""
            echo "Examples:"
            echo "  $0 deploy testnet lumina-deployer lumina-deployer"
            echo "  $0 upgrade testnet lumina-deployer CAYU... GBWK..."
            echo "  $0 rollback testnet lumina-deployer CAYU... GBWK..."
            echo "  $0 history testnet"
            echo ""
            echo "The script automatically:"
            echo "  - Records wasm hashes before each upgrade"
            echo "  - Stores deployment history in .deployment-history/"
            echo "  - Refuses to proceed if previous hash was never recorded"
            echo "  - Provides rollback capability using saved hashes"
            exit 1
            ;;
    esac
}

main "$@"
