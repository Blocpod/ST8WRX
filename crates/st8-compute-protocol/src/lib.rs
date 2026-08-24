#![deny(unsafe_code)]
#![warn(missing_docs)]
//! Deterministic, zero-I/O compute accounting primitives for ST8WRX.
//!
//! Compute expenses and provider earnings are denominated in integer satoshis.
//! They are deliberately unrelated to non-transferable Contribution Units.
//! Every digest uses a versioned domain and length-delimited binary encoding;
//! no protocol identity depends on JSON map or caller-provided set ordering.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

mod canonical;
use canonical::*;

const CAPABILITY_DOMAIN: &[u8] = b"ST8WRX\0COMPUTE_CAPABILITY\0V1";
const PRICING_DOMAIN: &[u8] = b"ST8WRX\0COMPUTE_PRICING\0V1";
const JOB_DOMAIN: &[u8] = b"ST8WRX\0COMPUTE_JOB\0V1";
const RECEIPT_DOMAIN: &[u8] = b"ST8WRX\0COMPUTE_RECEIPT\0V1";
const SETTLEMENT_DOMAIN: &[u8] = b"ST8WRX\0COMPUTE_SETTLEMENT\0V1";
const SETTLEMENT_LEAF_DOMAIN: &[u8] = b"ST8WRX\0COMPUTE_SETTLEMENT_LEAF\0V1";
const SETTLEMENT_NODE_DOMAIN: &[u8] = b"ST8WRX\0COMPUTE_SETTLEMENT_NODE\0V1";

/// A protocol digest.
pub type Digest32 = [u8; 32];

/// Maximum distinct metering dimensions in one policy or receipt.
pub const MAX_METER_DIMENSIONS: usize = 32;
/// Maximum receipts committed by one settlement snapshot.
pub const MAX_SETTLEMENT_RECEIPTS: usize = 4_096;

/// Compute workload category. Codes are permanent protocol values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum WorkloadKind {
    /// Language-model inference.
    LlmInference,
    /// General CPU execution.
    CpuTask,
    /// GPU-accelerated execution other than LLM inference.
    GpuTask,
    /// Agent or workflow execution composed from one or more runtime actions.
    AgentTask,
}

impl WorkloadKind {
    /// Returns the permanent protocol code.
    pub const fn protocol_code(self) -> u16 {
        match self {
            Self::LlmInference => 1,
            Self::CpuTask => 2,
            Self::GpuTask => 3,
            Self::AgentTask => 4,
        }
    }
}

/// A metering dimension with a permanent protocol code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MeterKind {
    /// Target/provider-tokenized prompt tokens.
    InputTokens,
    /// Target/provider-tokenized generated tokens.
    OutputTokens,
    /// Monotonic wall-clock execution milliseconds.
    WallTimeMs,
    /// Process or runtime CPU milliseconds.
    CpuTimeMs,
    /// Device-reported GPU execution milliseconds.
    GpuTimeMs,
    /// Peak resident or device memory bytes.
    PeakMemoryBytes,
    /// Bytes received by the execution runtime.
    NetworkIngressBytes,
    /// Bytes sent by the execution runtime.
    NetworkEgressBytes,
    /// Completed job count, normally one.
    Jobs,
}

impl MeterKind {
    /// Returns the permanent protocol code.
    pub const fn protocol_code(self) -> u16 {
        match self {
            Self::InputTokens => 1,
            Self::OutputTokens => 2,
            Self::WallTimeMs => 3,
            Self::CpuTimeMs => 4,
            Self::GpuTimeMs => 5,
            Self::PeakMemoryBytes => 6,
            Self::NetworkIngressBytes => 7,
            Self::NetworkEgressBytes => 8,
            Self::Jobs => 9,
        }
    }
}

/// Quality and authority of a runtime measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MeasurementQuality {
    /// Exact counter emitted by the target runtime or model response.
    TargetExact,
    /// Exact monotonic counter observed by the local host runtime.
    HostExact,
    /// Bounded aggregate delta whose attribution was unambiguous.
    AttributedDelta,
}

