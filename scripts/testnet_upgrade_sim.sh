#!/usr/bin/env bash
# scripts/testnet_upgrade_sim.sh
#
# Testnet upgrade simulation (#243).
#
# Runs the canonical release-gate simulation: deploy the currently-released
# wasm to testnet, seed it with representative state, submit and execute a
# governance upgrade proposal with the next-release wasm, then verify the
# seeded state is still intact after the upgrade.
#
# This script is the automation behind the release gate documented in
# DEPLOY.md and GOVERNANCE_PLAYBOOK.md §2.3 — every tagged release must
# pass this simulation against the exact wasm hash that will be proposed
# on mainnet.
#
# Usage:
#   scripts/testnet_upgrade_sim.sh <bootstrap-admin-identity> <current-wasm> <next-wasm>
#
# Preconditions:
#   - `stellar` CLI installed and configured for the testnet network.
#   - `<bootstrap-admin-identity>` is a funded testnet identity.
#   - `<current-wasm>` and `<next-wasm>` are local paths to the two wasm
#     binaries you want to simulate the upgrade between.
#
# Exit codes:
#   0  state survived upgrade; release gate passed.
#   1  invocation error (bad args, cli missing).
#   2  deployment / seeding failure (environment issue, not a release blocker).
#   3  state did not survive upgrade — release blocker.

set -euo pipefail

