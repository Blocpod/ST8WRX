use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(crate) fn canonical_usage(
    usage: &[MeteredQuantity],
) -> Result<Vec<MeteredQuantity>, ProtocolError> {
    if usage.is_empty() || usage.len() > MAX_METER_DIMENSIONS {
        return Err(ProtocolError::InvalidUsage);
    }
    let mut canonical = usage.to_vec();
    canonical.sort_by_key(|quantity| quantity.kind);
    if canonical
        .windows(2)
        .any(|pair| pair[0].kind == pair[1].kind)
    {
        return Err(ProtocolError::InvalidUsage);
    }
    for quantity in &canonical {
        let _ = quantity.canonical_bytes()?;
    }
    Ok(canonical)
}

pub(crate) fn put_price(out: &mut Vec<u8>, price: &PriceBreakdown) -> Result<(), ProtocolError> {
    put_u64(out, price.fixed_sats);
    put_u32(out, checked_len(price.lines.len())?);
    for line in &price.lines {
        put_u16(out, line.meter.protocol_code());
        put_u64(out, line.quantity);
        put_u64(out, line.units_per_rate);
        put_u64(out, line.rate_sats);
        put_u64(out, line.charge_sats);
    }
    put_u64(out, price.total_sats);
    Ok(())
}

pub(crate) fn seconds_to_millis(seconds: i64) -> Result<i64, ProtocolError> {
    seconds
        .checked_mul(1_000)
        .ok_or(ProtocolError::InvalidReceiptWindow)
}

pub(crate) fn validate_project(project: &str) -> Result<(), ProtocolError> {
    validate_text("project", project)?;
    let mut parts = project.splitn(3, ':');
    let valid = parts.next() == Some("30621")
        && parts.next().is_some_and(|owner| {
            owner.len() == 64 && owner.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        && parts.next().is_some_and(|slug| !slug.is_empty());
    if !valid {
        return Err(ProtocolError::InvalidProject);
    }
    Ok(())
}

pub(crate) fn validate_text(field: &'static str, value: &str) -> Result<(), ProtocolError> {
    if value.trim().is_empty() || value.as_bytes().contains(&0) {
        return Err(ProtocolError::InvalidText(field));
    }
    Ok(())
}

pub(crate) fn strict_canonical_set<T: Clone>(
    values: &[T],
    encode: impl Fn(&T) -> Result<Vec<u8>, ProtocolError>,
) -> Result<Vec<Vec<u8>>, ProtocolError> {
    let mut keyed = values.iter().map(encode).collect::<Result<Vec<_>, _>>()?;
    keyed.sort();
    if keyed.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ProtocolError::InvalidCapability);
    }
    Ok(keyed)
}

pub(crate) fn strict_digest_set(values: &[Digest32]) -> Result<BTreeSet<Digest32>, ProtocolError> {
    let set: BTreeSet<_> = values.iter().copied().collect();
    if set.len() != values.len() {
        return Err(ProtocolError::InvalidSettlement);
    }
    Ok(set)
}

pub(crate) fn put_digest_set(
    out: &mut Vec<u8>,
    values: &BTreeSet<Digest32>,
) -> Result<(), ProtocolError> {
    put_u32(out, checked_len(values.len())?);
    for digest in values {
        out.extend_from_slice(digest);
    }
    Ok(())
}

pub(crate) fn checked_len(value: usize) -> Result<u32, ProtocolError> {
    u32::try_from(value).map_err(|_| ProtocolError::LengthOverflow)
}

pub(crate) fn put_vec_bytes(out: &mut Vec<u8>, values: &[Vec<u8>]) -> Result<(), ProtocolError> {
    put_u32(out, checked_len(values.len())?);
    for value in values {
        put_bytes(out, value)?;
    }
    Ok(())
}

pub(crate) fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

pub(crate) fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

pub(crate) fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

pub(crate) fn put_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_be_bytes());
}

pub(crate) fn put_option_u64(out: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            out.push(1);
            put_u64(out, value);
        }
        None => out.push(0),
    }
}

pub(crate) fn put_option_digest(out: &mut Vec<u8>, value: Option<Digest32>) {
    match value {
        Some(value) => {
            out.push(1);
            out.extend_from_slice(&value);
        }
        None => out.push(0),
    }
}

pub(crate) fn put_option_text(out: &mut Vec<u8>, value: Option<&str>) -> Result<(), ProtocolError> {
    match value {
        Some(value) => {
            validate_text("optional text", value)?;
            out.push(1);
            put_text(out, value)
        }
        None => {
            out.push(0);
            Ok(())
        }
    }
}

pub(crate) fn put_text(out: &mut Vec<u8>, value: &str) -> Result<(), ProtocolError> {
    put_bytes(out, value.as_bytes())
}

pub(crate) fn put_bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<(), ProtocolError> {
    put_u32(out, checked_len(value.len())?);
    out.extend_from_slice(value);
    Ok(())
}

pub(crate) fn hash(bytes: &[u8]) -> Digest32 {
    Sha256::digest(bytes).into()
}