impl MeasurementQuality {
    const fn protocol_code(self) -> u16 {
        match self {
            Self::TargetExact => 1,
            Self::HostExact => 2,
            Self::AttributedDelta => 3,
        }
    }
}

/// One real runtime measurement. Missing signals are omitted, never guessed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MeteredQuantity {
    /// Metered resource dimension.
    pub kind: MeterKind,
    /// Non-negative integer quantity in the dimension's named unit.
    pub quantity: u64,
    /// Measurement authority/precision.
    pub quality: MeasurementQuality,
    /// Stable runtime signal name, such as `openai.usage.output_tokens`.
    pub source: String,
}

impl MeteredQuantity {
    fn canonical_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        validate_text("meter source", &self.source)?;
        let mut out = Vec::new();
        put_u16(&mut out, self.kind.protocol_code());
        put_u64(&mut out, self.quantity);
        put_u16(&mut out, self.quality.protocol_code());
        put_text(&mut out, &self.source)?;
        Ok(out)
    }
}

/// A model served by a compute node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModelCapability {
    /// Exact runtime model identifier.
    pub model_id: String,
    /// Maximum context length when the runtime reports it.
    pub context_tokens: Option<u64>,
}

/// A GPU or accelerator visible to the node runtime.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AcceleratorCapability {
    /// Runtime-reported device name.
    pub name: String,
    /// Runtime-reported memory bytes, when available.
    pub memory_bytes: Option<u64>,
}

/// Signed, versioned capability description for one persistent Mesh node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCapability {
    /// Persistent Mesh owner identity.
    pub node_owner_id: String,
    /// Ed25519 public key backing `node_owner_id`.
    pub node_public_key: Digest32,
    /// Nostr identity accountable for this provider node.
    pub provider: String,
    /// Runtime software/version string.
    pub runtime_version: String,
    /// Logical CPU cores observed by the runtime.
    pub cpu_cores: u32,
    /// System RAM bytes observed by the runtime.
    pub ram_bytes: u64,
    /// GPU/NPU/accelerator inventory.
    pub accelerators: Vec<AcceleratorCapability>,
    /// Exact available model identifiers.
    pub models: Vec<ModelCapability>,
    /// Supported workload classes.
    pub workloads: Vec<WorkloadKind>,
    /// Optional measured/administrative ingress ceiling in bits per second.
    pub ingress_bits_per_second: Option<u64>,
    /// Optional measured/administrative egress ceiling in bits per second.
    pub egress_bits_per_second: Option<u64>,
    /// Unix timestamp after which this availability statement is stale.
    pub available_until: i64,
    /// Versioned pricing policy offered by this node.
    pub pricing_policy_id: Digest32,
    /// Content-addressed reputation evidence references.
    pub reputation_refs: Vec<Digest32>,
}

impl NodeCapability {
    /// Returns versioned canonical bytes.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        validate_text("node owner id", &self.node_owner_id)?;
        validate_text("provider", &self.provider)?;
        validate_text("runtime version", &self.runtime_version)?;
        if self.cpu_cores == 0 || self.ram_bytes == 0 || self.workloads.is_empty() {
            return Err(ProtocolError::InvalidCapability);
        }
        let accelerators = strict_canonical_set(&self.accelerators, |item| {
            validate_text("accelerator name", &item.name)?;
            let mut bytes = Vec::new();
            put_text(&mut bytes, &item.name)?;
            put_option_u64(&mut bytes, item.memory_bytes);
            Ok(bytes)
        })?;
        let models = strict_canonical_set(&self.models, |item| {
            validate_text("model id", &item.model_id)?;
            let mut bytes = Vec::new();
            put_text(&mut bytes, &item.model_id)?;
            put_option_u64(&mut bytes, item.context_tokens);
            Ok(bytes)
        })?;
        let workloads: BTreeSet<_> = self.workloads.iter().copied().collect();
        let reputation: BTreeSet<_> = self.reputation_refs.iter().copied().collect();
        if workloads.len() != self.workloads.len() || reputation.len() != self.reputation_refs.len()
        {
            return Err(ProtocolError::InvalidCapability);
        }
        let mut out = Vec::new();
        out.extend_from_slice(CAPABILITY_DOMAIN);
        put_text(&mut out, &self.node_owner_id)?;
        out.extend_from_slice(&self.node_public_key);
        put_text(&mut out, &self.provider)?;
        put_text(&mut out, &self.runtime_version)?;
        put_u32(&mut out, self.cpu_cores);
        put_u64(&mut out, self.ram_bytes);
        put_vec_bytes(&mut out, &accelerators)?;
        put_vec_bytes(&mut out, &models)?;
        put_u32(&mut out, checked_len(workloads.len())?);
        for workload in workloads {
            put_u16(&mut out, workload.protocol_code());
        }
        put_option_u64(&mut out, self.ingress_bits_per_second);
        put_option_u64(&mut out, self.egress_bits_per_second);
        put_i64(&mut out, self.available_until);
        out.extend_from_slice(&self.pricing_policy_id);
        put_u32(&mut out, checked_len(reputation.len())?);
        for digest in reputation {
            out.extend_from_slice(&digest);
        }
        Ok(out)
    }

    /// Computes the deterministic capability ID.
    pub fn id(&self) -> Result<Digest32, ProtocolError> {
        Ok(hash(&self.canonical_bytes()?))
    }
}

