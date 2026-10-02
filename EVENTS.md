# Registry Events Reference

Events are the integration surface for downstream consumers, serving as the interface for both `lumina-backend`s indexer and `lumina-frontend`s registry history view.

## Downstream Consumers

- **Registry History (`lumina-frontend`)**: The frontend's history view rebuilds the per-contract event timeline by matching on the **first topic** (which must be the event name) and treating the **first data slot** as the subject ID (`contract_id`). Any events matching this shape will be attributed to the respective contract's history. Unknown topics will still be displayed as generic "Registry event" rows.
- **Indexer (`lumina-backend`)**: The backend indexer discovers contracts and listens to registry events to keep its database synchronized with the on-chain manifest. It specifically looks for registration, deactivation, and metadata/category changes to maintain an up-to-date registry graph.

## Events

| Topic | Payload Shape | When It Fires | Consumers | Example Test Reference |
|---|---|---|---|---|
| `proposal_proposed` | `(proposal_id: u32, proposer: Address, action: Symbol, data: T, description: String)` | When an admin proposes an action (e.g., slash, upgrade, change settings). The optional `description` is a human-readable rationale for the proposal, length-bounded by the contract. | | `propose_deactivate_requires_admin` |
| `proposal_proposed` (batch) | `(proposal_id: u32, proposer: Address, action: Symbol("batch"), action_count: u32)` | When an authenticated admin proposes a bounded batch. Read `get_proposal(proposal_id).action` for the ordered actions. Execution uses the existing per-action events and one `proposal_executed` event; a failure reverts the whole transaction. | | `batch_rotates_admins_in_order_under_one_proposal` |
| `proposal_approved` | `(proposal_id: u32, admin: Address, approvals_len: u32, threshold: u32)` | When an admin approves an existing proposal. | | `approve_proposal_records_approval_and_returns_total` |
| `proposal_ready` | `(proposal_id: u32, ready_at: u32, executable_from: u32)` | When a proposal receives enough approvals and enters the timelock. | | `proposal_enters_timelock_after_sufficient_approvals` |
| `proposal_executed` | `(proposal_id: u32, executed_at: u64, executor: Address)` | When a ready proposal is executed after the timelock elapses. | | `execute_proposal_applies_action` |
| `contract_deactivated` | `(contract_id: Address, caller: Address)` or `(contract_id: Address, Symbol("governance"))` | When a contract is deactivated by its owner or governance. | Indexer, History | `deactivate_requires_contract_owner` |
| `contract_deregistered` | `(contract_id: Address, owner: Address)` | When a deactivated and unstaked contract is fully deregistered. | Indexer, History | `deregister_removes_every_index_reference...` |
| `contract_registered` | `(contract_id: Address, owner: Address, name: String, categories: Vec<String>)` | When a new contract is registered to the manifest. | Indexer, History | `registration_recordsits_categories` |
| `categories_updated` | `(contract_id: Address, owner: Address, categories: Vec<String>)` | When a contract's categories are updated by its owner. | Indexer, History | `set_categories_moves_a_registration_between_categories` |
| `tags_updated` | `(contract_id: Address, owner: Address, tags_len: u32)` | When a contract's tags are updated by its owner. | History | `tags_are_updated_and_returned` |
| `metadata_updated` | `(contract_id: Address, owner: Address, name: String)` | When the contract's metadata (name) is updated. | Indexer, History | `update_metadata_succeeds_with_real_owner_signature` |
| `ownership_transfer_proposed` | `(contract_id: Address, current_owner: Address, new_owner: Address)` | When an owner proposes a two-step ownership transfer. | History | `propose_and_accept_ownership_transfer_succeeds_with_real_signatures` |
| `ownership_transfer_canceled` | `(contract_id: Address, owner: Address)` | When an owner cancels a pending ownership transfer. | History | `cancel_ownership_transfer_removes_pending_and_blocks_accept` |
| `ownership_transferred`| `(contract_id: Address, previous_owner: Address, new_owner: Address)` | When the contract's ownership is transferred to a new address. | History | `ownership_transfer_preserves_stake_and_verification` |
| `stake_deposited` | `(contract_id: Address, owner: Address, amount: i128, total_staked: i128)` | When the owner deposits tokens to top up their stake. | History | `stake_tops_up_an_existing_stake` |
| `stake_withdrawn` | `(contract_id: Address, owner: Address, total_staked: i128)` | When the owner withdraws their staked tokens after deactivation. | History | `withdraw_returns_the_full_stake_once_the_owner_has_deactivated` |
| `stake_slashed` | `(contract_id: Address, amount: i128, reason: String, treasury: Address, treasury_amount: i128, staker_pool_amount: i128)` | When governance slashes a contract's stake for a violation. The amount is split between the treasury and the staker reward pool according to the governance-set slash split. | History | `slash_splits_between_treasury_and_staker_pool` |
| `slash_split_configured` | `(treasury_bps: u32, staker_bps: u32)` | When governance changes the split of slashed funds between the treasury and the staker reward pool. | History | `slash_split_can_be_changed_by_treasury_bps` |
| `reward_claimed` | `(staker: Address, amount: i128, pool_remaining: i128)` | When a staker claims their share of the staker reward pool. | History | `staker_can_claim_a_share_of_a_slash` |
| `verification_set` | `(contract_id: Address, verified: bool)` | When governance grants or revokes verified status for a contract. | History | `governance_can_attest_and_later_revoke_verification` |
| `category_pruned` | `(category: String, removed: u32)` | When dead references in a category's index are cleaned up. | | `prune_category_drops_dead_references_and_is_safe_to_repeat` |
| all_contracts_pruned` | `(removed: u32,)` | When dead references in the global index are cleaned up. | | `contract_count_is_live_and_total_registered_is_lifetime` |
| `registry_upgraded` | `(new_wasm_hash: BytesN<32>, version: u32)` | When the registry contract's WASM is upgraded. | | `upgrade_carries_admin_across_swap` |
| `admin_added` | `(new_admin: Address,)` | When a new governance admin is added via executed proposal. | | `propose_add_admin_adds_a_new_admin` |
| `admin_removed` | `(admin_to_remove: Address,)` | When a governance admin is removed via executed proposal. | | `propose_remove_admin_removes_the_admin` |
| `threshold_changed` | `(new_threshold: u32,)` | When the multisig approval threshold is changed. | | `propose_change_threshold_changes_the_threshold` |
| `staking_configured` | `(token_id: Address, treasury: Address, decimals: u32)` | When governance configures the staking token and treasury. | | `configure_staking_records_token_and_treasury` |
| `allowlist_mode_changed` | `(enabled: bool,)` | When governance enables or disables the owner allowlist. | | `allowlist_mode_can_be_toggled` |
| `owner_allowlisted` | `(owner: Address, allowed: bool)` | When governance adds or removes an owner from the allowlist. | | `owner_can_be_added_to_allowlist` |
| `registration_rate_limit_changed` | `(limit: u32, window: u32)` | When governance updates the rate limit parameters. | | `registration_rate_limit_can_be_changed` |
| `registration_fee_set` | `(fee: i128,)` | When governance sets a flat fee for new registrations. | | `registration_fee_can_be_set` |

## Slash Economics

When governance executes a `slash` proposal, the slashed amount is divided into two parts according to the governance-set slash split (`SlashSplit`, in basis points):

- **Treasury share** (`treasury_bps`) is transferred to the configured treasury address immediately, as before.
- **Staker reward pool** (`staker_bps`) is held in the contract and distributed to honest stakers via a claim path.

The two shares always sum to 10,000 basis points. The default is `100%`(treasury_bps = 10000`, `staker_bps = 0`), which reproduces the historical behaviour exactly. Governance changes the split through `propose_set_slash_split`, which emits `slash_split_configured`.

### Why a claim path and not a push

The contract cannot efficiently push a share to each honest staker at slash time. The staker set is unbounded and grows with the registry, so a push would make a single slash proposal's execution cost scale linearly with the number of stakers — the governance transaction would eventually exceed the ledger's resource limits and become executable by nobody. A claim path moves the cost of distribution to the claimant, so a slash costs the same whether there are ten stakers or ten thousand.

The pool is funded on execution and drawn down by claims. A staker's share is proportional to the stake they held on other registrations at the moment of the slash, so the people who judged correctly are rewarded. Claiming is per-staker and idempotent: claiming twice claims nothing the second time.
