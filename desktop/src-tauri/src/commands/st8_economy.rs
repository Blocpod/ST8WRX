use std::collections::BTreeSet;

use buzz_core_pkg::kind::*;
use nostr::{Event, EventBuilder, Keys};
use serde_json::{json, Value};
use st8_contribution_engine::BuzzProjectContext;
use tauri::State;

use crate::{
    app_state::AppState,
    relay::{
        assert_expected_relay_scope, assert_expected_signer, query_relay_at_with_keys,
        relay_api_base_url_with_override, submit_event_at_with_keys, SubmitEventResponse,
    },
};

mod actions;
mod compute;
mod proposals;
#[cfg(test)]
mod tests;
pub use actions::*;
pub use compute::*;

struct EconomyScope<'a> {
    state: &'a AppState,
    relay_url: String,
    keys: Keys,
}

impl<'a> EconomyScope<'a> {
    fn capture(
        state: &'a AppState,
        expected_relay: Option<&str>,
        expected_signer: Option<&str>,
    ) -> Result<Self, String> {
        let relay_url = relay_api_base_url_with_override(state);
        assert_expected_relay_scope(expected_relay, &relay_url)?;
        let keys = state.signing_keys()?;
        assert_expected_signer(expected_signer, &keys.public_key().to_hex())?;
        Ok(Self {
            state,
            relay_url,
            keys,
        })
    }

    async fn query(&self, filter: Value) -> Result<Vec<Event>, String> {
        let events =
            query_relay_at_with_keys(self.state, &self.relay_url, &[filter], &self.keys, None)
                .await?;
        for event in &events {
            buzz_core_pkg::verify_event(event).map_err(error)?;
        }
        Ok(events)
    }

    async fn project(&self, coordinate: &str) -> Result<BuzzProjectContext, String> {
        let (owner, slug) = project_parts(coordinate)?;
        let event = self
            .query(json!({"kinds":[KIND_PROJECT], "authors":[owner], "#d":[slug], "limit":1}))
            .await?
            .into_iter()
            .next()
            .ok_or("Project was not found")?;
        let context = BuzzProjectContext::from_event(&event).map_err(error)?;
        if context.project != coordinate {
            return Err("Project response does not match the selected project".into());
        }
        Ok(context)
    }

    async fn exact(&self, ids: &[String], kinds: &[u32]) -> Result<Vec<Event>, String> {
        if ids.is_empty() || ids.len() > 256 {
            return Err("Choose between 1 and 256 evidence events".into());
        }
        let expected: BTreeSet<_> = ids
            .iter()
            .map(|id| digest(id).map(hex::encode))
            .collect::<Result<_, _>>()?;
        if expected.len() != ids.len() {
            return Err("Duplicate evidence event ID".into());
        }
        let requested: Vec<_> = expected.iter().collect();
        let mut events = Vec::new();
        for batch in requested.chunks(100) {
            events.extend(
                self.query(json!({"ids":batch, "kinds":kinds, "limit":batch.len()}))
                    .await?,
            );
        }
        let actual: BTreeSet<_> = events.iter().map(|event| event.id.to_hex()).collect();
        if actual != expected
            || events
                .iter()
                .any(|event| !kinds.contains(&u32::from(event.kind.as_u16())))
        {
            return Err("Required signed evidence is missing or has the wrong kind".into());
        }
        Ok(events)
    }

    async fn event(&self, id: &str, kind: u32) -> Result<Event, String> {
        self.exact(&[id.to_owned()], &[kind])
            .await?
            .into_iter()
            .next()
            .ok_or("Event was not found".into())
    }

