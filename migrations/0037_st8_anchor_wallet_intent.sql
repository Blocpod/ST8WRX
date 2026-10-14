-- A committed wallet call intent is never retried without recovering its exact transaction.
SET LOCAL lock_timeout = '5s';
ALTER TABLE st8_anchor_jobs ADD COLUMN wallet_requested_at TIMESTAMPTZ;
ALTER TABLE st8_compute_anchor_jobs ADD COLUMN wallet_requested_at TIMESTAMPTZ;
-- Old attempts without transaction bytes may already have spent wallet funds.
UPDATE st8_anchor_jobs SET wallet_requested_at=updated_at WHERE attempts>0 AND txid IS NULL;
UPDATE st8_compute_anchor_jobs SET wallet_requested_at=updated_at WHERE attempts>0 AND txid IS NULL;

-- Cardinality is a V1 u32 field; 4096 was an operational cap, not a protocol boundary.
ALTER TABLE st8_project_snapshots DROP CONSTRAINT st8_project_snapshots_leaf_count_check;
ALTER TABLE st8_project_snapshots ALTER COLUMN leaf_count TYPE BIGINT;
ALTER TABLE st8_project_snapshots ADD CONSTRAINT st8_project_snapshots_leaf_count_check
    CHECK (leaf_count BETWEEN 1 AND 4294967295);
