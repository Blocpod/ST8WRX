-- Retain the owner's final signed arbitration on the exact accepted dispute.
-- resolved_upheld means the dispute was upheld (charge voided);
-- resolved_rejected means the dispute was rejected (full charge released).
SET LOCAL lock_timeout = '5s';

ALTER TABLE st8_compute_disputes
    ADD COLUMN resolution_event_id BYTEA CHECK (octet_length(resolution_event_id) = 32),
    ADD COLUMN resolution_event JSONB,
    ADD CONSTRAINT compute_resolution_complete CHECK (
        (resolution_event_id IS NULL) = (resolution_event IS NULL)
    );

CREATE UNIQUE INDEX idx_st8_compute_resolution_event
    ON st8_compute_disputes (community_id, resolution_event_id)
    WHERE resolution_event_id IS NOT NULL;
