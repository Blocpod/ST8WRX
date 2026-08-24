-- Persist the complete relay-signed ledger projection independently from the
-- private normalized columns. Keeping this additive migration separate means
-- nodes that already initialized the Milestone 1 ledger can upgrade safely.
SET LOCAL lock_timeout = '5s';

ALTER TABLE st8_contributions
    ADD COLUMN ledger_projection JSONB;

UPDATE st8_contributions AS contribution
SET ledger_projection = jsonb_build_object(
    'contribution_id', encode(contribution.contribution_id, 'hex'),
    'project_id', contribution.project_id,
    'contributor', encode(contribution.contributor_pubkey, 'hex'),
    'status', contribution.status,
    'contribution_units', contribution.contribution_units,
    'record', contribution.record,
    'decision', contribution.decision,
    'project_event_id', contribution.project_event->>'id',
    'policy_event_id', contribution.policy_event->>'id',
    'claim_event_id', contribution.claim_event->>'id',
    'decision_proposal_event_id', contribution.decision_proposal_event->>'id',
    'evidence_event_ids', COALESCE((
        SELECT jsonb_agg(encode(evidence.evidence_event_id, 'hex') ORDER BY evidence.evidence_event_id)
        FROM st8_contribution_evidence AS evidence
        WHERE evidence.community_id = contribution.community_id
          AND evidence.contribution_id = contribution.contribution_id
    ), '[]'::jsonb),
    'approval_event_ids', COALESCE((
        SELECT jsonb_agg(encode(approval.approval_event_id, 'hex') ORDER BY approval.approval_event_id)
        FROM st8_governance_approvals AS approval
        WHERE approval.community_id = contribution.community_id
          AND approval.contribution_id = contribution.contribution_id
    ), '[]'::jsonb),
    'contribution_snapshot_id', CASE
        WHEN contribution.contribution_snapshot_id IS NULL THEN NULL
        ELSE encode(contribution.contribution_snapshot_id, 'hex')
    END,
    'project_snapshot_id', CASE
        WHEN contribution.project_snapshot_id IS NULL THEN NULL
        ELSE encode(contribution.project_snapshot_id, 'hex')
    END,
    'anchor_state', contribution.anchor_state
);

ALTER TABLE st8_contributions
    ALTER COLUMN ledger_projection SET NOT NULL;
