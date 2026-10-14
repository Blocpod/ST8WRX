use super::*;
use buzz_core::channel::{ChannelType, ChannelVisibility, MemberRole};
use buzz_db::workflow::{ApprovalStatus, RunStatus};
use buzz_workflow::action_sink::WorkflowSideEffect;

async fn fixture() -> (Arc<AppState>, CommunityId, Uuid, nostr::Keys) {
    let url = std::env::var("BUZZ_WORKFLOW_TEST_DATABASE_URL")
        .expect("set isolated workflow test DB URL");
    assert!(
        url.contains("workflow"),
        "use a dedicated workflow database"
    );
    let state = super::integration_tests::test_state().await;
    state.db.migrate().await.expect("migrate");
    let owner = nostr::Keys::generate();
    let host = format!("workflow-ready-{}.example", Uuid::new_v4());
    let community = match state
        .db
        .create_community_with_owner(&host, &owner.public_key().to_hex())
        .await
        .unwrap()
    {
        buzz_db::CreateCommunityWithOwnerResult::Created(record) => record.id,
        other => panic!("new community: {other:?}"),
    };
    state
        .db
        .ensure_user(community, &owner.public_key().to_bytes())
        .await
        .unwrap();
    let channel = state
        .db
        .create_channel(
            community,
            "workflow",
            ChannelType::Stream,
            ChannelVisibility::Private,
            None,
            &owner.public_key().to_bytes(),
            None,
        )
        .await
        .unwrap();
    state
        .workflow_engine
        .set_action_sink(Arc::new(RelayActionSink::new(&state)));
    (state, community, channel.id, owner)
}