/// One integer pricing rule: `ceil(quantity * rate_sats / units_per_rate)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PriceRate {
    /// Metered dimension billed by this rule.
    pub meter: MeterKind,
    /// Integer quantity represented by one rate unit.
    pub units_per_rate: u64,
    /// Integer satoshis charged per rate unit.
    pub rate_sats: u64,
}

/// Immutable provider pricing accepted by a signed compute job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingPolicy {
    /// Full kind-30621 project coordinate.
    pub project: String,
    /// Nostr provider identity.
    pub provider: String,
    /// Persistent Mesh owner identity.
    pub node_owner_id: String,
    /// Immutable human-readable version.
    pub version: String,
    /// Fixed completed-job charge in satoshis.
    pub fixed_sats: u64,
    /// Metered rates. A meter may appear at most once.
    pub rates: Vec<PriceRate>,
    /// Whether an unsuccessful execution can charge measured usage.
    pub failed_jobs_billable: bool,
}

impl PricingPolicy {
    /// Returns versioned canonical bytes.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        validate_project(&self.project)?;
        validate_text("provider", &self.provider)?;
        validate_text("node owner id", &self.node_owner_id)?;
        validate_text("pricing version", &self.version)?;
        if self.rates.len() > MAX_METER_DIMENSIONS {
            return Err(ProtocolError::TooManyMeters);
        }
        let mut rates = self.rates.clone();
        rates.sort_by_key(|rate| rate.meter);
        if rates.windows(2).any(|pair| pair[0].meter == pair[1].meter)
            || rates
                .iter()
                .any(|rate| rate.units_per_rate == 0 || rate.rate_sats == 0)
        {
            return Err(ProtocolError::InvalidPricing);
        }
        let mut out = Vec::new();
        out.extend_from_slice(PRICING_DOMAIN);
        put_text(&mut out, &self.project)?;
        put_text(&mut out, &self.provider)?;
        put_text(&mut out, &self.node_owner_id)?;
        put_text(&mut out, &self.version)?;
        put_u64(&mut out, self.fixed_sats);
        put_u32(&mut out, checked_len(rates.len())?);
        for rate in rates {
            put_u16(&mut out, rate.meter.protocol_code());
            put_u64(&mut out, rate.units_per_rate);
            put_u64(&mut out, rate.rate_sats);
        }
        out.push(u8::from(self.failed_jobs_billable));
        Ok(out)
    }

    /// Computes the deterministic pricing-policy ID.
    pub fn id(&self) -> Result<Digest32, ProtocolError> {
        Ok(hash(&self.canonical_bytes()?))
    }

    /// Prices real measured usage with checked integer arithmetic.
    pub fn price(
        &self,
        status: ExecutionStatus,
        usage: &[MeteredQuantity],
    ) -> Result<PriceBreakdown, ProtocolError> {
        let _ = self.canonical_bytes()?;
        let billable = status == ExecutionStatus::Completed || self.failed_jobs_billable;
        if !billable {
            return Ok(PriceBreakdown {
                fixed_sats: 0,
                lines: Vec::new(),
                total_sats: 0,
            });
        }
        let quantities = canonical_usage(usage)?;
        let rates: BTreeMap<_, _> = self.rates.iter().map(|rate| (rate.meter, rate)).collect();
        let mut total = self.fixed_sats;
        let mut lines = Vec::new();
        for quantity in quantities {
            let Some(rate) = rates.get(&quantity.kind) else {
                continue;
            };
            let numerator = u128::from(quantity.quantity)
                .checked_mul(u128::from(rate.rate_sats))
                .ok_or(ProtocolError::CostOverflow)?;
            let denominator = u128::from(rate.units_per_rate);
            let sats_u128 = numerator
                .checked_add(denominator.saturating_sub(1))
                .ok_or(ProtocolError::CostOverflow)?
                / denominator;
            let sats = u64::try_from(sats_u128).map_err(|_| ProtocolError::CostOverflow)?;
            total = total.checked_add(sats).ok_or(ProtocolError::CostOverflow)?;
            lines.push(PriceLine {
                meter: quantity.kind,
                quantity: quantity.quantity,
                units_per_rate: rate.units_per_rate,
                rate_sats: rate.rate_sats,
                charge_sats: sats,
            });
        }
        Ok(PriceBreakdown {
            fixed_sats: self.fixed_sats,
            lines,
            total_sats: total,
        })
    }
}

