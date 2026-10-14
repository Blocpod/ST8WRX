//! Approval checkpoints retain the exact definition used before suspension.
use super::*;

impl WorkflowEngine {
    pub(crate) async fn suspend_run(
        &self,
        community: CommunityId,
        run_id: Uuid,
        step_index: usize,
        trace: &serde_json::Value,
        token: &str,
    ) -> Result<(), WorkflowError> {
        let checkpoint = trace
            .as_array()
            .and_then(|trace| trace.last())
            .ok_or_else(|| {
                WorkflowError::InvalidDefinition("approval checkpoint is missing".into())
            })?;
        let action: ActionDef = serde_json::from_value(checkpoint["approval_action"].clone())
            .map_err(|e| WorkflowError::InvalidDefinition(e.to_string()))?;
        let ActionDef::RequestApproval {
            from,
            message,
            timeout,
        } = action
        else {
            return Err(WorkflowError::InvalidDefinition(
                "approval checkpoint has wrong action".into(),
            ));
        };
        let seconds = executor::parse_duration_secs(timeout.as_deref().unwrap_or("24h"))?;
        let expires = Utc::now()
            .checked_add_signed(chrono::Duration::seconds(
                i64::try_from(seconds)
                    .map_err(|e| WorkflowError::InvalidDefinition(e.to_string()))?,
            ))
            .ok_or_else(|| {
                WorkflowError::InvalidDefinition("approval expiration overflow".into())
            })?;
        let run = self.db.get_workflow_run(community, run_id).await?;
        let workflow = self.db.get_workflow(community, run.workflow_id).await?;
        let channel = workflow.channel_id.ok_or_else(|| {
            WorkflowError::InvalidDefinition("approval requires a workflow channel".into())
        })?;
        let sink = self.action_sink()?;
        let hash = buzz_db::workflow::suspend_approval(
            &self.db,
            buzz_db::workflow::CreateApprovalParams {
                community_id: community,
                token,
                workflow_id: workflow.id,
                run_id,
                step_id: checkpoint["step_id"].as_str().ok_or_else(|| {
                    WorkflowError::InvalidDefinition("approval step has no id".into())
                })?,
                step_index: i32::try_from(step_index)
                    .map_err(|e| WorkflowError::InvalidDefinition(e.to_string()))?,
                approver_spec: &from,
                expires_at: expires,
            },
            trace,
        )
        .await?;
        let approval = match self.db.get_approval_by_stored_hash(community, &hash).await {
            Ok(approval) => approval,
            Err(error) => {
                tracing::warn!(%run_id, %error, "Approval saved; notification will be recovered");
                return Ok(());
            }
        };
        let effect = action_sink::WorkflowSideEffect::ApprovalRequested {
            token_hash: hex::encode(hash),
            approver: from,
            message,
            run_id: run_id.to_string(),
            created_at: approval.created_at.timestamp().try_into().map_err(
                |error: std::num::TryFromIntError| {
                    WorkflowError::InvalidDefinition(error.to_string())
                },
            )?,
        };
        match sink
            .perform(
                community,
                &channel.to_string(),
                &hex::encode(workflow.owner_pubkey),
                effect,
            )
            .await
        {
            Ok(event_id) => {
                if let Err(error) = buzz_db::workflow::mark_approval_notified(
                    &self.db,
                    community,
                    run_id,
                    step_index as i32,
                    &event_id,
                )
                .await
                {
                    tracing::warn!(%run_id, %error, "Notification checkpoint failed; will retry");
                }
            }
            Err(error) => {
                tracing::warn!(%run_id, %error, "Approval checkpoint saved; notification delivery failed")
            }
        }
        Ok(())
    }
}

