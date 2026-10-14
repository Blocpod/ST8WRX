//! Regression test for `e3661764` / `7899c1a8`: a workflow `send_message`
//! that mentions a channel member by name (`@Name`) must emit a `p` tag for
//! that member so ACP agent wake (`event_mentions_agent`, p-tag gated) fires.
//!
//! Postgres-gated like the other DB-backed relay tests. Run with:
//!   `cargo test -p buzz-relay --lib workflow_sink -- --ignored`
use super::*;
use buzz_core::channel::{ChannelType, ChannelVisibility, MemberRole};
use buzz_db::CreateCommunityWithOwnerResult;
use std::sync::Arc;

/// Real-PG state mirroring `handlers::event::tests::test_state_with_redis_url`.
pub(super) async fn test_state() -> Arc<AppState> {
    let mut config = crate::config::Config::from_env().expect("default config loads");
    if let Ok(url) = std::env::var("BUZZ_WORKFLOW_TEST_DATABASE_URL") {
        config.database_url = url;
    }
    config.require_relay_membership = false;
    config.redis_url = "redis://127.0.0.1:1".to_string();
    let pool = sqlx::PgPool::connect_lazy(&config.database_url).expect("lazy pg pool");
    let db = buzz_db::Db::from_pool(pool.clone());
    let redis_pool = deadpool_redis::Config::from_url(&config.redis_url)
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .expect("redis pool");
    let pubsub = Arc::new(
        buzz_pubsub::PubSubManager::new(&config.redis_url, redis_pool.clone())
            .await
            .expect("pubsub manager"),
    );
    let audit = buzz_audit::AuditService::new(pool.clone());
    let auth = buzz_auth::AuthService::new(config.auth.clone());
    let search = buzz_search::SearchService::new(pool.clone());
    let workflow_engine = Arc::new(buzz_workflow::WorkflowEngine::new(
        db.clone(),
        buzz_workflow::WorkflowConfig::default(),
    ));
    let media_storage = buzz_media::MediaStorage::new(&config.media).expect("media storage");
    let (state, _audit_shutdown) = AppState::new(
        config,
        db,
        redis_pool,
        audit,
        pubsub,
        auth,
        search,
        workflow_engine,
        nostr::Keys::generate(),
        media_storage,
    );
    Arc::new(state)
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn workflow_send_message_p_tags_mentioned_member() {
    let state = test_state().await;

    let author = nostr::Keys::generate();
    let author_hex = author.public_key().to_hex();
    let agent = nostr::Keys::generate();
    let agent_hex = agent.public_key().to_hex();
    let agent_bytes = agent.public_key().to_bytes().to_vec();

    let host = format!("wf-ptag-{}.example", uuid::Uuid::new_v4().simple());
    let community = match state
        .db
        .create_community_with_owner(&host, &author_hex)
        .await
        .expect("create community")
    {
        CreateCommunityWithOwnerResult::Created(rec) => rec.id,
        other => panic!("expected fresh community, got {other:?}"),
    };

    // Open channel; the creator (author) is bootstrapped as an owner-member.
    let channel = state
        .db
        .create_channel(
            community,
            "wf-ptag",
            ChannelType::Stream,
            ChannelVisibility::Open,
            None,
            &author.public_key().to_bytes(),
            None,
        )
        .await
        .expect("create channel");

    // The mentioned agent is a real member with a resolvable display name.
    state
        .db
        .ensure_user(community, &agent_bytes)
        .await
        .expect("ensure agent user row");
    state
        .db
        .update_user_profile(community, &agent_bytes, Some("Robby"), None, None, None)
        .await
        .expect("set agent display name");
    state
        .db
        .add_member(
            community,
            channel.id,
            &agent_bytes,
            MemberRole::Bot,
            Some(&author.public_key().to_bytes()),
        )
        .await
        .expect("add agent member");

    let sink = RelayActionSink::new(&state);
    let event_id_hex = sink
        .send_message(
            community,
            &channel.id.to_string(),
            "heads up @Robby — please take a look",
            &author_hex,
            None,
        )
        .await
        .expect("send_message");

    let id_bytes = nostr::EventId::from_hex(&event_id_hex)
        .expect("event id")
        .as_bytes()
        .to_vec();
    let stored = state
        .db
        .get_event_by_id(community, &id_bytes)
        .await
        .expect("query event")
        .expect("event persisted");

    let p_tag_targets: Vec<&str> = stored
        .event
        .tags
        .iter()
        .filter(|t| t.as_slice().first().map(|s| s.as_str()) == Some("p"))
        .filter_map(|t| t.as_slice().get(1).map(|s| s.as_str()))
        .collect();

    assert!(
        p_tag_targets.contains(&author_hex.as_str()),
        "author should still be attributed via p tag; got {p_tag_targets:?}"
    );
    assert!(
        p_tag_targets.contains(&agent_hex.as_str()),
        "mentioned member {agent_hex} must be p-tagged so it wakes; got {p_tag_targets:?}"
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn workflow_reply_in_thread_threads_onto_parent() {
    let state = test_state().await;

    let author = nostr::Keys::generate();
    let author_hex = author.public_key().to_hex();

    let host = format!("wf-thread-{}.example", uuid::Uuid::new_v4().simple());
    let community = match state
        .db
        .create_community_with_owner(&host, &author_hex)
        .await
        .expect("create community")
    {
        CreateCommunityWithOwnerResult::Created(rec) => rec.id,
        other => panic!("expected fresh community, got {other:?}"),
    };

    let channel = state
        .db
        .create_channel(
            community,
            "wf-thread",
            ChannelType::Stream,
            ChannelVisibility::Open,
            None,
            &author.public_key().to_bytes(),
            None,
        )
        .await
        .expect("create channel");

    let sink = RelayActionSink::new(&state);

    // 1. A top-level workflow message becomes the thread root.
    let root_hex = sink
        .send_message(
            community,
            &channel.id.to_string(),
            "root message",
            &author_hex,
            None,
        )
        .await
        .expect("send root");

    // 2. A reply_in_thread message threads onto it.
    let reply_hex = sink
        .send_message(
            community,
            &channel.id.to_string(),
            "threaded reply",
            &author_hex,
            Some(&root_hex),
        )
        .await
        .expect("send reply");

    // A direct reply carries a single NIP-10 reply e-tag at the root (no
    // root marker), matching SDK `thread_tags`.
    let reply_id_bytes = nostr::EventId::from_hex(&reply_hex)
        .expect("reply id")
        .as_bytes()
        .to_vec();
    let stored = state
        .db
        .get_event_by_id(community, &reply_id_bytes)
        .await
        .expect("query reply")
        .expect("reply persisted");
    let marker = |m: &str| -> Option<String> {
        stored.event.tags.iter().find_map(|t| {
            let p = t.as_slice();
            if p.len() >= 4 && p[0] == "e" && p[3] == m {
                Some(p[1].clone())
            } else {
                None
            }
        })
    };
    assert_eq!(
        marker("reply").as_deref(),
        Some(root_hex.as_str()),
        "direct reply emits a single reply marker at the root"
    );
    assert_eq!(
        marker("root"),
        None,
        "direct reply omits the root marker (matches SDK thread_tags)"
    );

    // Thread metadata reflects a depth-1 reply parented on the root.
    let meta = state
        .db
        .get_thread_metadata_by_event(community, &reply_id_bytes)
        .await
        .expect("query meta")
        .expect("reply has thread metadata");
    assert_eq!(
        meta.depth, 1,
        "direct reply to a top-level message is depth 1"
    );
    let root_bytes = nostr::EventId::from_hex(&root_hex)
        .expect("root id")
        .as_bytes()
        .to_vec();
    assert_eq!(meta.parent_event_id.as_deref(), Some(root_bytes.as_slice()));
    assert_eq!(meta.root_event_id.as_deref(), Some(root_bytes.as_slice()));
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn workflow_replies_recover_metadata_less_parent_ancestry() {
    // A parent that carries NIP-10 root/reply markers but has NO
    // thread_metadata row (legacy or not-yet-indexed) must be recognized as
    // nested: the workflow reply threads at depth 2 onto the parent's own
    // root, not a false top-level depth 1.
    let state = test_state().await;

    let author = nostr::Keys::generate();
    let author_hex = author.public_key().to_hex();

    let host = format!("wf-legacy-{}.example", uuid::Uuid::new_v4().simple());
    let community = match state
        .db
        .create_community_with_owner(&host, &author_hex)
        .await
        .expect("create community")
    {
        CreateCommunityWithOwnerResult::Created(rec) => rec.id,
        other => panic!("expected fresh community, got {other:?}"),
    };

    let channel = state
        .db
        .create_channel(
            community,
            "wf-legacy",
            ChannelType::Stream,
            ChannelVisibility::Open,
            None,
            &author.public_key().to_bytes(),
            None,
        )
        .await
        .expect("create channel");

    let channel_hex = channel.id.to_string();

    // A top-level root message, inserted WITHOUT any thread metadata row.
    let root_event = EventBuilder::new(Kind::from(KIND_STREAM_MESSAGE as u16), "root")
        .tags([Tag::parse(["h", &channel_hex]).expect("h tag")])
        .sign_with_keys(&author)
        .expect("sign root");
    let root_hex = root_event.id.to_hex();
    state
        .db
        .insert_event(community, &root_event, Some(channel.id))
        .await
        .expect("insert root");

    // A nested parent that marks its root/reply — but, crucially, is stored
    // with NO thread_metadata row (the legacy/unindexed case F1 addresses).
    let parent_event = EventBuilder::new(Kind::from(KIND_STREAM_MESSAGE as u16), "nested parent")
        .tags([
            Tag::parse(["h", &channel_hex]).expect("h tag"),
            Tag::parse(["e", &root_hex, "", "root"]).expect("root tag"),
            Tag::parse(["e", &root_hex, "", "reply"]).expect("reply tag"),
        ])
        .sign_with_keys(&author)
        .expect("sign parent");
    let parent_hex = parent_event.id.to_hex();
    state
        .db
        .insert_event(community, &parent_event, Some(channel.id))
        .await
        .expect("insert parent");
    assert!(
        state
            .db
            .get_thread_metadata_by_event(community, parent_event.id.as_bytes())
            .await
            .expect("query parent meta")
            .is_none(),
        "test premise: the nested parent must have no thread_metadata row"
    );

    // A workflow reply onto the metadata-less nested parent.
    let reply_hex = RelayActionSink::new(&state)
        .send_message(
            community,
            &channel_hex,
            "workflow reply",
            &author_hex,
            Some(&parent_hex),
        )
        .await
        .expect("send reply");

    let reply_id_bytes = nostr::EventId::from_hex(&reply_hex)
        .expect("reply id")
        .as_bytes()
        .to_vec();
    let meta = state
        .db
        .get_thread_metadata_by_event(community, &reply_id_bytes)
        .await
        .expect("query meta")
        .expect("reply has thread metadata");

    assert_eq!(
        meta.depth, 2,
        "reply to a marked-but-unindexed nested parent is depth 2, not top-level"
    );
    let root_bytes = nostr::EventId::from_hex(&root_hex)
        .expect("root id")
        .as_bytes()
        .to_vec();
    let parent_bytes = parent_event.id.as_bytes().to_vec();
    assert_eq!(
        meta.root_event_id.as_deref(),
        Some(root_bytes.as_slice()),
        "root recovered from the parent's own NIP-10 markers"
    );
    assert_eq!(
        meta.parent_event_id.as_deref(),
        Some(parent_bytes.as_slice())
    );

    // The reply's own NIP-10 e-tags point root→the recovered root,
    // reply→the immediate parent (matching the ingest resolver).
    let stored = state
        .db
        .get_event_by_id(community, &reply_id_bytes)
        .await
        .expect("query reply")
        .expect("reply persisted");
    let marker = |m: &str| -> Option<String> {
        stored.event.tags.iter().find_map(|t| {
            let p = t.as_slice();
            if p.len() >= 4 && p[0] == "e" && p[3] == m {
                Some(p[1].clone())
            } else {
                None
            }
        })
    };
    assert_eq!(marker("root").as_deref(), Some(root_hex.as_str()));
    assert_eq!(marker("reply").as_deref(), Some(parent_hex.as_str()));

    // A root-only parent is top-level under the shared collapse rule, even
    // without metadata. A workflow reply therefore starts a thread at P,
    // rather than incorrectly inheriting the marker's unrelated root R.
    let root_only_parent =
        EventBuilder::new(Kind::from(KIND_STREAM_MESSAGE as u16), "root-only parent")
            .tags([
                Tag::parse(["h", &channel_hex]).expect("h tag"),
                Tag::parse(["e", &root_hex, "", "root"]).expect("root tag"),
            ])
            .sign_with_keys(&author)
            .expect("sign root-only parent");
    let root_only_parent_hex = root_only_parent.id.to_hex();
    let root_only_parent_bytes = root_only_parent.id.as_bytes().to_vec();
    state
        .db
        .insert_event(community, &root_only_parent, Some(channel.id))
        .await
        .expect("insert root-only parent");

    let root_only_reply_hex = RelayActionSink::new(&state)
        .send_message(
            community,
            &channel_hex,
            "workflow reply to root-only parent",
            &author_hex,
            Some(&root_only_parent_hex),
        )
        .await
        .expect("send root-only reply");
    let root_only_reply_bytes = nostr::EventId::from_hex(&root_only_reply_hex)
        .expect("reply id")
        .as_bytes()
        .to_vec();
    let root_only_meta = state
        .db
        .get_thread_metadata_by_event(community, &root_only_reply_bytes)
        .await
        .expect("query root-only reply meta")
        .expect("root-only reply has thread metadata");
    assert_eq!(root_only_meta.depth, 1);
    assert_eq!(
        root_only_meta.parent_event_id.as_deref(),
        Some(root_only_parent_bytes.as_slice())
    );
    assert_eq!(
        root_only_meta.root_event_id.as_deref(),
        Some(root_only_parent_bytes.as_slice())
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn workflow_reply_to_missing_parent_errors() {
    let state = test_state().await;
    let author = nostr::Keys::generate();
    let author_hex = author.public_key().to_hex();
    let host = format!("wf-missing-{}.example", uuid::Uuid::new_v4().simple());
    let community = match state
        .db
        .create_community_with_owner(&host, &author_hex)
        .await
        .expect("create community")
    {
        CreateCommunityWithOwnerResult::Created(rec) => rec.id,
        other => panic!("expected fresh community, got {other:?}"),
    };
    let channel = state
        .db
        .create_channel(
            community,
            "wf-missing",
            ChannelType::Stream,
            ChannelVisibility::Open,
            None,
            &author.public_key().to_bytes(),
            None,
        )
        .await
        .expect("create channel");

    let unknown = nostr::Keys::generate().public_key().to_hex();
    let err = RelayActionSink::new(&state)
        .send_message(
            community,
            &channel.id.to_string(),
            "orphan reply",
            &author_hex,
            Some(&unknown),
        )
        .await
        .expect_err("reply to a non-existent parent must fail");
    assert!(
        matches!(err, ActionSinkError::InvalidInput(_)),
        "expected InvalidInput, got {err:?}"
    );
}