/// A requester-signed, project-scoped compute authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeJob {
    /// Project bearing the compute expense.
    pub project: String,
    /// Nostr requester identity.
    pub requester: String,
    /// Nostr provider identity selected by the requester.
    pub provider: String,
    /// Persistent Mesh node selected by the requester.
    pub node_owner_id: String,
    /// Workload category.
    pub workload: WorkloadKind,
    /// Agent identity responsible for the work, when applicable.
    pub agent: Option<String>,
    /// Exact requested model/runtime identifier.
    pub model: String,
    /// Exact immutable pricing policy accepted by the requester.
    pub pricing_policy_id: Digest32,
    /// Digest of the private input or public job request evidence.
    pub input_commitment: Digest32,
    /// Signed Buzz Mesh/job request event ID.
    pub request_event_id: Digest32,
    /// Maximum authorized cost in satoshis.
    pub max_cost_sats: u64,
    /// Request time in Unix seconds.
    pub created_at: i64,
    /// Expiration time in Unix seconds.
    pub expires_at: i64,
    /// Requester-chosen replay nonce.
    pub nonce: Digest32,
}

impl ComputeJob {
    /// Returns versioned canonical bytes.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        validate_project(&self.project)?;
        validate_text("requester", &self.requester)?;
        validate_text("provider", &self.provider)?;
        validate_text("node owner id", &self.node_owner_id)?;
        validate_text("model", &self.model)?;
        if self.expires_at <= self.created_at {
            return Err(ProtocolError::InvalidJobWindow);
        }
        let mut out = Vec::new();
        out.extend_from_slice(JOB_DOMAIN);
        put_text(&mut out, &self.project)?;
        put_text(&mut out, &self.requester)?;
        put_text(&mut out, &self.provider)?;
        put_text(&mut out, &self.node_owner_id)?;
        put_u16(&mut out, self.workload.protocol_code());
        put_option_text(&mut out, self.agent.as_deref())?;
        put_text(&mut out, &self.model)?;
        out.extend_from_slice(&self.pricing_policy_id);
        out.extend_from_slice(&self.input_commitment);
        out.extend_from_slice(&self.request_event_id);
        put_u64(&mut out, self.max_cost_sats);
        put_i64(&mut out, self.created_at);
        put_i64(&mut out, self.expires_at);
        out.extend_from_slice(&self.nonce);
        Ok(out)
    }

    /// Computes the deterministic job ID.
    pub fn id(&self) -> Result<Digest32, ProtocolError> {
        Ok(hash(&self.canonical_bytes()?))
    }
}

