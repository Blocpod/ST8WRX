-- Durable, project-scoped ST8WRX contribution ledger and asynchronous BSV
-- anchoring queue. Blockchain work is deliberately outside the relay write
-- transaction and is claimed by a separate worker.
SET LOCAL lock_timeout = '5s';

CREATE TABLE st8_contributions (
    community_id UUID NOT NULL REFERENCES communities(id) ON DELETE CASCADE,
    contribution_id BYTEA NOT NULL CHECK (octet_length(contribution_id) = 32),
    project_id TEXT NOT NULL CHECK (length(project_id) BETWEEN 1 AND 1024),
    contributor_pubkey BYTEA NOT NULL CHECK (octet_length(contributor_pubkey) = 32),
    status TEXT NOT NULL CHECK (status IN ('accepted', 'adjusted', 'rejected')),
    contribution_units BIGINT NOT NULL CHECK (contribution_units >= 0),
    project_event JSONB NOT NULL,
    policy_event JSONB NOT NULL,
    claim_event JSONB NOT NULL,
    decision_proposal_event JSONB NOT NULL,
    record JSONB NOT NULL,
    decision JSONB NOT NULL,
    contribution_snapshot JSONB,
    contribution_snapshot_id BYTEA CHECK (
        contribution_snapshot_id IS NULL OR octet_length(contribution_snapshot_id) = 32
    ),
    project_snapshot_id BYTEA CHECK (
        project_snapshot_id IS NULL OR octet_length(project_snapshot_id) = 32
    ),
    anchor_state TEXT NOT NULL DEFAULT 'not_applicable' CHECK (
        anchor_state IN ('not_applicable', 'queued', 'leased', 'broadcast', 'confirmed', 'failed')
    ),
    receipt JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, contribution_id),
    CHECK (
        (status = 'rejected' AND contribution_units = 0 AND anchor_state = 'not_applicable')
        OR (status IN ('accepted', 'adjusted') AND contribution_units > 0)
    )
);

CREATE INDEX idx_st8_contributions_project
    ON st8_contributions (community_id, project_id, created_at, contribution_id);
CREATE INDEX idx_st8_contributions_contributor
    ON st8_contributions (community_id, contributor_pubkey, created_at, contribution_id);
CREATE INDEX idx_st8_contributions_anchor_state
    ON st8_contributions (community_id, anchor_state, updated_at)
    WHERE anchor_state <> 'not_applicable';

CREATE TABLE st8_contribution_evidence (
    community_id UUID NOT NULL,
    contribution_id BYTEA NOT NULL CHECK (octet_length(contribution_id) = 32),
    evidence_event_id BYTEA NOT NULL CHECK (octet_length(evidence_event_id) = 32),
    evidence_kind INTEGER NOT NULL CHECK (evidence_kind BETWEEN 0 AND 65535),
    evidence_event JSONB NOT NULL,
    PRIMARY KEY (community_id, contribution_id, evidence_event_id),
    FOREIGN KEY (community_id, contribution_id)
        REFERENCES st8_contributions(community_id, contribution_id) ON DELETE CASCADE
);

CREATE TABLE st8_governance_approvals (
    community_id UUID NOT NULL,
    contribution_id BYTEA NOT NULL CHECK (octet_length(contribution_id) = 32),
    approver_pubkey BYTEA NOT NULL CHECK (octet_length(approver_pubkey) = 32),
    approval_event_id BYTEA NOT NULL CHECK (octet_length(approval_event_id) = 32),
    approval_event JSONB NOT NULL,
    approved_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (community_id, contribution_id, approver_pubkey),
    UNIQUE (community_id, approval_event_id),
    FOREIGN KEY (community_id, contribution_id)
        REFERENCES st8_contributions(community_id, contribution_id) ON DELETE CASCADE
);

CREATE TABLE st8_project_snapshots (
    community_id UUID NOT NULL REFERENCES communities(id) ON DELETE CASCADE,
    snapshot_id BYTEA NOT NULL CHECK (octet_length(snapshot_id) = 32),
    project_id TEXT NOT NULL CHECK (length(project_id) BETWEEN 1 AND 1024),
    merkle_root BYTEA NOT NULL CHECK (octet_length(merkle_root) = 32),
    leaf_count INTEGER NOT NULL CHECK (leaf_count > 0 AND leaf_count <= 4096),
    snapshot JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, snapshot_id),
    UNIQUE (community_id, project_id, snapshot_id)
);

ALTER TABLE st8_contributions ADD CONSTRAINT fk_st8_contribution_project_snapshot
    FOREIGN KEY (community_id, project_id, project_snapshot_id)
    REFERENCES st8_project_snapshots(community_id, project_id, snapshot_id);

CREATE INDEX idx_st8_project_snapshots_project
    ON st8_project_snapshots (community_id, project_id, created_at, snapshot_id);

CREATE TABLE st8_anchor_jobs (
    community_id UUID NOT NULL,
    snapshot_id BYTEA NOT NULL CHECK (octet_length(snapshot_id) = 32),
    project_id TEXT NOT NULL CHECK (length(project_id) BETWEEN 1 AND 1024),
    status TEXT NOT NULL DEFAULT 'queued' CHECK (
        status IN ('queued', 'leased', 'broadcast', 'confirmed', 'failed')
    ),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    lease_owner TEXT,
    lease_until TIMESTAMPTZ,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    prepared_anchor JSONB NOT NULL,
    txid BYTEA CHECK (txid IS NULL OR octet_length(txid) = 32),
    raw_transaction BYTEA,
    atomic_beef BYTEA,
    broadcast_receipt JSONB,
    network_evidence JSONB,
    receipt JSONB,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, snapshot_id),
    FOREIGN KEY (community_id, project_id, snapshot_id)
        REFERENCES st8_project_snapshots(community_id, project_id, snapshot_id) ON DELETE CASCADE,
    CHECK ((status = 'leased') = (lease_owner IS NOT NULL AND lease_until IS NOT NULL)),
    CHECK (status IN ('queued', 'leased', 'failed') OR txid IS NOT NULL)
);

CREATE INDEX idx_st8_anchor_jobs_claim
    ON st8_anchor_jobs (status, next_attempt_at, created_at)
    WHERE status IN ('queued', 'failed');

SELECT attach_community_write_fence('st8_contributions');
SELECT attach_community_write_fence('st8_contribution_evidence');
SELECT attach_community_write_fence('st8_governance_approvals');
SELECT attach_community_write_fence('st8_project_snapshots');
SELECT attach_community_write_fence('st8_anchor_jobs');
