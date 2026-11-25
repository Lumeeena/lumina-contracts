# What to do when your project is slashed

This guide is for a project whose registration has just been slashed by
governance. It is a complement to `SLASH_RESPONSE_EXAMPLES.md`, which covers
the on-chain mechanics; here we cover **what a slash means, what your options
are, and how to recover standing**.

## What a slash is

A slash is a governance action that reduces a project's stake and lowers its
reputation score. It is recorded on-chain as a `SlashRecord` (see
`EVENTS.md`) and is **permanent and public**. It is signed by the governance
admins who reached the approval threshold, not by a single actor, so a slash
is a joint statement by the registry's multi-sig that something was wrong at
the time it was issued.

What a slash is **not**:

- It is not a deregistration. Your contract stays indexed. Reads keep working.
- It is not a ban. You remain eligible to top up your stake, re-attest, and
  request reinstatement of verified status.
- It is not an audit finding. Governance slashes on evidence it finds
  sufficient; it does not necessarily imply that an independent audit would
  reach the same conclusion.

A slash records a `reason` string and amount. Your project's slash history is
retrievable with `get_slashes(contract_id)`.

## Deciding how to respond

The first question is whether governance's reason is factually correct. The
response you file on-chain is permanent, so slow down before writing it.

Three broad cases:

1. **The slash describes a real issue.** Acknowledge it, describe what you
   have done or will do, and commit to a timeline. A short, specific
   response of this kind tends to help reputation recover.
2. **The slash describes something that happened but misreads it.** State
   the facts, point to evidence (transaction hashes, commit links, audit
   reports), and ask governance to review. Responses are not contests —
   they sit alongside the slash, they do not replace it.
3. **The slash is, in your view, mistaken.** File a response that says so,
   with evidence, and open a governance proposal (or ask an admin to) to
   reduce or reverse the slash. Governance can issue follow-up actions;
   it cannot delete the original record.

Do **not** file a response that is only an objection. The response you file
is read alongside the slash whenever anyone queries your reputation, and
future users will weigh both.

## Responding on-chain

See `SLASH_RESPONSE_EXAMPLES.md` for the exact calls. In short:

1. Fetch your slash list with `get_slashes(contract_id)` and find the
   `slash_index` for the record you want to answer.
2. Call `respond_to_slash(owner, contract_id, slash_index, response)` as
   the registered owner.
3. Verify with `get_slashes` that the `response` field is now set.

Constraints to know before you start:

- Only the registered owner can respond.
- Each slash accepts **one** response. There is no edit; draft offline first.
- Responses are plain text, bounded by the contract's string limits
  (`SlashResponse.response`). Keep it factual and short.

## Recovering standing

A slash lowers reputation but does not make it unrecoverable. The levers
available to your project:

- **Top up your stake.** A stake withdrawal below a healthy threshold stays
  visible for the configured `COOLDOWN_LEDGERS` period. Replenishing it
  shows commitment.
- **Re-attest verified status.** If a slash was paired with revoking
  verified status, governance can re-attest once the underlying concern is
  resolved. See the "Attesting or revoking verified status" section of
  `DEPLOY.md`.
- **Ship a fix, point to it.** Your response can reference the commit, the
  audit report, or the patched wasm hash. On-chain history of a prompt,
  specific fix is one of the strongest signals a registrant can show.
- **Let time pass.** Reputation is a trailing signal. Consistent healthy
  behaviour after a slash is read as such.

## What governance weighs when slashing

When admins consider slashing a project, the common questions are:

- Is the behaviour clearly out of line with the registered manifest or the
  registry's documented policies?
- Was the owner given a chance to respond or correct course first? A slash
  is a last step, not a first one — but for urgent harm it may be the only
  step.
- Is there a less severe action that achieves the same outcome? Revoking
  verified status, for instance, is reversible; a slash is not.
- Would an independent reader of the on-chain record agree with the reason
  given, from the evidence cited?

Projects that answer these questions pre-emptively — in their response,
in a public post-mortem, in a `CHANGELOG.md` entry — make a reinstatement
or a reduction easier to argue for later.

## Related

- `SLASH_RESPONSE_EXAMPLES.md` — on-chain mechanics for `respond_to_slash`.
- `SLASH_RESPONSE_IMPLEMENTATION.md` — contract-side implementation notes.
- `DEPLOY.md` — "Staking, verification and slashing" section covers the
  commands governance runs.
- `GOVERNANCE_PLAYBOOK.md` — how governance decides.
- `GOVERNANCE_RECOVERY.md` — recovering from governance mistakes.