/// Terminal runtime state of a compute receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ExecutionStatus {
    /// The requested result completed.
    Completed,
    /// Execution failed after starting.
    Failed,
    /// Execution was cancelled.
    Cancelled,
}

impl ExecutionStatus {
    const fn protocol_code(self) -> u16 {
        match self {
            Self::Completed => 1,
            Self::Failed => 2,
            Self::Cancelled => 3,
        }
    }
}

/// One deterministically calculated pricing line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceLine {
    /// Metered dimension.
    pub meter: MeterKind,
    /// Billed measured quantity.
    pub quantity: u64,
    /// Quantity represented by one rate unit.
    pub units_per_rate: u64,
    /// Integer satoshis per rate unit.
    pub rate_sats: u64,
    /// Rounded-up integer charge in satoshis.
    pub charge_sats: u64,
}

/// Exact deterministic price calculation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceBreakdown {
    /// Fixed charge.
    pub fixed_sats: u64,
    /// Metered charges in meter-code order.
    pub lines: Vec<PriceLine>,
    /// Fixed plus metered charges.
    pub total_sats: u64,
}

/// Node-signed deterministic compute result and accounting record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeReceipt {
    /// Deterministic requester-signed job ID.
    pub job_id: Digest32,
    /// Project bearing the expense.
    pub project: String,
    /// Requester identity.
    pub requester: String,
    /// Provider identity.
    pub provider: String,
    /// Persistent Mesh owner identity.
    pub node_owner_id: String,
    /// Workload category.
    pub workload: WorkloadKind,
    /// Agent identity, when applicable.
    pub agent: Option<String>,
    /// Actual model/runtime identifier.
    pub model: String,
    /// Pricing policy used.
    pub pricing_policy_id: Digest32,
    /// Signed job request event.
    pub request_event_id: Digest32,
    /// Signed terminal result event.
    pub result_event_id: Digest32,
    /// Agent-signed usage metric event, when one was published.
    pub agent_metric_event_id: Option<Digest32>,
    /// Actual execution start in Unix milliseconds.
    pub started_at_ms: i64,
    /// Actual execution end in Unix milliseconds.
    pub ended_at_ms: i64,
    /// Real runtime measurements. Missing signals are absent.
    pub usage: Vec<MeteredQuantity>,
    /// Deterministic pricing calculation.
    pub price: PriceBreakdown,
    /// SHA-256 commitment to the result without revealing it.
    pub result_commitment: Digest32,
    /// Terminal execution state.
    pub status: ExecutionStatus,
    /// Exact Mesh routing target observed for the execution.
    pub routing_target: String,
    /// Versioned runtime metering implementation.
    pub meter_version: String,
    /// Additional signed/content-addressed evidence IDs.
    pub evidence: Vec<Digest32>,
}

impl ComputeReceipt {
    /// Revalidates job binding and deterministic price calculation.
    pub fn validate_for(
        &self,
        job: &ComputeJob,
        pricing: &PricingPolicy,
    ) -> Result<(), ProtocolError> {
        if self.job_id != job.id()?
            || self.project != job.project
            || self.requester != job.requester
            || self.provider != job.provider
            || self.node_owner_id != job.node_owner_id
            || self.workload != job.workload
            || self.agent != job.agent
            || self.model != job.model
            || self.pricing_policy_id != job.pricing_policy_id
            || self.request_event_id != job.request_event_id
            || pricing.id()? != job.pricing_policy_id
            || pricing.project != job.project
            || pricing.provider != job.provider
            || pricing.node_owner_id != job.node_owner_id
        {
            return Err(ProtocolError::ReceiptBindingMismatch);
        }
        if self.started_at_ms < seconds_to_millis(job.created_at)?
            || self.ended_at_ms < self.started_at_ms
            || self.started_at_ms >= seconds_to_millis(job.expires_at)?
        {
            return Err(ProtocolError::InvalidReceiptWindow);
        }
        let expected = pricing.price(self.status, &self.usage)?;
        if self.price != expected || self.price.total_sats > job.max_cost_sats {
            return Err(ProtocolError::PriceMismatch);
        }
        let _ = self.canonical_bytes()?;
        Ok(())
    }

