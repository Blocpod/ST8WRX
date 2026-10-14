# Relay-backed workflow actions

`send_dm` opens the owner's private conversation with the exact recipient public
key and posts through the existing channel message sink. Both must belong to the
same community. `set_channel_topic` requires the owner to still be an owner/admin
of the workflow channel. `add_reaction` resolves the triggering message inside
that channel and atomically stores the Nostr reaction and its indexed reaction
row. It no longer calls an HTTP route. Every action rechecks current membership.

An approval gate accepts `from: any` (a current member of the workflow channel)
or an exact 64-character public key. Named roles and display-name mentions are
rejected before suspension. Example:

```yaml
name: Release approval
trigger:
  on: message_posted
steps:
  - id: permission
    action: request_approval
    from: any
    message: Approve this release?
    timeout: 24h
  - id: announce
    action: send_message
    text: Approved by the release workflow.
```

Pending approval and its run checkpoint are saved in one database transaction.
The checkpoint retains the exact workflow definition, prior outputs, and trigger
context. Editing a definition while it waits cannot replace its remaining
steps. Approval grant/deny commands persist their signed event and decision in
the same transaction. Both grant and denial require channel membership; an exact
approver spec also requires that key. Expiry is checked in the database update.

The scheduler reconciles saved approval state at startup and every 60 seconds.
It retries missing notifications with an identical signed event ID, resumes
committed grants, and cancels denied/expired runs. A per-run database advisory
lock prevents simultaneous reconcilers from executing the same continuation.
Approval continuations are serialized per relay process to avoid exhausting
the database pool with advisory-lock connections. Owner authority is checked
again before continuation. This closes the crash gap
between committing a decision and starting its in-memory continuation.

General interruption during an already-running side effect still needs operator
review; this change does not replay uncertain webhooks or messages automatically.
The new recovery path handles waiting approval gates, not an exactly-once engine
for every external side effect.

## Checks

```sh
cargo test -p buzz-workflow --lib
BUZZ_WORKFLOW_TEST_DATABASE_URL='postgresql:///st8_workflow_readiness_20261013?host=/tmp' \
  cargo test -p buzz-relay --lib workflow_readiness -- --ignored
```

The integration suite requires a dedicated database whose name contains
`workflow`; it migrates that database and creates unique test communities.
It covers checkpoint rollback, expired authorization, committed-decision
recovery, immutable workflow continuation, concurrent reconcilers, private DM
membership, reaction projection/deduplication and cross-channel rejection, and
topic authority.