    async fn submit(&self, builder: EventBuilder) -> Result<SubmitEventResponse, String> {
        submit_event_at_with_keys(builder, self.state, &self.relay_url, &self.keys).await
    }
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn digest(value: &str) -> Result<[u8; 32], String> {
    hex::decode(value)
        .map_err(error)?
        .try_into()
        .map_err(|_| "Expected a 32-byte hexadecimal ID".into())
}

fn project_parts(project: &str) -> Result<(&str, &str), String> {
    let mut parts = project.splitn(3, ':');
    if parts.next() != Some("30621") {
        return Err("Invalid project coordinate".into());
    }
    let owner = parts.next().ok_or("Project owner is missing")?;
    let slug = parts
        .next()
        .filter(|slug| !slug.is_empty())
        .ok_or("Project identifier is missing")?;
    digest(owner)?;
    Ok((owner, slug))
}

fn unique_tag<'a>(event: &'a Event, name: &'a str) -> Result<&'a str, String> {
    let values: Vec<_> = tag_values(event, name).collect();
    if values.len() != 1 {
        return Err(format!("Expected one {name} tag"));
    }
    Ok(values[0])
}

fn tag_values<'a>(event: &'a Event, name: &'a str) -> impl Iterator<Item = &'a str> {
    event.tags.iter().filter_map(move |tag| {
        let parts = tag.as_slice();
        (parts.first().map(String::as_str) == Some(name))
            .then(|| parts.get(1).map(String::as_str))
            .flatten()
    })
}

fn public_evidence_kinds() -> Vec<u32> {
    ALL_KINDS
        .iter()
        .copied()
        .filter(|kind| !P_GATED_KINDS.contains(kind) && *kind != KIND_AUTH && !is_ephemeral(*kind))
        .collect()
}

fn public_evidence(project: &BuzzProjectContext, event: &Event) -> bool {
    !P_GATED_KINDS.contains(&u32::from(event.kind.as_u16()))
        && tag_values(event, "h").next().is_none()
        && project.evidence_ref(event).is_ok()
}

/// Read bounded, signed project accounting history from one captured community.
#[tauri::command]
pub async fn get_st8_project_economy(
    project: String,
    expected_relay_url: Option<String>,
    expected_signer_pubkey: Option<String>,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let scope = EconomyScope::capture(
        &state,
        expected_relay_url.as_deref(),
        expected_signer_pubkey.as_deref(),
    )?;
    let context = scope.project(&project).await?;
    let mut result = json!({"project_event":context.project_event, "viewer_pubkey":scope.keys.public_key().to_hex(),
        "relay_url":scope.relay_url, "has_more":false, "has_more_by_kind":{}});
    for (name, kind) in [
        ("policies", KIND_ST8_GOVERNANCE_POLICY),
        ("claims", KIND_ST8_CONTRIBUTION_CLAIM),
        ("proposals", KIND_ST8_DECISION_PROPOSAL),
        ("approvals", KIND_ST8_GOVERNANCE_APPROVAL),
        ("contribution_ledger", KIND_ST8_LEDGER_ENTRY),
        ("compute_receipts", KIND_ST8_COMPUTE_LEDGER_ENTRY),
        ("compute_settlements", KIND_ST8_COMPUTE_SETTLEMENT_ENTRY),
        ("compute_disputes", KIND_ST8_COMPUTE_DISPUTE),
        ("compute_resolutions", KIND_ST8_COMPUTE_RESOLUTION),
    ] {
        let mut filter = json!({"kinds":[kind], "#a":[project], "limit":100});
        if kind == KIND_ST8_GOVERNANCE_POLICY {
            filter["authors"] = json!([context.project_event.pubkey.to_hex()]);
        }
        let events = scope.query(filter).await?;
        if kind == KIND_ST8_GOVERNANCE_POLICY {
            for event in &events {
                st8_contribution_engine::GovernancePolicyContext::from_event(&context, event)
                    .map_err(error)?;
            }
        }
        // Ledger projection kinds are reserved to the serving relay at ingest;
        // source-event signatures alone do not imply an accepted accounting action.
        if events.iter().any(|event| {
            event.kind.as_u16() != kind as u16 || !tag_values(event, "a").any(|a| a == project)
        }) {
            return Err("Relay returned events outside the selected project".into());
        }
        if kind == KIND_ST8_DECISION_PROPOSAL {
            let (actionable, truncated) =
                proposals::actionable_proposals(&scope, &context, &events).await?;
            result["actionable_proposal_ids"] = json!(actionable);
            if truncated {
                result["has_more"] = json!(true);
            }
        }
        let has_more = events.len() == 100;
        result["has_more_by_kind"][kind.to_string()] = json!(has_more);
        if has_more {
            result["has_more"] = json!(true);
        }
        result[name] = json!(events);
    }
    let mut coordinates = context.repository_coordinates.clone();
    coordinates.push(project);
    let events = scope
        .query(json!({"kinds":public_evidence_kinds(), "#a":coordinates, "limit":100}))
        .await?;
    if events.len() == 100 {
        result["has_more"] = json!(true);
        result["has_more_by_kind"]["evidence"] = json!(true);
    }
    let mut evidence: Vec<_> = events
        .into_iter()
        .filter(|event| public_evidence(&context, event))
        .collect();
    // The signed project itself is valid evidence, including for a newly created
    // project whose first contribution predates other public repository activity.
    if !evidence
        .iter()
        .any(|event| event.id == context.project_event.id)
    {
        evidence.insert(0, context.project_event.clone());
        if evidence.len() > 100 {
            evidence.truncate(100);
            result["has_more"] = json!(true);
            result["has_more_by_kind"]["evidence"] = json!(true);
        }
    }
    result["evidence"] = json!(evidence);
    Ok(result)
}