    /// Returns versioned canonical bytes.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        validate_project(&self.project)?;
        validate_text("requester", &self.requester)?;
        validate_text("provider", &self.provider)?;
        validate_text("node owner id", &self.node_owner_id)?;
        validate_text("model", &self.model)?;
        validate_text("routing target", &self.routing_target)?;
        validate_text("meter version", &self.meter_version)?;
        if self.ended_at_ms < self.started_at_ms {
            return Err(ProtocolError::InvalidReceiptWindow);
        }
        let usage = canonical_usage(&self.usage)?;
        let evidence: BTreeSet<_> = self.evidence.iter().copied().collect();
        if evidence.len() != self.evidence.len() {
            return Err(ProtocolError::ReceiptBindingMismatch);
        }
        let mut out = Vec::new();
        out.extend_from_slice(RECEIPT_DOMAIN);
        out.extend_from_slice(&self.job_id);
        put_text(&mut out, &self.project)?;
        put_text(&mut out, &self.requester)?;
        put_text(&mut out, &self.provider)?;
        put_text(&mut out, &self.node_owner_id)?;
        put_u16(&mut out, self.workload.protocol_code());
        put_option_text(&mut out, self.agent.as_deref())?;
        put_text(&mut out, &self.model)?;
        out.extend_from_slice(&self.pricing_policy_id);
        out.extend_from_slice(&self.request_event_id);
        out.extend_from_slice(&self.result_event_id);
        put_option_digest(&mut out, self.agent_metric_event_id);
        put_i64(&mut out, self.started_at_ms);
        put_i64(&mut out, self.ended_at_ms);
        put_u32(&mut out, checked_len(usage.len())?);
        for quantity in usage {
            put_bytes(&mut out, &quantity.canonical_bytes()?)?;
        }
        put_price(&mut out, &self.price)?;
        out.extend_from_slice(&self.result_commitment);
        put_u16(&mut out, self.status.protocol_code());
        put_text(&mut out, &self.routing_target)?;
        put_text(&mut out, &self.meter_version)?;
        put_u32(&mut out, checked_len(evidence.len())?);
        for digest in evidence {
            out.extend_from_slice(&digest);
        }
        Ok(out)
    }

    /// Computes the deterministic receipt ID.
    pub fn id(&self) -> Result<Digest32, ProtocolError> {
        Ok(hash(&self.canonical_bytes()?))
    }
}

/// Provider balance in one settlement snapshot.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProviderBalance {
    /// Nostr provider identity.
    pub provider: String,
    /// Persistent Mesh owner identity.
    pub node_owner_id: String,
    /// Gross undisputed earnings in satoshis.
    pub earned_sats: u64,
    /// Number of receipts contributing to the balance.
    pub receipt_count: u64,
}

/// Periodic deterministic aggregation of undisputed compute receipts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeSettlement {
    /// Project whose compute state is settled.
    pub project: String,
    /// Inclusive period start in Unix seconds.
    pub period_start: i64,
    /// Exclusive period end in Unix seconds.
    pub period_end: i64,
    /// Included terminal receipt IDs.
    pub receipt_ids: Vec<Digest32>,
    /// Explicitly excluded disputed receipt IDs.
    pub disputed_receipt_ids: Vec<Digest32>,
    /// Project-scoped deterministic Merkle root of the included receipt IDs.
    pub receipt_merkle_root: Digest32,
    /// Deterministically aggregated provider balances.
    pub balances: Vec<ProviderBalance>,
    /// Total project expense in satoshis.
    pub total_sats: u64,
}

