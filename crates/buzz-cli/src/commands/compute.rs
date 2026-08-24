//! `buzz compute` project/provider ledger query commands.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use buzz_core::kind::{
    KIND_ST8_COMPUTE_CAPABILITY, KIND_ST8_COMPUTE_JOB, KIND_ST8_COMPUTE_LEDGER_ENTRY,
    KIND_ST8_COMPUTE_PRICING, KIND_ST8_COMPUTE_SETTLEMENT_ENTRY,
};
use serde::Serialize;
use serde_json::Value;

use crate::client::BuzzClient;
use crate::error::CliError;
use crate::ComputeCmd;

/// Dispatches a project-scoped compute ledger query.
pub async fn dispatch(command: ComputeCmd, client: &BuzzClient) -> Result<(), CliError> {
    match command {
        ComputeCmd::Nodes { project, limit } => nodes(client, &project, limit).await,
        ComputeCmd::Jobs {
            project,
            provider,
            limit,
        } => jobs(client, &project, provider.as_deref(), limit).await,
        ComputeCmd::Receipts {
            project,
            provider,
            status,
            limit,
        } => {
            receipts(
                client,
                &project,
                provider.as_deref(),
                status.as_deref(),
                limit,
            )
            .await
        }
        ComputeCmd::Balances {
            project,
            provider,
            limit,
        } => balances(client, &project, provider.as_deref(), limit).await,
        ComputeCmd::Settlements { project, limit } => settlements(client, &project, limit).await,
        ComputeCmd::ShowReceipt { receipt_id } => show_receipt(client, &receipt_id).await,
    }
}

async fn nodes(client: &BuzzClient, project: &str, limit: u32) -> Result<(), CliError> {
    validate_project(project)?;
    validate_limit(limit)?;
    let pricing = client
        .query_paginated(
            serde_json::json!({
                "kinds": [KIND_ST8_COMPUTE_PRICING],
                "#a": [project],
            }),
            limit,
        )
        .await?;
    let node_ids: BTreeSet<String> = pricing
        .iter()
        .map(|event| {
            let content = event_content(event)?;
            json_string(&content, "node_owner_id").map(str::to_owned)
        })
        .collect::<Result<_, _>>()?;
    let capabilities = if node_ids.is_empty() {
        Vec::new()
    } else {
        client
            .query_paginated(
                serde_json::json!({
                    "kinds": [KIND_ST8_COMPUTE_CAPABILITY],
                    "#d": node_ids,
                }),
                limit,
            )
            .await?
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CliError::Other(format!("system clock is before Unix epoch: {error}")))?
        .as_secs();
    let (pricing, capabilities) = select_active_nodes(project, pricing, capabilities, now);
    print_json(&serde_json::json!({
        "project": project,
        "pricing": pricing,
        "capabilities": capabilities,
    }))
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct NodeBinding {
    provider: String,
    node_owner_id: String,
    pricing_policy_id: String,
}

fn select_active_nodes(
    project: &str,
    pricing: Vec<Value>,
    capabilities: Vec<Value>,
    now: u64,
) -> (Vec<Value>, Vec<Value>) {
    let pricing_by_binding: BTreeMap<NodeBinding, Value> = pricing
        .into_iter()
        .filter_map(|event| pricing_binding(&event, project).map(|binding| (binding, event)))
        .collect();
    let mut capability_by_binding: BTreeMap<NodeBinding, Value> = BTreeMap::new();
    for event in capabilities {
        let Some(binding) = capability_binding(&event, now) else {
            continue;
        };
        if !pricing_by_binding.contains_key(&binding) {
            continue;
        }
        let replace = capability_by_binding
            .get(&binding)
            .is_none_or(|current| event_created_at(&event) > event_created_at(current));
        if replace {
            capability_by_binding.insert(binding, event);
        }
    }
    let pricing = capability_by_binding
        .keys()
        .filter_map(|binding| pricing_by_binding.get(binding).cloned())
        .collect();
    let capabilities = capability_by_binding.into_values().collect();
    (pricing, capabilities)
}

fn pricing_binding(event: &Value, project: &str) -> Option<NodeBinding> {
    let content = event_content(event).ok()?;
    if json_string(&content, "project").ok()? != project {
        return None;
    }
    let provider = json_string(&content, "provider").ok()?.to_ascii_lowercase();
    if !event_pubkey_matches_provider(event, &provider) {
        return None;
    }
    Some(NodeBinding {
        provider,
        node_owner_id: json_string(&content, "node_owner_id").ok()?.to_owned(),
        pricing_policy_id: event_tag(event, "d")?.to_ascii_lowercase(),
    })
}

fn capability_binding(event: &Value, now: u64) -> Option<NodeBinding> {
    let content = event_content(event).ok()?;
    let capability = content.get("capability")?;
    let available_until = capability.get("available_until")?.as_u64()?;
    if available_until <= now {
        return None;
    }
    let provider = json_string(capability, "provider")
        .ok()?
        .to_ascii_lowercase();
    if !event_pubkey_matches_provider(event, &provider) {
        return None;
    }
    let node_owner_id = json_string(capability, "node_owner_id").ok()?.to_owned();
    if event_tag(event, "d")? != node_owner_id {
        return None;
    }
    Some(NodeBinding {
        provider,
        node_owner_id,
        pricing_policy_id: byte_array_hex(capability.get("pricing_policy_id")?)?,
    })
}

fn event_pubkey_matches_provider(event: &Value, provider: &str) -> bool {
    let Some(public_key) = event.get("pubkey").and_then(Value::as_str) else {
        return false;
    };
    crate::validate::validate_hex64(public_key).is_ok()
        && provider == format!("nostr:{}", public_key.to_ascii_lowercase())
}

fn event_tag<'a>(event: &'a Value, name: &str) -> Option<&'a str> {
    event.get("tags")?.as_array()?.iter().find_map(|tag| {
        let tag = tag.as_array()?;
        (tag.first()?.as_str()? == name)
            .then(|| tag.get(1)?.as_str())
            .flatten()
    })
}