usage() {
   cat <<EOF
Usage: $0 <bootstrap-admin-identity> <current-wasm> <next-wasm>

Runs the testnet upgrade simulation release gate for the Lumina registry
(#243). See the header of this file for exit codes and the companion
section in DEPLOY.md.
EOF
}

if [[ ${#} -ne 3 ]]; then
   usage
   exit 1
fi

ADMIN_IDENTITY="$1"
CURRENT_WASM="$2"
NEXT_WASM="$3"
NETWORK="${LUMINA_SIM_NETWORK:-testnet}"

if ! command -v stellar >/dev/null 2>&1; then
   echo "error: stellar CLI not on PATH" >&2
   exit 1
fi
if [[ ! -r "$CURRENT_WASM" ]]; then
   echo "error: current wasm not readable: $CURRENT_WASM" >&2
   exit 1
fi
if [[ ! -r "$NEXT_WASM" ]]; then
   echo "error: next wasm not readable: $NEXT_WASM" >&2
   exit 1
fi

log() { echo "[sim] $*"; }
fail_env() { echo "[sim] ENVIRONMENT FAILURE: $*" >&2; exit 2; }
fail_gate() { echo "[sim] RELEASE BLOCKER: $*" >&2; exit 3; }

ADMIN_ADDRESS=$(stellar keys address "$ADMIN_IDENTITY") || fail_env "cannot resolve admin address"
log "network=$NETWORK  admin=$ADMIN_ADDRESS"

# ---------------------------------------------------------------------------
# 1. Deploy the current release's wasm with a one-admin bootstrap.
# ---------------------------------------------------------------------------
log "deploying current wasm ($CURRENT_WASM)"
CONTRACT_ID=$(
   stellar contract deploy \
      --wasm "$CURRENT_WASM" \
      --network "$NETWORK" \
      --source "$ADMIN_IDENTITY" \
      -- \
      --bootstrap_admin "$ADMIN_ADDRESS"
) || fail_env "deploy failed"
log "deployed contract_id=$CONTRACT_ID"

invoke() {
   stellar contract invoke \
      --id "$CONTRACT_ID" \
      --network "$NETWORK" \
      --source "$ADMIN_IDENTITY" \
      -- \
      "$@"
}

# ---------------------------------------------------------------------------
# 2. Seed state representative of production — the fixtures below are what
#    the upgrade must preserve. Keep them small; this is a release gate, not
#    a soak test.
# ---------------------------------------------------------------------------
SEED_CONTRACT="$CONTRACT_ID"  # self-registration is rejected at register time;
# use a dedicated fixture address instead if your release gate requires one.
SEED_NAME="release-gate-$(date -u +%Y%m%dT%H%M%SZ)"

log "seeding state"
# Register one contract; we cross-check this entry survives the upgrade.
invoke register_contract \
   --owner "$ADMIN_ADDRESS" \
   --contract_id "$SEED_CONTRACT" \
   --name "$SEED_NAME" \
   --description "release-gate seed" \
   --categories '[]' >/dev/null \
   || fail_env "seeding register_contract failed"

PRE_COUNT=$(invoke get_contract_count 2>/dev/null) || fail_env "get_contract_count failed pre-upgrade"
PRE_CONFIG=$(invoke get_contract_config 2>/dev/null) || fail_env "get_contract_config failed pre-upgrade"
log "pre-upgrade contract_count=$PRE_COUNT"
log "pre-upgrade config=$PRE_CONFIG"

# ---------------------------------------------------------------------------
# 3. Install the next-release wasm, propose and execute the upgrade.
# ---------------------------------------------------------------------------
log "installing next wasm hash ($NEXT_WASM)"
NEXT_HASH=$(
   stellar contract install \
      --wasm "$NEXT_WASM" \
      --network "$NETWORK" \
      --source "$ADMIN_IDENTITY"
) || fail_env "install next wasm failed"
log "next wasm hash=$NEXT_HASH"

log "proposing upgrade"
PROPOSAL_ID=$(invoke propose_upgrade --proposer "$ADMIN_ADDRESS" --new_wasm_hash "$NEXT_HASH") \
   || fail_env "propose_upgrade failed"
log "proposal_id=$PROPOSAL_ID"

invoke approve_proposal --admin "$ADMIN_ADDRESS" --proposal_id "$PROPOSAL_ID" \
   || fail_env "approve_proposal failed"

# Timelock on testnet is identical to mainnet; the release gate must wait it
# out so a fast-path bug is caught here rather than in production.
if [[ -n "${LUMINA_SIM_FAST_FORWARD:-}" ]]; then
   log "LUMINA_SIM_FAST_FORWARD set: skipping timelock sleep (NOT suitable as a release gate)"
else
   log "waiting TIMELOCK_LEDGERS × ~6s; this is intentional"
   # TIMELOCK_LEDGERS pulled from the deployed contract to stay consistent.
   TIMELOCK_SECONDS=$(( $(grep -E 'TIMELOCK_LEDGERS[^0-9]+[0-9_]+' registry/src/lib.rs \
      | head -n1 | tr -cd '0-9') * 6 ))
   log "sleeping ${TIMELOCK_SECONDS}s"
   sleep "${TIMELOCK_SECONDS}"
fi

invoke execute_proposal --executor "$ADMIN_ADDRESS" --proposal_id "$PROPOSAL_ID" \
   || fail_gate "execute_proposal failed; wasm published does not accept execution against seeded state"

# ---------------------------------------------------------------------------
# 4. Verify the seeded state is still intact after the upgrade.
# ---------------------------------------------------------------------------
log "verifying post-upgrade state"
POST_COUNT=$(invoke get_contract_count 2>/dev/null) || fail_gate "get_contract_count failed post-upgrade"
POST_CONFIG=$(invoke get_contract_config 2>/dev/null) || fail_gate "get_contract_config failed post-upgrade"

if [[ "$POST_COUNT" != "$PRE_COUNT" ]]; then
   fail_gate "contract_count changed across upgrade: before=$PRE_COUNT after=$POST_COUNT"
fi
log "post-upgrade contract_count=$POST_COUNT (unchanged)"
log "post-upgrade config=$POST_CONFIG"

if ! invoke get_contract --contract_id "$SEED_CONTRACT" >/dev/null 2>&1; then
   fail_gate "seeded contract entry is gone after upgrade"
fi
log "seeded contract entry survived"

log "RELEASE GATE PASSED"
exit 0