impl ComputeSettlement {
    /// Constructs an aggregation from verified receipts and a disputed set.
    pub fn from_receipts(
        project: String,
        period_start: i64,
        period_end: i64,
        receipts: &[ComputeReceipt],
        disputed: &[Digest32],
    ) -> Result<Self, ProtocolError> {
        validate_project(&project)?;
        if period_end <= period_start
            || receipts.is_empty()
            || receipts.len() > MAX_SETTLEMENT_RECEIPTS
        {
            return Err(ProtocolError::InvalidSettlement);
        }
        let period_start_ms = seconds_to_millis(period_start)?;
        let period_end_ms = seconds_to_millis(period_end)?;
        let disputed_set: BTreeSet<_> = disputed.iter().copied().collect();
        if disputed_set.len() != disputed.len() {
            return Err(ProtocolError::InvalidSettlement);
        }
        let mut seen = BTreeSet::new();
        let mut included = Vec::new();
        let mut balances: BTreeMap<(String, String), (u64, u64)> = BTreeMap::new();
        let mut total = 0u64;
        for receipt in receipts {
            let id = receipt.id()?;
            if receipt.project != project
                || receipt.ended_at_ms < period_start_ms
                || receipt.ended_at_ms >= period_end_ms
                || !seen.insert(id)
            {
                return Err(ProtocolError::InvalidSettlement);
            }
            if disputed_set.contains(&id) {
                continue;
            }
            included.push(id);
            total = total
                .checked_add(receipt.price.total_sats)
                .ok_or(ProtocolError::CostOverflow)?;
            let balance = balances
                .entry((receipt.provider.clone(), receipt.node_owner_id.clone()))
                .or_default();
            balance.0 = balance
                .0
                .checked_add(receipt.price.total_sats)
                .ok_or(ProtocolError::CostOverflow)?;
            balance.1 = balance
                .1
                .checked_add(1)
                .ok_or(ProtocolError::CostOverflow)?;
        }
        if disputed_set.iter().any(|id| !seen.contains(id)) {
            return Err(ProtocolError::InvalidSettlement);
        }
        included.sort();
        if included.is_empty() {
            return Err(ProtocolError::InvalidSettlement);
        }
        let receipt_merkle_root = settlement_receipt_root(&project, &included)?;
        let mut disputed_receipt_ids: Vec<_> = disputed_set.into_iter().collect();
        disputed_receipt_ids.sort();
        let balances = balances
            .into_iter()
            .map(
                |((provider, node_owner_id), (earned_sats, receipt_count))| ProviderBalance {
                    provider,
                    node_owner_id,
                    earned_sats,
                    receipt_count,
                },
            )
            .collect();
        let settlement = Self {
            project,
            period_start,
            period_end,
            receipt_ids: included,
            disputed_receipt_ids,
            receipt_merkle_root,
            balances,
            total_sats: total,
        };
        let _ = settlement.canonical_bytes()?;
        Ok(settlement)
    }

    /// Returns versioned canonical bytes.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        validate_project(&self.project)?;
        if self.period_end <= self.period_start
            || self.receipt_ids.is_empty()
            || self.receipt_ids.len() > MAX_SETTLEMENT_RECEIPTS
        {
            return Err(ProtocolError::InvalidSettlement);
        }
        let receipt_ids = strict_digest_set(&self.receipt_ids)?;
        let disputed_ids = strict_digest_set(&self.disputed_receipt_ids)?;
        if receipt_ids.iter().any(|id| disputed_ids.contains(id)) {
            return Err(ProtocolError::InvalidSettlement);
        }
        if self.receipt_merkle_root != settlement_receipt_root(&self.project, &self.receipt_ids)? {
            return Err(ProtocolError::InvalidSettlement);
        }
        let mut balances = self.balances.clone();
        balances.sort_by(|left, right| {
            (&left.provider, &left.node_owner_id).cmp(&(&right.provider, &right.node_owner_id))
        });
        if balances.windows(2).any(|pair| {
            pair[0].provider == pair[1].provider && pair[0].node_owner_id == pair[1].node_owner_id
        }) {
            return Err(ProtocolError::InvalidSettlement);
        }
        let derived_total = balances.iter().try_fold(0u64, |sum, balance| {
            validate_text("provider", &balance.provider)?;
            validate_text("node owner id", &balance.node_owner_id)?;
            if balance.receipt_count == 0 {
                return Err(ProtocolError::InvalidSettlement);
            }
            sum.checked_add(balance.earned_sats)
                .ok_or(ProtocolError::CostOverflow)
        })?;
        if derived_total != self.total_sats
            || balances.iter().try_fold(0u64, |sum, balance| {
                sum.checked_add(balance.receipt_count)
                    .ok_or(ProtocolError::CostOverflow)
            })? != u64::try_from(receipt_ids.len()).map_err(|_| ProtocolError::LengthOverflow)?
        {
            return Err(ProtocolError::InvalidSettlement);
        }
        let mut out = Vec::new();
        out.extend_from_slice(SETTLEMENT_DOMAIN);
        put_text(&mut out, &self.project)?;
        put_i64(&mut out, self.period_start);
        put_i64(&mut out, self.period_end);
        put_digest_set(&mut out, &receipt_ids)?;
        put_digest_set(&mut out, &disputed_ids)?;
        out.extend_from_slice(&self.receipt_merkle_root);
        put_u32(&mut out, checked_len(balances.len())?);
        for balance in balances {
            put_text(&mut out, &balance.provider)?;
            put_text(&mut out, &balance.node_owner_id)?;
            put_u64(&mut out, balance.earned_sats);
            put_u64(&mut out, balance.receipt_count);
        }
        put_u64(&mut out, self.total_sats);
        Ok(out)
    }

    /// Computes the deterministic settlement ID.
    pub fn id(&self) -> Result<Digest32, ProtocolError> {
        Ok(hash(&self.canonical_bytes()?))
    }
}