impl WorkflowEngine {
    /// Apply a saved approval decision; safe to call again after a relay restart.
    pub async fn reconcile_approval(
        &self,
        community: CommunityId,
        run_id: Uuid,
    ) -> Result<(), WorkflowError> {
        use buzz_db::workflow::ApprovalStatus;
        let Ok(_local_guard) = self.approval_transitions.try_lock() else {
            return Ok(());
        };
        let Some(_lock) =
            buzz_db::workflow::lock_approval_transition(&self.db, community, run_id).await?
        else {
            return Ok(());
        };
        let run = self.db.get_workflow_run(community, run_id).await?;
        if run.status != RunStatus::WaitingApproval {
            return Ok(());
        }
        let approvals = self
            .db
            .get_run_approvals(community, run.workflow_id, run_id)
            .await?;
        let Some(approval) = approvals
            .iter()
            .find(|approval| approval.step_index == run.current_step)
        else {
            return Ok(());
        };
        if approval.status == ApprovalStatus::Pending && approval.expires_at > Utc::now() {
            let checkpoint = run
                .execution_trace
                .as_array()
                .and_then(|trace| trace.last())
                .ok_or_else(|| {
                    WorkflowError::InvalidDefinition("approval has no checkpoint".into())
                })?;
            let action: ActionDef =
                serde_json::from_value(checkpoint["approval_action"].clone())
                    .map_err(|error| WorkflowError::InvalidDefinition(error.to_string()))?;
            let ActionDef::RequestApproval { from, message, .. } = action else {
                return Err(WorkflowError::InvalidDefinition(
                    "wrong approval checkpoint action".into(),
                ));
            };
            let workflow = self.db.get_workflow(community, run.workflow_id).await?;
            let channel = workflow.channel_id.ok_or_else(|| {
                WorkflowError::InvalidDefinition("approval has no channel".into())
            })?;
            let event_id = self
                .action_sink()?
                .perform(
                    community,
                    &channel.to_string(),
                    &hex::encode(workflow.owner_pubkey),
                    action_sink::WorkflowSideEffect::ApprovalRequested {
                        token_hash: hex::encode(&approval.token),
                        approver: from,
                        message,
                        run_id: run_id.to_string(),
                        created_at: approval.created_at.timestamp().try_into().map_err(
                            |error: std::num::TryFromIntError| {
                                WorkflowError::InvalidDefinition(error.to_string())
                            },
                        )?,
                    },
                )
                .await?;
            buzz_db::workflow::mark_approval_notified(
                &self.db,
                community,
                run_id,
                run.current_step,
                &event_id,
            )
            .await?;
            return Ok(());
        }
        if approval.status != ApprovalStatus::Granted {
            buzz_db::workflow::cancel_approval_transition(
                &self.db,
                community,
                run_id,
                run.current_step,
                &approval.token,
                approval.status != ApprovalStatus::Denied,
            )
            .await?;
            return Ok(());
        }
        let snapshot = run
            .execution_trace
            .as_array()
            .and_then(|trace| {
                trace
                    .iter()
                    .rev()
                    .find_map(|entry| entry.get("workflow_snapshot"))
            })
            .cloned()
            .ok_or_else(|| {
                WorkflowError::InvalidDefinition("approval has no saved workflow definition".into())
            })?;
        let def: WorkflowDef = serde_json::from_value(snapshot)
            .map_err(|error| WorkflowError::InvalidDefinition(error.to_string()))?;
        let workflow = self.db.get_workflow(community, run.workflow_id).await?;
        let channel = workflow
            .channel_id
            .ok_or_else(|| WorkflowError::InvalidDefinition("approval has no channel".into()))?;
        if let Err(error) = self
            .check_owner_authority(community, channel, &workflow.owner_pubkey, &def)
            .await
        {
            self.db
                .update_workflow_run(
                    community,
                    run_id,
                    RunStatus::Failed,
                    run.current_step,
                    &run.execution_trace,
                    Some(buzz_db::workflow::WorkflowRunFailure {
                        code: "unauthorized",
                        message: &error.to_string(),
                    }),
                )
                .await?;
            return Ok(());
        }
        let mut trace = run.execution_trace.as_array().cloned().unwrap_or_default();
        if let Some(checkpoint) = trace.last_mut() {
            checkpoint["status"] = serde_json::json!("completed");
            checkpoint["output"] = serde_json::json!({"approved": true});
        }
        let outputs = trace
            .iter()
            .filter_map(|entry| {
                Some((
                    entry.get("step_id")?.as_str()?.to_owned(),
                    entry.get("output")?.clone(),
                ))
            })
            .collect();
        let trigger =
            serde_json::from_value(run.trigger_context.clone().unwrap_or(serde_json::json!({})))
                .map_err(|error| WorkflowError::InvalidDefinition(error.to_string()))?;
        let next = usize::try_from(run.current_step)
            .map_err(|error| WorkflowError::InvalidDefinition(error.to_string()))?
            + 1;
        let result = executor::execute_from_step(
            self,
            community,
            run_id,
            &def,
            &trigger,
            next,
            Some(outputs),
        )
        .await;
        if matches!(&result, Err((WorkflowError::CapacityExceeded, _))) {
            return Ok(());
        }
        self.finalize_run(community, run_id, result, Some(trace))
            .await;
        Ok(())
    }

    pub(crate) async fn reconcile_approvals(self: &Arc<Self>) {
        match buzz_db::workflow::pending_approval_transitions(&self.db).await {
            Ok(runs) => {
                for (community, run) in runs {
                    let engine = Arc::clone(self);
                    tokio::spawn(async move {
                        if let Err(error) = engine.reconcile_approval(community, run).await {
                            tracing::error!(%run, %error, "Approval recovery failed; will retry");
                        }
                    });
                }
            }
            Err(error) => tracing::error!(%error, "Failed to list approval recovery work"),
        }
    }
}
