use super::*;
use crate::handlers::side_effects::{emit_group_discovery_events, emit_membership_notification};
use buzz_core::kind::{
    KIND_MEMBER_ADDED_NOTIFICATION, KIND_REACTION, KIND_WORKFLOW_APPROVAL_REQUESTED,
};
use buzz_workflow::action_sink::WorkflowSideEffect;

pub(super) async fn perform(
    sink: &RelayActionSink,
    community: CommunityId,
    channel_id: &str,
    author: &str,
    effect: WorkflowSideEffect,
) -> Result<String, ActionSinkError> {
    let state = sink
        .state
        .upgrade()
        .ok_or_else(|| ActionSinkError::Database("relay is shutting down".into()))?;
    let channel_id = Uuid::parse_str(channel_id).map_err(invalid)?;
    let owner = nostr::PublicKey::from_hex(author).map_err(invalid)?;
    let channel = state
        .db
        .get_channel(community, channel_id)
        .await
        .map_err(database)?;
    if channel.archived_at.is_some() {
        return Err(ActionSinkError::ChannelArchived(channel_id.to_string()));
    }
    if !state
        .db
        .is_member(community, channel_id, &owner.to_bytes())
        .await
        .map_err(database)?
    {
        return Err(ActionSinkError::InvalidInput(
            "workflow owner is no longer a channel member".into(),
        ));
    }
    let host = state
        .db
        .lookup_community_host(community)
        .await
        .map_err(database)?
        .ok_or_else(|| ActionSinkError::Database("community has no host".into()))?;
    let tenant = buzz_core::tenant::TenantContext::resolved(community, host);
    match effect {
        WorkflowSideEffect::SendDm { to, text } => {
            if text.trim().is_empty() {
                return Err(ActionSinkError::EmptyContent);
            }
            let recipient = nostr::PublicKey::from_hex(&to).map_err(invalid)?;
            if !state
                .db
                .is_relay_member(community, &recipient.to_hex())
                .await
                .map_err(database)?
            {
                return Err(ActionSinkError::InvalidInput(
                    "recipient is not a community member".into(),
                ));
            }
            let owner_bytes = owner.to_bytes();
            let recipient_bytes = recipient.to_bytes();
            let (dm, created) = state
                .db
                .open_dm(community, &[&owner_bytes, &recipient_bytes], &owner_bytes)
                .await
                .map_err(database)?;
            if created {
                for participant in [&owner_bytes, &recipient_bytes] {
                    state.invalidate_membership(&tenant, dm.id, participant);
                }
                emit_group_discovery_events(&tenant, &state, dm.id)
                    .await
                    .map_err(database)?;
                for participant in [&owner_bytes, &recipient_bytes] {
                    emit_membership_notification(
                        &tenant,
                        &state,
                        dm.id,
                        participant,
                        &owner_bytes,
                        KIND_MEMBER_ADDED_NOTIFICATION,
                    )
                    .await
                    .map_err(database)?;
                }
            }
            sink.send_message(community, &dm.id.to_string(), &text, author, None)
                .await
        }
        WorkflowSideEffect::SetChannelTopic { topic } => {
            let role = state
                .db
                .get_member_role(community, channel_id, &owner.to_bytes())
                .await
                .map_err(database)?;
            if !matches!(role.as_deref(), Some("owner") | Some("admin")) {
                return Err(ActionSinkError::InvalidInput(
                    "changing the topic requires channel owner/admin authority".into(),
                ));
            }
            state
                .db
                .set_topic(community, channel_id, &topic, &owner.to_bytes())
                .await
                .map_err(database)?;
            emit_group_discovery_events(&tenant, &state, channel_id)
                .await
                .map_err(database)?;
            Ok(channel_id.to_string())
        }
        WorkflowSideEffect::AddReaction { message_id, emoji } => {
            if emoji.trim().is_empty() || emoji.chars().count() > 64 {
                return Err(ActionSinkError::InvalidInput(
                    "reaction must contain 1..64 characters".into(),
                ));
            }
            let id = nostr::EventId::from_hex(&message_id).map_err(invalid)?;
            let target = state
                .db
                .get_event_by_id(community, id.as_bytes())
                .await
                .map_err(database)?
                .ok_or_else(|| ActionSinkError::InvalidInput("reaction target not found".into()))?;
            if target.channel_id != Some(channel_id) {
                return Err(ActionSinkError::InvalidInput(
                    "reaction target belongs to another channel".into(),
                ));
            }
            let tags = vec![
                Tag::parse(["e", id.to_hex().as_str()]).map_err(invalid)?,
                Tag::parse(["p", target.event.pubkey.to_hex().as_str()]).map_err(invalid)?,
            ];
            publish(
                &tenant,
                &state,
                channel_id,
                author,
                KIND_REACTION,
                &emoji,
                tags,
                None,
            )
            .await
        }
        WorkflowSideEffect::ApprovalRequested {
            token_hash,
            approver,
            message,
            run_id,
            created_at,
        } => {
            let mut tags = vec![
                Tag::parse(["d", token_hash.as_str()]).map_err(invalid)?,
                Tag::parse(["run", run_id.as_str()]).map_err(invalid)?,
            ];
            if approver != "any" {
                tags.push(Tag::parse(["p", approver.as_str()]).map_err(invalid)?);
            }
            publish(
                &tenant,
                &state,
                channel_id,
                author,
                KIND_WORKFLOW_APPROVAL_REQUESTED,
                &message,
                tags,
                Some(nostr::Timestamp::from(created_at)),
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn publish(
    tenant: &buzz_core::tenant::TenantContext,
    state: &Arc<AppState>,
    channel: Uuid,
    author: &str,
    kind: u32,
    content: &str,
    mut tags: Vec<Tag>,
    created_at: Option<nostr::Timestamp>,
) -> Result<String, ActionSinkError> {
    tags.extend([
        Tag::parse(["h", channel.to_string().as_str()]).map_err(invalid)?,
        Tag::parse(["buzz:workflow", "true"]).map_err(invalid)?,
        Tag::parse(["actor", author]).map_err(invalid)?,
    ]);
    let mut builder = EventBuilder::new(Kind::from(kind as u16), content).tags(tags);
    if let Some(created_at) = created_at {
        builder = builder.custom_created_at(created_at);
    }
    let event = builder
        .sign_with_keys(&state.relay_keypair)
        .map_err(invalid)?;
    let (stored, inserted) = if kind == KIND_REACTION {
        let target = event
            .tags
            .iter()
            .find_map(|tag| {
                let parts = tag.as_slice();
                (parts.first().map(String::as_str) == Some("e"))
                    .then(|| parts.get(1))
                    .flatten()
            })
            .ok_or_else(|| ActionSinkError::InvalidInput("reaction has no target".into()))?;
        let target = nostr::EventId::from_hex(target).map_err(invalid)?;
        let actor = nostr::PublicKey::from_hex(author).map_err(invalid)?;
        match state
            .db
            .insert_reaction_event_with_thread_metadata(
                tenant.community(),
                &event,
                Some(channel),
                None,
                target.as_bytes(),
                &actor.to_bytes(),
                content,
            )
            .await
            .map_err(database)?
        {
            buzz_db::ReactionEventInsertOutcome::Inserted {
                stored_event,
                was_inserted,
            } => (*stored_event, was_inserted),
            buzz_db::ReactionEventInsertOutcome::TargetMissing => {
                return Err(ActionSinkError::InvalidInput(
                    "reaction target disappeared".into(),
                ))
            }
            buzz_db::ReactionEventInsertOutcome::Duplicate => {
                let target_event = state
                    .db
                    .get_event_by_id(tenant.community(), target.as_bytes())
                    .await
                    .map_err(database)?
                    .ok_or_else(|| {
                        ActionSinkError::InvalidInput("reaction target disappeared".into())
                    })?;
                let existing = state
                    .db
                    .get_active_reaction_record(
                        tenant.community(),
                        target.as_bytes(),
                        chrono::DateTime::from_timestamp(
                            i64::try_from(target_event.event.created_at.as_secs())
                                .map_err(invalid)?,
                            0,
                        )
                        .ok_or_else(|| {
                            ActionSinkError::InvalidInput(
                                "reaction target timestamp is invalid".into(),
                            )
                        })?,
                        &actor.to_bytes(),
                        content,
                    )
                    .await
                    .map_err(database)?;
                return existing
                    .and_then(|reaction| reaction.reaction_event_id)
                    .map(hex::encode)
                    .ok_or_else(|| {
                        ActionSinkError::Database("existing reaction has no event".into())
                    });
            }
        }
    } else {
        state
            .db
            .insert_event(tenant.community(), &event, Some(channel))
            .await
            .map_err(database)?
    };
    if inserted {
        dispatch_persistent_event(tenant, state, &stored, kind, author, None).await;
    }
    Ok(event.id.to_hex())
}

fn invalid(error: impl std::fmt::Display) -> ActionSinkError {
    ActionSinkError::InvalidInput(error.to_string())
}
fn database(error: impl std::fmt::Display) -> ActionSinkError {
    ActionSinkError::Database(error.to_string())
}
