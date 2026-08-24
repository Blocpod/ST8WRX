-- Durable project/provider compute accounting, disputes, deterministic
-- settlement snapshots, and an asynchronous BSV settlement-anchor queue.
SET LOCAL lock_timeout = '5s';

CREATE TABLE st8_compute_jobs (
    community_id UUID NOT NULL REFERENCES communities(id) ON DELETE CASCADE,
    job_id BYTEA NOT NULL CHECK (octet_length(job_id) = 32),
    project_id TEXT NOT NULL CHECK (length(project_id) BETWEEN 1 AND 1024),
    requester_pubkey BYTEA NOT NULL CHECK (octet_length(requester_pubkey) = 32),
    provider_pubkey BYTEA NOT NULL CHECK (octet_length(provider_pubkey) = 32),
    node_owner_id TEXT NOT NULL CHECK (length(node_owner_id) = 64),
    pricing_policy_id BYTEA NOT NULL CHECK (octet_length(pricing_policy_id) = 32),
    request_event_id BYTEA NOT NULL CHECK (octet_length(request_event_id) = 32),
    pricing_event JSONB NOT NULL,
    job_event JSONB NOT NULL,
    request_event JSONB NOT NULL,
    job JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'authorized' CHECK (
        status IN ('authorized', 'completed', 'failed', 'cancelled', 'disputed', 'settled')
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, job_id),
    UNIQUE (community_id, request_event_id)
);

CREATE INDEX idx_st8_compute_jobs_project
    ON st8_compute_jobs (community_id, project_id, created_at, job_id);
CREATE INDEX idx_st8_compute_jobs_requester
    ON st8_compute_jobs (community_id, requester_pubkey, created_at, job_id);
CREATE INDEX idx_st8_compute_jobs_provider
    ON st8_compute_jobs (community_id, provider_pubkey, node_owner_id, created_at, job_id);

CREATE TABLE st8_compute_receipts (
    community_id UUID NOT NULL,
    receipt_id BYTEA NOT NULL CHECK (octet_length(receipt_id) = 32),
    job_id BYTEA NOT NULL CHECK (octet_length(job_id) = 32),
    project_id TEXT NOT NULL CHECK (length(project_id) BETWEEN 1 AND 1024),
    requester_pubkey BYTEA NOT NULL CHECK (octet_length(requester_pubkey) = 32),
    provider_pubkey BYTEA NOT NULL CHECK (octet_length(provider_pubkey) = 32),
    node_owner_id TEXT NOT NULL CHECK (length(node_owner_id) = 64),
    result_event_id BYTEA NOT NULL CHECK (octet_length(result_event_id) = 32),
    started_at_ms BIGINT NOT NULL,
    ended_at_ms BIGINT NOT NULL CHECK (ended_at_ms >= started_at_ms),
    cost_sats BIGINT NOT NULL CHECK (cost_sats >= 0),
    execution_status TEXT NOT NULL CHECK (
        execution_status IN ('completed', 'failed', 'cancelled')
    ),
    dispute_state TEXT NOT NULL DEFAULT 'undisputed' CHECK (
        dispute_state IN ('undisputed', 'disputed', 'resolved_upheld', 'resolved_rejected')
    ),
    settlement_id BYTEA CHECK (settlement_id IS NULL OR octet_length(settlement_id) = 32),
    result_event JSONB NOT NULL,
    receipt_event JSONB NOT NULL,
    verified_material JSONB NOT NULL,
    ledger_projection JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, receipt_id),
    UNIQUE (community_id, job_id),
    UNIQUE (community_id, result_event_id),
    FOREIGN KEY (community_id, job_id)
        REFERENCES st8_compute_jobs(community_id, job_id) ON DELETE CASCADE
);

CREATE INDEX idx_st8_compute_receipts_project
    ON st8_compute_receipts (community_id, project_id, created_at, receipt_id);