fn byte_array_hex(value: &Value) -> Option<String> {
    let bytes = value
        .as_array()?
        .iter()
        .map(|value| u8::try_from(value.as_u64()?).ok())
        .collect::<Option<Vec<_>>>()?;
    (bytes.len() == 32).then(|| hex::encode(bytes))
}

fn event_created_at(event: &Value) -> u64 {
    event.get("created_at").and_then(Value::as_u64).unwrap_or(0)
}

async fn jobs(
    client: &BuzzClient,
    project: &str,
    provider: Option<&str>,
    limit: u32,
) -> Result<(), CliError> {
    validate_project(project)?;
    validate_limit(limit)?;
    let mut filter = serde_json::json!({
        "kinds": [KIND_ST8_COMPUTE_JOB],
        "#a": [project],
    });
    add_provider_filter(&mut filter, provider)?;
    print_events(client.query_paginated(filter, limit).await?)
}

async fn receipts(
    client: &BuzzClient,
    project: &str,
    provider: Option<&str>,
    status: Option<&str>,
    limit: u32,
) -> Result<(), CliError> {
    validate_project(project)?;
    validate_limit(limit)?;
    let mut filter = serde_json::json!({
        "kinds": [KIND_ST8_COMPUTE_LEDGER_ENTRY],
        "#a": [project],
    });
    add_provider_filter(&mut filter, provider)?;
    if let Some(status) = status {
        if !matches!(status, "completed" | "failed" | "cancelled") {
            return Err(CliError::Usage(
                "--status must be completed, failed, or cancelled".into(),
            ));
        }
        filter["#st8-status"] = serde_json::json!([status]);
    }
    let events = client.query_paginated(filter, 1_000).await?;
    print_events(latest_addressable(events, limit as usize)?)
}

#[derive(Debug, Default, Eq, PartialEq, Serialize)]
struct ProviderBalance {
    provider: String,
    node_owner_id: String,
    receipt_count: u64,
    pending_sats: u64,
    disputed_sats: u64,
    queued_sats: u64,
    confirmed_sats: u64,
    total_sats: u64,
}

async fn balances(
    client: &BuzzClient,
    project: &str,
    provider: Option<&str>,
    limit: u32,
) -> Result<(), CliError> {
    validate_project(project)?;
    validate_limit(limit)?;
    let mut filter = serde_json::json!({
        "kinds": [KIND_ST8_COMPUTE_LEDGER_ENTRY],
        "#a": [project],
    });
    add_provider_filter(&mut filter, provider)?;
    let events = latest_addressable(client.query_paginated(filter, 1_000).await?, limit as usize)?;
    let totals = derive_balances(&events)?;
    print_json(&serde_json::json!({
        "project": project,
        "receipts_considered": events.len(),
        "limit": limit,
        "balances": totals,
        "currency": "BSV_SATOSHIS",
        "contribution_units": null,
    }))
}

