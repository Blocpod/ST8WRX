use super::*;
use buzz_core::kind::KIND_ST8_COMPUTE_RESOLUTION;

/// Final project-owner arbitration; the original metering and price never change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeResolutionOutcome {
    /// Reject the dispute and release the full original charge for settlement.
    UpholdCharge,
    /// Uphold the dispute and permanently exclude the charge from settlement.
    VoidCharge,
}

/// An owner-signed, final decision bound to one exact signed dispute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeResolution {
    /// Kind-30621 project coordinate whose owner arbitrates compute charges.
    pub project: String,
    /// Exact disputed receipt ID.
    pub receipt_id: Digest32,
    /// Exact signed dispute event ID.
    pub dispute_event_id: Digest32,
    /// Release the original charge or permanently void it.
    pub outcome: ComputeResolutionOutcome,
    /// Unix timestamp of the decision.
    pub resolved_at: i64,
    /// Public reason explaining the owner's decision.
    pub reason: String,
}

impl ComputeResolution {
    /// Builds the owner-signable event, binding the project, receipt, and dispute.
    pub fn event_builder(&self) -> Result<EventBuilder, EngineError> {
        if self.project.trim().is_empty() || self.reason.trim().is_empty() || self.resolved_at < 0 {
            return Err(EngineError::InvalidDispute);
        }
        let receipt = hex::encode(self.receipt_id);
        let dispute = hex::encode(self.dispute_event_id);
        Ok(EventBuilder::new(
            Kind::Custom(KIND_ST8_COMPUTE_RESOLUTION as u16),
            serde_json::to_string(self)?,
        )
        .tags([
            tag(["a", self.project.as_str()])?,
            tag(["d", receipt.as_str()])?,
            tag(["e", dispute.as_str()])?,
            tag(["st8-receipt", receipt.as_str()])?,
        ]))
    }

    /// Verifies owner authority and the exact dispute/receipt linkage.
    pub fn from_event(
        project: &BuzzProjectContext,
        receipt: &VerifiedComputeReceipt,
        dispute_event: &Event,
        event: &Event,
    ) -> Result<Self, EngineError> {
        project.verify()?;
        receipt.verify()?;
        let dispute = ComputeDispute::from_event(project, receipt, dispute_event)?;
        verify_signed_event(event)?;
        let resolution: Self = serde_json::from_str(&event.content)?;
        let _ = resolution.event_builder()?;
        if u32::from(event.kind.as_u16()) != KIND_ST8_COMPUTE_RESOLUTION
            || event.pubkey != project.project_event.pubkey
            || resolution.project != project.project
            || resolution.receipt_id != dispute.receipt_id
            || resolution.dispute_event_id != event_digest(dispute_event)
            || resolution.resolved_at < dispute.disputed_at
            || single_tag(event, "a") != Some(project.project.as_str())
            || single_tag(event, "d") != Some(hex::encode(resolution.receipt_id).as_str())
            || single_tag(event, "e") != Some(dispute_event.id.to_hex().as_str())
            || single_tag(event, "st8-receipt") != Some(hex::encode(resolution.receipt_id).as_str())
        {
            return Err(EngineError::EventBindingMismatch);
        }
        Ok(resolution)
    }
}