CREATE INDEX idx_st8_compute_receipts_provider
    ON st8_compute_receipts (
        community_id, provider_pubkey, node_owner_id, dispute_state, created_at, receipt_id
    );
CREATE INDEX idx_st8_compute_receipts_unsettled
    ON st8_compute_receipts (community_id, project_id, ended_at_ms, receipt_id)
    WHERE settlement_id IS NULL AND dispute_state = 'undisputed';

CREATE TABLE st8_compute_disputes (
    community_id UUID NOT NULL,
    dispute_event_id BYTEA NOT NULL CHECK (octet_length(dispute_event_id) = 32),
    receipt_id BYTEA NOT NULL CHECK (octet_length(receipt_id) = 32),
    job_id BYTEA NOT NULL CHECK (octet_length(job_id) = 32),
    signer_pubkey BYTEA NOT NULL CHECK (octet_length(signer_pubkey) = 32),
    dispute_event JSONB NOT NULL,
    dispute JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, dispute_event_id),
    UNIQUE (community_id, receipt_id, signer_pubkey),
    FOREIGN KEY (community_id, receipt_id)
        REFERENCES st8_compute_receipts(community_id, receipt_id) ON DELETE CASCADE,
    FOREIGN KEY (community_id, job_id)
        REFERENCES st8_compute_jobs(community_id, job_id) ON DELETE CASCADE
);

CREATE TABLE st8_compute_settlements (
    community_id UUID NOT NULL REFERENCES communities(id) ON DELETE CASCADE,
    settlement_id BYTEA NOT NULL CHECK (octet_length(settlement_id) = 32),
    project_id TEXT NOT NULL CHECK (length(project_id) BETWEEN 1 AND 1024),
    merkle_root BYTEA NOT NULL CHECK (octet_length(merkle_root) = 32),
    receipt_count INTEGER NOT NULL CHECK (receipt_count > 0 AND receipt_count <= 4096),
    total_sats BIGINT NOT NULL CHECK (total_sats >= 0),
    settlement_event JSONB NOT NULL,
    settlement JSONB NOT NULL,
    settlement_snapshot JSONB NOT NULL,
    prepared_anchor JSONB NOT NULL,
    anchor_state TEXT NOT NULL DEFAULT 'queued' CHECK (
        anchor_state IN ('queued', 'leased', 'broadcast', 'confirmed', 'failed')
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, settlement_id),
    UNIQUE (community_id, project_id, settlement_id)
);

ALTER TABLE st8_compute_receipts ADD CONSTRAINT fk_st8_compute_receipt_settlement
    FOREIGN KEY (community_id, project_id, settlement_id)
    REFERENCES st8_compute_settlements(community_id, project_id, settlement_id);

CREATE INDEX idx_st8_compute_settlements_project
    ON st8_compute_settlements (community_id, project_id, created_at, settlement_id);

CREATE TABLE st8_compute_anchor_jobs (
    community_id UUID NOT NULL,
    settlement_id BYTEA NOT NULL CHECK (octet_length(settlement_id) = 32),
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
    PRIMARY KEY (community_id, settlement_id),
    FOREIGN KEY (community_id, project_id, settlement_id)
        REFERENCES st8_compute_settlements(community_id, project_id, settlement_id)
        ON DELETE CASCADE,
    CHECK ((status = 'leased') = (lease_owner IS NOT NULL AND lease_until IS NOT NULL)),
    CHECK (status IN ('queued', 'leased', 'failed') OR txid IS NOT NULL)
);

CREATE INDEX idx_st8_compute_anchor_jobs_claim
    ON st8_compute_anchor_jobs (status, next_attempt_at, created_at)
    WHERE status IN ('queued', 'failed');

SELECT attach_community_write_fence('st8_compute_jobs');
SELECT attach_community_write_fence('st8_compute_receipts');
SELECT attach_community_write_fence('st8_compute_disputes');
SELECT attach_community_write_fence('st8_compute_settlements');
SELECT attach_community_write_fence('st8_compute_anchor_jobs');