fn derive_balances(events: &[Value]) -> Result<Vec<ProviderBalance>, CliError> {
    let mut totals: BTreeMap<(String, String), ProviderBalance> = BTreeMap::new();
    for event in events {
        let content = event_content(event)?;
        let provider = json_string(&content, "provider")?.to_owned();
        let node = json_string(&content, "node_owner_id")?.to_owned();
        let cost = content
            .get("cost_sats")
            .and_then(Value::as_u64)
            .ok_or_else(|| CliError::Other("compute ledger entry has invalid cost".into()))?;
        let balance = totals
            .entry((provider.clone(), node.clone()))
            .or_insert_with(|| ProviderBalance {
                provider,
                node_owner_id: node,
                ..ProviderBalance::default()
            });
        balance.receipt_count = checked_add(balance.receipt_count, 1)?;
        balance.total_sats = checked_add(balance.total_sats, cost)?;
        let disputed = content.get("dispute_state").and_then(Value::as_str) == Some("disputed");
        let settlement = content.get("settlement_state").and_then(Value::as_str);
        if disputed {
            balance.disputed_sats = checked_add(balance.disputed_sats, cost)?;
        } else if settlement == Some("confirmed") {
            balance.confirmed_sats = checked_add(balance.confirmed_sats, cost)?;
        } else if settlement == Some("queued")
            || content.get("settlement_id").is_some_and(|id| !id.is_null())
        {
            balance.queued_sats = checked_add(balance.queued_sats, cost)?;
        } else {
            balance.pending_sats = checked_add(balance.pending_sats, cost)?;
        }
    }
    Ok(totals.into_values().collect())
}

async fn settlements(client: &BuzzClient, project: &str, limit: u32) -> Result<(), CliError> {
    validate_project(project)?;
    validate_limit(limit)?;
    let events = client
        .query_paginated(
            serde_json::json!({
                "kinds": [KIND_ST8_COMPUTE_SETTLEMENT_ENTRY],
                "#a": [project],
            }),
            1_000,
        )
        .await?;
    print_events(latest_addressable(events, limit as usize)?)
}

async fn show_receipt(client: &BuzzClient, receipt_id: &str) -> Result<(), CliError> {
    crate::validate::validate_hex64(receipt_id)?;
    let events = client
        .query_paginated(
            serde_json::json!({
                "kinds": [KIND_ST8_COMPUTE_LEDGER_ENTRY],
                "#d": [receipt_id.to_ascii_lowercase()],
            }),
            1_000,
        )
        .await?;
    let events = latest_addressable(events, 1)?;
    if events.is_empty() {
        return Err(CliError::NotFound(format!(
            "compute receipt {receipt_id} is not in the ledger"
        )));
    }
    print_events(events)
}

fn add_provider_filter(filter: &mut Value, provider: Option<&str>) -> Result<(), CliError> {
    if let Some(provider) = provider {
        filter["#p"] = serde_json::json!([normalize_provider(provider)?]);
    }
    Ok(())
}

fn normalize_provider(provider: &str) -> Result<String, CliError> {
    let public_key = provider.strip_prefix("nostr:").unwrap_or(provider);
    crate::validate::validate_hex64(public_key)?;
    Ok(public_key.to_ascii_lowercase())
}

fn validate_project(project: &str) -> Result<(), CliError> {
    let mut parts = project.splitn(3, ':');
    if parts.next() != Some("30621") {
        return Err(CliError::Usage(
            "project must be a full 30621:<owner-pubkey>:<slug> coordinate".into(),
        ));
    }
    let owner = parts
        .next()
        .ok_or_else(|| CliError::Usage("project coordinate is missing its owner".into()))?;
    crate::validate::validate_hex64(owner)?;
    if parts.next().is_none_or(str::is_empty) {
        return Err(CliError::Usage(
            "project coordinate is missing its slug".into(),
        ));
    }
    Ok(())
}

fn validate_limit(limit: u32) -> Result<(), CliError> {
    if !(1..=1_000).contains(&limit) {
        return Err(CliError::Usage("--limit must be between 1 and 1000".into()));
    }
    Ok(())
}

