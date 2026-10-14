use super::*;

fn contribution() -> PersistedContributionReceipt {
    serde_json::from_str(include_str!(
        "../../../docs/st8wrx/milestone-1/live-receipt.json"
    ))
    .expect("public contribution fixture")
}

fn compute() -> PersistedComputeSettlementReceipt {
    serde_json::from_str(include_str!(
        "../../../docs/st8wrx/compute-m1/bsv-receipt.json"
    ))
    .expect("public compute fixture")
}

fn rewrite_nonce(network: &mut BsvNetworkEvidence) {
    let block = network.woc_block.as_mut().expect("block");
    block["nonce"] = serde_json::json!(block["nonce"].as_u64().unwrap() ^ 1);
    let hash = compute_header_hash(block).expect("modified header hash");
    block["hash"] = serde_json::json!(hash);
    network.arc_status["blockHash"] = serde_json::json!(hash);
    network.woc_transaction["blockhash"] = serde_json::json!(hash);
}

#[test]
fn original_receipts_verify_without_claiming_chain_authenticity() {
    assert_eq!(
        contribution().verify_with_trusted_block_hash(None).unwrap(),
        "header_inclusion_verified"
    );
    assert_eq!(
        compute().verify_with_trusted_block_hash(None).unwrap(),
        "header_inclusion_verified"
    );
    let mut receipt = contribution();
    receipt.network.verification_state = "header_inclusion_verified".into();
    receipt.verify().unwrap();
    receipt.network.verification_state = "trusted_block_inclusion_verified".into();
    assert!(receipt.verify().is_err());
}

#[test]
fn coherent_nonce_and_hash_substitution_fails_proof_of_work() {
    let mut receipt = contribution();
    rewrite_nonce(&mut receipt.network);
    assert!(receipt
        .verify()
        .unwrap_err()
        .to_string()
        .contains("proof-of-work"));
    let mut receipt = compute();
    rewrite_nonce(&mut receipt.network);
    assert!(receipt
        .verify()
        .unwrap_err()
        .to_string()
        .contains("proof-of-work"));
}

#[test]
fn independent_hash_pin_is_required_for_trusted_inclusion() {
    // Fixed public block identities: deliberately not derived from the receipt under test.
    const CONTRIBUTION_BLOCK: &str =
        "0000000000c046761bce820232597af48e915d011a3e3cd4bf6bd78b06f00224";
    assert_eq!(
        compute()
            .verify_with_trusted_block_hash(Some(
                "000000000044d39b39c6c65cc52fd9583c88a92a3425583e152cdcb992d1e5b1"
            ))
            .unwrap(),
        "trusted_block_inclusion_verified"
    );
    let receipt = contribution();
    assert_eq!(
        receipt
            .verify_with_trusted_block_hash(Some(CONTRIBUTION_BLOCK))
            .unwrap(),
        "trusted_block_inclusion_verified"
    );
    assert!(receipt
        .verify_with_trusted_block_hash(Some(&"11".repeat(32)))
        .is_err());
    assert!(receipt
        .verify_with_trusted_block_hash(Some("abcd"))
        .is_err());
}

#[test]
fn unmined_observations_are_not_trusted_chain_evidence() {
    let mut receipt = contribution();
    receipt.network.arc_status["txStatus"] = serde_json::json!("SEEN_ON_NETWORK");
    receipt.network.verification_state = "seen_on_testnet".into();
    assert_eq!(
        receipt.verify_with_trusted_block_hash(None).unwrap(),
        "network_observation_unverified"
    );
    receipt.network.verification_state = "network_observation_unverified".into();
    receipt.verify().unwrap();
    assert!(receipt
        .verify_with_trusted_block_hash(Some(
            "0000000000c046761bce820232597af48e915d011a3e3cd4bf6bd78b06f00224"
        ))
        .is_err());
}

#[test]
fn recovered_wallet_transaction_must_match_exact_prepared_commitment() {
    let receipt = contribution();
    let tx = &receipt.contribution.transaction;
    let payload = &receipt.contribution.anchor_payload;
    resume_signed_anchor_transaction(
        payload,
        &tx.txid,
        tx.raw_transaction.clone(),
        tx.atomic_beef.clone().unwrap(),
    )
    .unwrap();
    let mut wrong = payload.clone();
    wrong.snapshot_id[0] ^= 1;
    assert!(resume_signed_anchor_transaction(
        &wrong,
        &tx.txid,
        tx.raw_transaction.clone(),
        tx.atomic_beef.clone().unwrap()
    )
    .is_err());
}
