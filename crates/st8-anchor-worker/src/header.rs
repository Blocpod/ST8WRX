use crate::{json_string, json_u64};
use anyhow::{anyhow, Context};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(super) fn compute_header_hash(block: &Value) -> anyhow::Result<String> {
    let version = u32::try_from(json_u64(block, "version")?)?;
    let previous = display_hash_wire_bytes(json_string(block, "previousblockhash")?)?;
    let merkle = display_hash_wire_bytes(json_string(block, "merkleroot")?)?;
    let time = u32::try_from(json_u64(block, "time")?)?;
    let bits = header_bits(block)?;
    let nonce = u32::try_from(json_u64(block, "nonce")?)?;
    let mut header = Vec::with_capacity(80);
    header.extend_from_slice(&version.to_le_bytes());
    header.extend_from_slice(&previous);
    header.extend_from_slice(&merkle);
    header.extend_from_slice(&time.to_le_bytes());
    header.extend_from_slice(&bits.to_le_bytes());
    header.extend_from_slice(&nonce.to_le_bytes());
    let first = Sha256::digest(&header);
    let mut second: [u8; 32] = Sha256::digest(first).into();
    second.reverse();
    Ok(hex::encode(second))
}

pub(super) fn display_hash_wire_bytes(value: &str) -> anyhow::Result<Vec<u8>> {
    let mut bytes = hex::decode(value)?;
    if bytes.len() != 32 {
        return Err(anyhow!("header hash field is not 32 bytes"));
    }
    bytes.reverse();
    Ok(bytes)
}

fn header_bits(block: &Value) -> anyhow::Result<u32> {
    let bits_value = block.get("bits").context("block missing bits")?;
    let bits = if let Some(text) = bits_value.as_str() {
        u32::from_str_radix(text.trim_start_matches("0x"), 16)?
    } else {
        u32::try_from(bits_value.as_u64().context("invalid block bits")?)?
    };
    Ok(bits)
}

pub(super) fn verify_header_pow(block: &Value, hash: &str) -> anyhow::Result<()> {
    let target = compact_target(header_bits(block)?)?;
    let hash: [u8; 32] = hex::decode(hash)?
        .try_into()
        .map_err(|_| anyhow!("invalid block hash length"))?;
    if hash > target {
        return Err(anyhow!(
            "block header does not satisfy proof-of-work target"
        ));
    }
    Ok(())
}

fn compact_target(bits: u32) -> anyhow::Result<[u8; 32]> {
    let size = (bits >> 24) as usize;
    let mut word = bits & 0x007fffff;
    if bits & 0x00800000 != 0
        || size > 34
        || (size > 33 && word > 0xff)
        || (size > 32 && word > 0xffff)
    {
        return Err(anyhow!("negative or overflowing proof-of-work target"));
    }
    let mut target = [0u8; 32];
    if size <= 3 {
        word >>= 8 * (3 - size);
        target[28..].copy_from_slice(&word.to_be_bytes());
    } else {
        for i in 0..3 {
            let byte = (word >> (8 * i)) as u8;
            let power = size - 3 + i;
            if power < 32 {
                target[31 - power] = byte;
            }
        }
    }
    // BSV testnet powLimit, matching the node consensus parameters.
    let mut limit = [0xff; 32];
    limit[..4].fill(0);
    if target == [0; 32] || target > limit {
        return Err(anyhow!("zero or above-testnet-limit proof-of-work target"));
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn block_header_hash_matches_known_genesis_vector() {
        let block = serde_json::json!({
            "version": 1,
            "previousblockhash": "00".repeat(32),
            "merkleroot": "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b",
            "time": 1231006505,
            "bits": "1d00ffff",
            "nonce": 2083236893u64,
        });
        assert_eq!(
            compute_header_hash(&block).expect("header hash"),
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
        );
    }

    #[test]
    fn proof_of_work_accepts_target_boundary_and_rejects_next_hash() {
        let block = serde_json::json!({"bits": "1d00ffff"});
        let mut hash = compact_target(0x1d00ffff).unwrap();
        verify_header_pow(&block, &hex::encode(hash)).unwrap();
        hash[31] = 1;
        assert!(verify_header_pow(&block, &hex::encode(hash)).is_err());
    }

    #[test]
    fn compact_targets_reject_invalid_ranges() {
        for bits in [
            0, 0x01800000, 0x1d80ffff, 0x23000001, 0x22000100, 0x21010000, 0x01000001, 0x1d010000,
            0x207fffff,
        ] {
            assert!(compact_target(bits).is_err(), "{bits:08x}");
        }
        assert_eq!(compact_target(0x03001234).unwrap()[30..], [0x12, 0x34]);
        assert_eq!(compact_target(0x02123400).unwrap()[30..], [0x12, 0x34]);
        assert_eq!(compact_target(0x01120000).unwrap()[31], 0x12);
        assert!(compact_target(0x1d00ffff).is_ok());
    }
}