/// Protocol validation and arithmetic errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// Required text was empty or contained NUL.
    #[error("invalid required text field: {0}")]
    InvalidText(&'static str),
    /// A project is not a full kind-30621 coordinate.
    #[error("invalid project coordinate")]
    InvalidProject,
    /// A length exceeds the protocol encoding.
    #[error("protocol field length overflow")]
    LengthOverflow,
    /// Capability data is incomplete or internally inconsistent.
    #[error("invalid node capability")]
    InvalidCapability,
    /// Pricing contains duplicate, empty, or invalid rates.
    #[error("invalid pricing policy")]
    InvalidPricing,
    /// Too many independent meter dimensions were supplied.
    #[error("too many meter dimensions")]
    TooManyMeters,
    /// Metering contains duplicate dimensions or invalid sources.
    #[error("invalid metered usage")]
    InvalidUsage,
    /// Integer price calculation overflowed.
    #[error("compute cost overflow")]
    CostOverflow,
    /// Job expiration is not after job creation.
    #[error("invalid compute job window")]
    InvalidJobWindow,
    /// Receipt does not bind the exact signed job and policy.
    #[error("compute receipt binding mismatch")]
    ReceiptBindingMismatch,
    /// Receipt timestamps are inconsistent with the job.
    #[error("invalid compute receipt window")]
    InvalidReceiptWindow,
    /// Receipt price differs from the deterministic policy calculation.
    #[error("compute receipt price mismatch or exceeds authorization")]
    PriceMismatch,
    /// Settlement contains duplicates, cross-project state, or inconsistent totals.
    #[error("invalid compute settlement")]
    InvalidSettlement,
}

/// Computes the project-scoped deterministic Merkle root for a strict receipt set.
pub fn settlement_receipt_root(
    project: &str,
    receipt_ids: &[Digest32],
) -> Result<Digest32, ProtocolError> {
    validate_project(project)?;
    if receipt_ids.is_empty() || receipt_ids.len() > MAX_SETTLEMENT_RECEIPTS {
        return Err(ProtocolError::InvalidSettlement);
    }
    let ids = strict_digest_set(receipt_ids)?;
    let mut level: Vec<Digest32> = ids
        .into_iter()
        .map(|id| {
            let mut bytes = Vec::from(SETTLEMENT_LEAF_DOMAIN);
            put_text(&mut bytes, project)?;
            bytes.extend_from_slice(&id);
            Ok(hash(&bytes))
        })
        .collect::<Result<_, ProtocolError>>()?;
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            let left = pair[0];
            let right = pair.get(1).copied().unwrap_or(left);
            let mut bytes = Vec::from(SETTLEMENT_NODE_DOMAIN);
            bytes.extend_from_slice(&left);
            bytes.extend_from_slice(&right);
            next.push(hash(&bytes));
        }
        level = next;
    }
    level.pop().ok_or(ProtocolError::InvalidSettlement)
}

#[cfg(test)]
mod tests;