fn event_content(event: &Value) -> Result<Value, CliError> {
    let body = event
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Other("compute event is missing content".into()))?;
    // Relay query values are signature-stripped for reads, but the signed
    // event content itself remains byte-exact JSON.
    serde_json::from_str(body)
        .map_err(|error| CliError::Other(format!("compute event content is invalid: {error}")))
}

/// Collapse NIP-33 addressable history before displaying or totaling state.
///
/// The relay intentionally retains prior signed versions for auditability. A
/// ledger view must therefore apply the NIP-01 replacement rule: greatest
/// `created_at` wins and the lowest event id breaks an equal-time tie.
fn latest_addressable(events: Vec<Value>, limit: usize) -> Result<Vec<Value>, CliError> {
    let mut latest: BTreeMap<(u64, String, String), Value> = BTreeMap::new();
    for event in events {
        let kind = event
            .get("kind")
            .and_then(Value::as_u64)
            .ok_or_else(|| CliError::Other("compute event is missing kind".into()))?;
        let public_key = event
            .get("pubkey")
            .and_then(Value::as_str)
            .ok_or_else(|| CliError::Other("compute event is missing pubkey".into()))?
            .to_ascii_lowercase();
        let identifier = event_tag(&event, "d")
            .ok_or_else(|| CliError::Other("compute event is missing d tag".into()))?
            .to_owned();
        let key = (kind, public_key, identifier);
        let replace = latest
            .get(&key)
            .is_none_or(|current| addressable_precedes(current, &event));
        if replace {
            latest.insert(key, event);
        }
    }
    let mut selected: Vec<Value> = latest.into_values().collect();
    selected.sort_by(|left, right| {
        event_created_at(right)
            .cmp(&event_created_at(left))
            .then_with(|| event_id(left).cmp(event_id(right)))
    });
    selected.truncate(limit);
    Ok(selected)
}

fn addressable_precedes(current: &Value, candidate: &Value) -> bool {
    event_created_at(candidate) > event_created_at(current)
        || (event_created_at(candidate) == event_created_at(current)
            && event_id(candidate) < event_id(current))
}

fn event_id(event: &Value) -> &str {
    event.get("id").and_then(Value::as_str).unwrap_or("")
}

fn json_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, CliError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Other(format!("compute ledger entry is missing {field}")))
}

fn checked_add(left: u64, right: u64) -> Result<u64, CliError> {
    left.checked_add(right)
        .ok_or_else(|| CliError::Other("compute balance overflow".into()))
}

fn print_events(events: Vec<Value>) -> Result<(), CliError> {
    print_json(&Value::Array(events))
}