#[tokio::test]
#[ignore = "requires isolated BUZZ_WORKFLOW_TEST_DATABASE_URL"]
async fn workflow_readiness_side_effects_preserve_authority_and_channel_scope() {
    let (state, community, channel, owner) = fixture().await;
    let sink = RelayActionSink::new(&state);
    let author = owner.public_key().to_hex();
    let member = nostr::Keys::generate();
    state
        .db
        .add_member(
            community,
            channel,
            &member.public_key().to_bytes(),
            MemberRole::Member,
            Some(&owner.public_key().to_bytes()),
        )
        .await
        .unwrap();
    state
        .db
        .add_relay_member(
            community,
            &member.public_key().to_hex(),
            "member",
            Some(&author),
        )
        .await
        .unwrap();
    let effect = WorkflowSideEffect::SetChannelTopic {
        topic: "ready".into(),
    };
    assert!(sink
        .perform(
            community,
            &channel.to_string(),
            &member.public_key().to_hex(),
            effect.clone()
        )
        .await
        .is_err());
    sink.perform(community, &channel.to_string(), &author, effect)
        .await
        .unwrap();
    assert_eq!(
        state
            .db
            .get_channel(community, channel)
            .await
            .unwrap()
            .topic
            .as_deref(),
        Some("ready")
    );
    let message = sink
        .send_message(community, &channel.to_string(), "source", &author, None)
        .await
        .unwrap();
    let reaction = sink
        .perform(
            community,
            &channel.to_string(),
            &author,
            WorkflowSideEffect::AddReaction {
                message_id: message.clone(),
                emoji: "👍".into(),
            },
        )
        .await
        .unwrap();
    let stored = state
        .db
        .get_event_by_id(community, &hex::decode(reaction).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.event.kind.as_u16(), 7);
    let source = state
        .db
        .get_event_by_id(community, &hex::decode(&message).unwrap())
        .await
        .unwrap()
        .unwrap();
    let reaction_rows = state
        .db
        .get_reactions(
            community,
            source.event.id.as_bytes(),
            chrono::DateTime::from_timestamp(source.event.created_at.as_secs() as i64, 0).unwrap(),
            100,
            None,
        )
        .await
        .unwrap();
    assert_eq!(reaction_rows.len(), 1);
    let duplicate = sink
        .perform(
            community,
            &channel.to_string(),
            &author,
            WorkflowSideEffect::AddReaction {
                message_id: message.clone(),
                emoji: "👍".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(duplicate, stored.event.id.to_hex());

    let other = state
        .db
        .create_channel(
            community,
            "other",
            ChannelType::Stream,
            ChannelVisibility::Private,
            None,
            &owner.public_key().to_bytes(),
            None,
        )
        .await
        .unwrap();
    assert!(sink
        .perform(
            community,
            &other.id.to_string(),
            &author,
            WorkflowSideEffect::AddReaction {
                message_id: message,
                emoji: "👍".into()
            }
        )
        .await
        .is_err());
    let dm = sink
        .perform(
            community,
            &channel.to_string(),
            &author,
            WorkflowSideEffect::SendDm {
                to: member.public_key().to_hex(),
                text: "private".into(),
            },
        )
        .await
        .unwrap();
    let dm = state
        .db
        .get_event_by_id(community, &hex::decode(dm).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(dm.channel_id, Some(channel));
    assert_eq!(
        state
            .db
            .get_members(community, dm.channel_id.unwrap())
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
#[ignore = "requires isolated BUZZ_WORKFLOW_TEST_DATABASE_URL"]
async fn workflow_readiness_approval_recovers_saved_definition_once() {
    let (state, community, channel, owner) = fixture().await;
    let (def, json) = buzz_workflow::WorkflowEngine::parse_yaml("name: gate\ntrigger:\n  on: message_posted\nsteps:\n  - id: approve\n    action: request_approval\n    from: any\n    message: Approve\n  - id: change\n    action: set_channel_topic\n    topic: approved-original\n").unwrap();
    let workflow = state
        .db
        .create_workflow(
            community,
            Some(channel),
            &owner.public_key().to_bytes(),
            "gate",
            &json,
            &[1; 32],
        )
        .await
        .unwrap();
    let trigger = buzz_workflow::executor::TriggerContext {
        channel_id: channel.to_string(),
        ..Default::default()
    };
    let run = state
        .db
        .create_workflow_run(
            community,
            workflow,
            None,
            Some(&serde_json::to_value(&trigger).unwrap()),
        )
        .await
        .unwrap();
    let result = buzz_workflow::executor::execute_run(
        &state.workflow_engine,
        community,
        run,
        &def,
        &trigger,
    )
    .await;
    state
        .workflow_engine
        .finalize_run(community, run, result, None)
        .await;
    assert_eq!(
        state
            .db
            .get_workflow_run(community, run)
            .await
            .unwrap()
            .status,
        RunStatus::WaitingApproval
    );
    let approvals = state
        .db
        .get_run_approvals(community, workflow, run)
        .await
        .unwrap();
    assert_eq!(approvals.len(), 1);
    state
        .db
        .update_workflow(
            community,
            workflow,
            "changed",
            &json.replace("approved-original", "changed-after-approval"),
            &[3; 32],
        )
        .await
        .unwrap();
    let mut decision = state.db.begin_transaction().await.unwrap();
    assert!(buzz_db::workflow::decide_approval_in_transaction(
        &mut decision,
        community,
        &approvals[0].token,
        true,
        &owner.public_key().to_bytes(),
        None
    )
    .await
    .unwrap());
    decision.rollback().await.unwrap();
    assert_eq!(
        state
            .db
            .get_approval_by_stored_hash(community, &approvals[0].token)
            .await
            .unwrap()
            .status,
        ApprovalStatus::Pending
    );
    // Only update the saved decision: emulate crash before the command's resume task.
    state
        .db
        .update_approval_by_stored_hash(
            community,
            &approvals[0].token,
            ApprovalStatus::Granted,
            Some(&owner.public_key().to_bytes()),
            None,
        )
        .await
        .unwrap();
    assert!(buzz_db::workflow::pending_approval_transitions(&state.db)
        .await
        .unwrap()
        .contains(&(community, run)));
    let (first, second) = tokio::join!(
        state.workflow_engine.reconcile_approval(community, run),
        state.workflow_engine.reconcile_approval(community, run)
    );
    first.unwrap();
    second.unwrap();
    assert_eq!(
        state
            .db
            .get_workflow_run(community, run)
            .await
            .unwrap()
            .status,
        RunStatus::Completed
    );
    assert_eq!(
        state
            .db
            .get_channel(community, channel)
            .await
            .unwrap()
            .topic
            .as_deref(),
        Some("approved-original")
    );
    assert!(!buzz_db::workflow::pending_approval_transitions(&state.db)
        .await
        .unwrap()
        .contains(&(community, run)));
}

#[tokio::test]
#[ignore = "requires isolated BUZZ_WORKFLOW_TEST_DATABASE_URL"]
async fn workflow_readiness_checkpoint_rollback_and_timeout_recovery() {
    let (state, community, channel, owner) = fixture().await;
    let workflow = state
        .db
        .create_workflow(
            community,
            Some(channel),
            &owner.public_key().to_bytes(),
            "rollback",
            "{}",
            &[2; 32],
        )
        .await
        .unwrap();
    let mut runs = Vec::new();
    for _ in 0..2 {
        let run = state
            .db
            .create_workflow_run(community, workflow, None, None)
            .await
            .unwrap();
        state
            .db
            .update_workflow_run(
                community,
                run,
                RunStatus::Running,
                0,
                &serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
        runs.push(run);
    }
    for (index, run) in runs.iter().enumerate() {
        let result = buzz_db::workflow::suspend_approval(
            &state.db,
            buzz_db::workflow::CreateApprovalParams {
                community_id: community,
                token: "same-token",
                workflow_id: workflow,
                run_id: *run,
                step_id: "gate",
                step_index: 0,
                approver_spec: "any",
                expires_at: Utc::now() - chrono::Duration::seconds(1),
            },
            &serde_json::json!([]),
        )
        .await;
        assert_eq!(result.is_ok(), index == 0);
    }
    assert_eq!(
        state
            .db
            .get_workflow_run(community, runs[1])
            .await
            .unwrap()
            .status,
        RunStatus::Running
    );
    let approvals = state
        .db
        .get_run_approvals(community, workflow, runs[0])
        .await
        .unwrap();
    assert!(!state
        .db
        .update_approval_by_stored_hash(
            community,
            &approvals[0].token,
            ApprovalStatus::Granted,
            Some(&owner.public_key().to_bytes()),
            None
        )
        .await
        .unwrap());
    state
        .workflow_engine
        .reconcile_approval(community, runs[0])
        .await
        .unwrap();
    assert_eq!(
        state
            .db
            .get_workflow_run(community, runs[0])
            .await
            .unwrap()
            .status,
        RunStatus::Cancelled
    );
}

#[tokio::test]
#[ignore = "requires isolated BUZZ_WORKFLOW_TEST_DATABASE_URL"]
async fn workflow_readiness_approval_denial_recovers_without_execution() {
    let (state, community, channel, owner) = fixture().await;
    let (def, json) = buzz_workflow::WorkflowEngine::parse_yaml("name: deny\ntrigger:\n  on: message_posted\nsteps:\n  - id: approve\n    action: request_approval\n    from: any\n    message: Approve\n  - id: change\n    action: set_channel_topic\n    topic: must-not-run\n").unwrap();
    let workflow = state
        .db
        .create_workflow(
            community,
            Some(channel),
            &owner.public_key().to_bytes(),
            "deny",
            &json,
            &[4; 32],
        )
        .await
        .unwrap();
    let trigger = buzz_workflow::executor::TriggerContext {
        channel_id: channel.to_string(),
        ..Default::default()
    };
    let run = state
        .db
        .create_workflow_run(
            community,
            workflow,
            None,
            Some(&serde_json::to_value(&trigger).unwrap()),
        )
        .await
        .unwrap();
    let result = buzz_workflow::executor::execute_run(
        &state.workflow_engine,
        community,
        run,
        &def,
        &trigger,
    )
    .await;
    state
        .workflow_engine
        .finalize_run(community, run, result, None)
        .await;
    let approvals = state
        .db
        .get_run_approvals(community, workflow, run)
        .await
        .unwrap();
    state
        .db
        .update_approval_by_stored_hash(
            community,
            &approvals[0].token,
            ApprovalStatus::Denied,
            Some(&owner.public_key().to_bytes()),
            None,
        )
        .await
        .unwrap();
    state
        .workflow_engine
        .reconcile_approval(community, run)
        .await
        .unwrap();
    assert_eq!(
        state
            .db
            .get_workflow_run(community, run)
            .await
            .unwrap()
            .status,
        RunStatus::Cancelled
    );
    assert_ne!(
        state
            .db
            .get_channel(community, channel)
            .await
            .unwrap()
            .topic
            .as_deref(),
        Some("must-not-run")
    );
}

struct OfflineSink;
impl ActionSink for OfflineSink {
    fn send_message(
        &self,
        _: CommunityId,
        _: &str,
        _: &str,
        _: &str,
        _: Option<&str>,
    ) -> Pin<Box<dyn Future<Output = Result<String, ActionSinkError>> + Send + '_>> {
        Box::pin(async {
            Err(ActionSinkError::Database(
                "offline notification sink".into(),
            ))
        })
    }
}

#[tokio::test]
#[ignore = "requires isolated BUZZ_WORKFLOW_TEST_DATABASE_URL"]
async fn workflow_readiness_missing_notification_recovers_same_event_id() {
    let (state, community, channel, owner) = fixture().await;
    let (def, json) = buzz_workflow::WorkflowEngine::parse_yaml("name: notify\ntrigger:\n  on: message_posted\nsteps:\n  - id: approve\n    action: request_approval\n    from: any\n    message: Approve\n").unwrap();
    let workflow = state
        .db
        .create_workflow(
            community,
            Some(channel),
            &owner.public_key().to_bytes(),
            "notify",
            &json,
            &[5; 32],
        )
        .await
        .unwrap();
    let trigger = buzz_workflow::executor::TriggerContext {
        channel_id: channel.to_string(),
        ..Default::default()
    };
    let run = state
        .db
        .create_workflow_run(
            community,
            workflow,
            None,
            Some(&serde_json::to_value(&trigger).unwrap()),
        )
        .await
        .unwrap();
    let offline = buzz_workflow::WorkflowEngine::new(
        state.db.clone(),
        buzz_workflow::WorkflowConfig::default(),
    );
    offline.set_action_sink(Arc::new(OfflineSink));
    let result =
        buzz_workflow::executor::execute_run(&offline, community, run, &def, &trigger).await;
    offline.finalize_run(community, run, result, None).await;
    let waiting = state.db.get_workflow_run(community, run).await.unwrap();
    assert_eq!(waiting.status, RunStatus::WaitingApproval);
    assert!(waiting.execution_trace[0]
        .get("notification_event_id")
        .is_none());
    assert!(buzz_db::workflow::pending_approval_transitions(&state.db)
        .await
        .unwrap()
        .contains(&(community, run)));
    state
        .workflow_engine
        .reconcile_approval(community, run)
        .await
        .unwrap();
    let notified = state.db.get_workflow_run(community, run).await.unwrap();
    let id = notified.execution_trace[0]["notification_event_id"]
        .as_str()
        .unwrap();
    assert!(state
        .db
        .get_event_by_id(community, &hex::decode(id).unwrap())
        .await
        .unwrap()
        .is_some());
    state
        .workflow_engine
        .reconcile_approval(community, run)
        .await
        .unwrap();
    let repeated = state.db.get_workflow_run(community, run).await.unwrap();
    assert_eq!(repeated.execution_trace[0]["notification_event_id"], id);
    assert!(!buzz_db::workflow::pending_approval_transitions(&state.db)
        .await
        .unwrap()
        .contains(&(community, run)));
}

#[tokio::test]
#[ignore = "requires isolated BUZZ_WORKFLOW_TEST_DATABASE_URL"]
async fn workflow_readiness_approval_commands_bind_member_and_channel() {
    use crate::handlers::{command_executor::handle_command, ingest::IngestAuth};
    let (state, community, channel, owner) = fixture().await;
    let (def, json) = buzz_workflow::WorkflowEngine::parse_yaml("name: command\ntrigger:\n  on: message_posted\nsteps:\n  - id: approve\n    action: request_approval\n    from: any\n    message: Approve\n").unwrap();
    let workflow = state
        .db
        .create_workflow(
            community,
            Some(channel),
            &owner.public_key().to_bytes(),
            "command",
            &json,
            &[6; 32],
        )
        .await
        .unwrap();
    let trigger = buzz_workflow::executor::TriggerContext {
        channel_id: channel.to_string(),
        ..Default::default()
    };
    let run = state
        .db
        .create_workflow_run(
            community,
            workflow,
            None,
            Some(&serde_json::to_value(&trigger).unwrap()),
        )
        .await
        .unwrap();
    let result = buzz_workflow::executor::execute_run(
        &state.workflow_engine,
        community,
        run,
        &def,
        &trigger,
    )
    .await;
    state
        .workflow_engine
        .finalize_run(community, run, result, None)
        .await;
    let approval = state
        .db
        .get_run_approvals(community, workflow, run)
        .await
        .unwrap()
        .remove(0);
    let reference = hex::encode(&approval.token);
    let tenant = buzz_core::tenant::TenantContext::resolved(
        community,
        state
            .db
            .lookup_community_host(community)
            .await
            .unwrap()
            .unwrap(),
    );
    let auth = |keys: &nostr::Keys| IngestAuth::Nip42 {
        pubkey: keys.public_key(),
        scopes: vec![buzz_auth::Scope::MessagesWrite],
        channel_ids: None,
        conn_id: Uuid::new_v4(),
    };
    let make = |keys: &nostr::Keys, target: Uuid| {
        EventBuilder::new(
            Kind::Custom(buzz_core::kind::KIND_APPROVAL_GRANT as u16),
            "approved",
        )
        .tags([
            Tag::parse(["d", reference.as_str()]).unwrap(),
            Tag::parse(["h", target.to_string().as_str()]).unwrap(),
        ])
        .sign_with_keys(keys)
        .unwrap()
    };
    let outsider = nostr::Keys::generate();
    let rejected = make(&outsider, channel);
    assert!(
        handle_command(&tenant, &state, rejected.clone(), auth(&outsider))
            .await
            .is_err()
    );
    assert!(state
        .db
        .get_event_by_id(community, rejected.id.as_bytes())
        .await
        .unwrap()
        .is_none());
    assert!(
        handle_command(&tenant, &state, make(&owner, Uuid::new_v4()), auth(&owner))
            .await
            .is_err()
    );
    let accepted = make(&owner, channel);
    assert!(
        handle_command(&tenant, &state, accepted.clone(), auth(&owner))
            .await
            .unwrap()
            .accepted
    );
    assert_eq!(
        state
            .db
            .get_approval_by_stored_hash(community, &approval.token)
            .await
            .unwrap()
            .status,
        ApprovalStatus::Granted
    );
    assert_eq!(
        state
            .db
            .get_event_by_id(community, accepted.id.as_bytes())
            .await
            .unwrap()
            .unwrap()
            .channel_id,
        Some(channel)
    );
}
