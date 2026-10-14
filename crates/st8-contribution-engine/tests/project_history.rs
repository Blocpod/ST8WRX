use st8_contribution_engine::{ContributionAnchorReceipt, ProjectLedgerSnapshot};

#[test]
fn project_snapshot_continues_after_4096_accepted_contributions() {
    let receipt: serde_json::Value = serde_json::from_str(include_str!(
        "../../../docs/st8wrx/milestone-1/live-receipt.json"
    ))
    .unwrap();
    let receipt: ContributionAnchorReceipt =
        serde_json::from_value(receipt["contribution"].clone()).unwrap();
    let contributions = (0..4_097)
        .map(|index| {
            let mut snapshot = receipt.snapshot.clone();
            snapshot.record.summary = format!("Accepted contribution {index}");
            snapshot.decision.contribution_id = snapshot.record.id().unwrap();
            snapshot
        })
        .collect();
    let snapshot = ProjectLedgerSnapshot::new(contributions).unwrap();
    snapshot.verify().unwrap();
    let batch = snapshot.merkle_batch().unwrap();
    assert_eq!(batch.len(), 4_097);
    let commitment = snapshot.contributions.last().unwrap().commitment().unwrap();
    batch
        .proof_for(&commitment)
        .unwrap()
        .verify(&commitment, batch.root())
        .unwrap();
    let restored: ProjectLedgerSnapshot =
        serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    assert_eq!(snapshot.id().unwrap(), restored.id().unwrap());
}