fn print_json(value: &Value) -> Result<(), CliError> {
    println!(
        "{}",
        serde_json::to_string(value)
            .map_err(|error| CliError::Other(format!("JSON output failed: {error}")))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_normalization_accepts_nostr_and_rejects_bad_keys() {
        let key = "ab".repeat(32);
        assert_eq!(
            normalize_provider(&format!("nostr:{key}")).expect("key"),
            key
        );
        assert!(normalize_provider("not-a-key").is_err());
    }

    #[test]
    fn limit_and_project_validation_are_bounded() {
        let project = format!("30621:{}:compute", "ab".repeat(32));
        assert!(validate_project(&project).is_ok());
        assert!(validate_project("30621:bad:compute").is_err());
        assert!(validate_limit(1).is_ok());
        assert!(validate_limit(1_001).is_err());
    }

    #[test]
    fn balances_separate_pending_disputed_queued_and_confirmed_satoshis() {
        fn event(cost: u64, dispute: &str, settlement: &str, settlement_id: Value) -> Value {
            serde_json::json!({
                "content": serde_json::json!({
                    "provider": "nostr:provider",
                    "node_owner_id": "mesh-node",
                    "cost_sats": cost,
                    "dispute_state": dispute,
                    "settlement_state": settlement,
                    "settlement_id": settlement_id,
                }).to_string()
            })
        }

        let events = vec![
            event(2, "clear", "pending", Value::Null),
            event(3, "disputed", "confirmed", serde_json::json!("ignored")),
            event(5, "clear", "pending", serde_json::json!("settlement")),
            event(7, "clear", "confirmed", serde_json::json!("settlement")),
        ];
        let balances = derive_balances(&events).expect("valid ledger entries");
        assert_eq!(
            balances,
            vec![ProviderBalance {
                provider: "nostr:provider".into(),
                node_owner_id: "mesh-node".into(),
                receipt_count: 4,
                pending_sats: 2,
                disputed_sats: 3,
                queued_sats: 5,
                confirmed_sats: 7,
                total_sats: 17,
            }]
        );
    }

    #[test]
    fn node_discovery_filters_expired_and_mismatched_bindings() {
        fn pricing(project: &str, public_key: &str, node: &str, policy: &str) -> Value {
            serde_json::json!({
                "pubkey": public_key,
                "content": serde_json::json!({
                    "project": project,
                    "provider": format!("nostr:{public_key}"),
                    "node_owner_id": node,
                }).to_string(),
                "tags": [["d", policy], ["a", project]],
            })
        }

        fn capability(
            public_key: &str,
            node: &str,
            policy_byte: u8,
            available_until: u64,
            created_at: u64,
        ) -> Value {
            serde_json::json!({
                "pubkey": public_key,
                "created_at": created_at,
                "content": serde_json::json!({
                    "capability": {
                        "provider": format!("nostr:{public_key}"),
                        "node_owner_id": node,
                        "pricing_policy_id": vec![policy_byte; 32],
                        "available_until": available_until,
                    }
                }).to_string(),
                "tags": [["d", node]],
            })
        }

        let project = format!("30621:{}:compute", "ab".repeat(32));
        let provider = "01".repeat(32);
        let expired_provider = "02".repeat(32);
        let node = "03".repeat(32);
        let expired_node = "04".repeat(32);
        let policy = "11".repeat(32);
        let expired_policy = "22".repeat(32);
        let pricing = vec![
            pricing(&project, &provider, &node, &policy),
            pricing(&project, &expired_provider, &expired_node, &expired_policy),
        ];
        let capabilities = vec![
            capability(&provider, &node, 0x11, 2_000, 10),
            capability(&provider, &node, 0x11, 2_000, 20),
            capability(&provider, &node, 0x33, 2_000, 30),
            capability(&expired_provider, &expired_node, 0x22, 999, 40),
        ];

        let (pricing, capabilities) = select_active_nodes(&project, pricing, capabilities, 1_000);
        assert_eq!(pricing.len(), 1);
        assert_eq!(capabilities.len(), 1);
        assert_eq!(capabilities[0]["created_at"], 20);
        assert_eq!(event_tag(&pricing[0], "d"), Some(policy.as_str()));
    }

    #[test]
    fn addressable_history_collapses_to_latest_state_before_balancing() {
        fn receipt(id: &str, created_at: u64, state: &str) -> Value {
            serde_json::json!({
                "id": id,
                "kind": KIND_ST8_COMPUTE_LEDGER_ENTRY,
                "pubkey": "01".repeat(32),
                "created_at": created_at,
                "tags": [["d", "receipt-1"]],
                "content": serde_json::json!({
                    "provider": "nostr:provider",
                    "node_owner_id": "mesh-node",
                    "cost_sats": 6,
                    "dispute_state": "undisputed",
                    "settlement_state": state,
                }).to_string(),
            })
        }

        let selected = latest_addressable(
            vec![
                receipt(&"03".repeat(32), 10, "pending"),
                receipt(&"02".repeat(32), 20, "queued"),
                receipt(&"01".repeat(32), 30, "confirmed"),
            ],
            100,
        )
        .expect("valid addressable events");
        assert_eq!(selected.len(), 1);
        assert_eq!(
            event_content(&selected[0]).expect("content")["settlement_state"],
            "confirmed"
        );

        let balances = derive_balances(&selected).expect("balance");
        assert_eq!(balances[0].receipt_count, 1);
        assert_eq!(balances[0].total_sats, 6);
        assert_eq!(balances[0].confirmed_sats, 6);
    }

    #[test]
    fn addressable_equal_timestamp_uses_lowest_event_id() {
        let event = |id: &str, content: &str| {
            serde_json::json!({
                "id": id,
                "kind": KIND_ST8_COMPUTE_SETTLEMENT_ENTRY,
                "pubkey": "01".repeat(32),
                "created_at": 10,
                "tags": [["d", "settlement-1"]],
                "content": content,
            })
        };
        let selected = latest_addressable(
            vec![
                event(&"ff".repeat(32), "older"),
                event(&"00".repeat(32), "winner"),
            ],
            1,
        )
        .expect("valid addressable events");
        assert_eq!(selected[0]["content"], "winner");
    }
}
