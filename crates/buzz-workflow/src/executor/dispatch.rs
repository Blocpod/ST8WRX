use super::*;

pub(super) fn resolve_send_message_channel(
    explicit_channel: Option<&str>,
    trigger_channel: &str,
    workflow_channel_id: Option<Uuid>,
) -> Result<String, WorkflowError> {
    let explicit_channel = explicit_channel
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(workflow_channel_id) = workflow_channel_id {
        if let Some(explicit_channel) = explicit_channel {
            let override_channel_id = explicit_channel.parse::<Uuid>().map_err(|e| {
                WorkflowError::InvalidDefinition(format!(
                    "SendMessage: invalid channel override UUID: {e}"
                ))
            })?;
            if override_channel_id != workflow_channel_id {
                return Err(WorkflowError::InvalidDefinition(format!(
                    "SendMessage: channel override must match the workflow channel ({workflow_channel_id})"
                )));
            }
        }
        return Ok(workflow_channel_id.to_string());
    }

    if let Some(explicit_channel) = explicit_channel {
        let override_channel_id = explicit_channel.parse::<Uuid>().map_err(|e| {
            WorkflowError::InvalidDefinition(format!(
                "SendMessage: invalid channel override UUID: {e}"
            ))
        })?;
        return Ok(override_channel_id.to_string());
    }

    if trigger_channel.trim().is_empty() {
        return Err(WorkflowError::InvalidDefinition(
            "SendMessage: no channel_id available (trigger has no channel context and no channel override was specified)"
                .into(),
        ));
    }

    Ok(trigger_channel.trim().to_string())
}

/// Dispatch a resolved action and return its output.
///
/// For MVP, most actions log their intent and return a success output.
/// Real event emission is wired in WF-07/08 (relay integration).
///
/// `RequestApproval` returns `StepResult::Suspended` — the caller must
/// persist state and stop the execution loop.
pub async fn dispatch_action(
    step_id: &str,
    action: &ActionDef,
    engine: &WorkflowEngine,
    community_id: CommunityId,
    run_id: Uuid,
    trigger_ctx: &TriggerContext,
) -> Result<StepResult, WorkflowError> {
    use ActionDef::*;

    // The workflow engine can outlive the serving request that spawned it.
    // Revalidate the durable community fence immediately before every external
    // side effect (message publish, webhook, delay/resume). A storage failure is
    // a denial, never permission to continue.
    let serving_write =
        buzz_deletion::acquire_serving_write(&engine.db, community_id, "workflow_action")
            .await
            .map_err(|error| {
                WorkflowError::WebhookError(format!(
                    "community write fence rejected workflow side effect: {error}"
                ))
            })?;

    serving_write.verify().await.map_err(|error| {
        WorkflowError::WebhookError(format!("community write lease lost: {error}"))
    })?;

    let result = serving_write
        .protect(async {
            match action {
                SendMessage {
                    text,
                    channel,
                    reply_in_thread,
                } => {
                    // Look up workflow metadata for destination validation and
                    // attribution, scoped to the run's community — the same run/workflow
                    // UUID may exist in another community, so a bare-id lookup could
                    // load the wrong row and drive a side effect under it.
                    let wf_run = engine
                        .db
                        .get_workflow_run(community_id, run_id)
                        .await
                        .map_err(|e| {
                            WorkflowError::WebhookError(format!(
                                "SendMessage: failed to load workflow run {run_id}: {e}"
                            ))
                        })?;
                    let workflow = engine
                        .db
                        .get_workflow(community_id, wf_run.workflow_id)
                        .await
                        .map_err(|e| {
                            WorkflowError::WebhookError(format!(
                                "SendMessage: failed to load workflow {}: {e}",
                                wf_run.workflow_id
                            ))
                        })?;
                    let channel_id = resolve_send_message_channel(
                        channel.as_deref(),
                        &trigger_ctx.channel_id,
                        workflow.channel_id,
                    )?;
                    let owner_pubkey_hex = hex::encode(&workflow.owner_pubkey);

                    // Thread the reply onto the triggering message when requested.
                    // The trigger must carry the event to reply to; schema
                    // validation already forbids `reply_in_thread` on triggers
                    // that have no message, so an empty id here is a real fault.
                    let reply_to = if *reply_in_thread {
                        if trigger_ctx.message_id.is_empty() {
                            return Err(WorkflowError::InvalidDefinition(
                                "SendMessage: reply_in_thread is set but the trigger has no message_id to reply to".into(),
                            ));
                        }
                        Some(trigger_ctx.message_id.as_str())
                    } else {
                        None
                    };

                    info!(
                        run_id = %run_id,
                        step = step_id,
                        channel = %channel_id,
                        reply_in_thread = *reply_in_thread,
                        "SendMessage → {channel_id}: {text}"
                    );

                    let event_id = engine
                        .action_sink()?
                        .send_message(
                            community_id,
                            &channel_id,
                            text,
                            &owner_pubkey_hex,
                            reply_to,
                        )
                        .await
                        .map_err(WorkflowError::from)?;

                    Ok(StepResult::Completed(serde_json::json!({
                        "sent": true,
                        "event_id": event_id,
                    })))
                }

                SendDm { .. } | SetChannelTopic { .. } | AddReaction { .. } => {
                    use crate::action_sink::WorkflowSideEffect;
                    let run = engine.db.get_workflow_run(community_id, run_id).await?;
                    let workflow = engine.db.get_workflow(community_id, run.workflow_id).await?;
                    let channel = workflow.channel_id.ok_or_else(|| WorkflowError::InvalidDefinition("workflow action requires a channel".into()))?;
                    let effect = match action {
                        SendDm { to, text } => WorkflowSideEffect::SendDm { to: to.clone(), text: text.clone() },
                        SetChannelTopic { topic } => WorkflowSideEffect::SetChannelTopic { topic: topic.clone() },
                        AddReaction { emoji } => WorkflowSideEffect::AddReaction { message_id: trigger_ctx.message_id.clone(), emoji: emoji.clone() },
                        _ => return Err(WorkflowError::InvalidDefinition("unsupported action".into())),
                    };
                    let event_id = engine.action_sink()?.perform(community_id, &channel.to_string(), &hex::encode(workflow.owner_pubkey), effect).await?;
                    Ok(StepResult::Completed(serde_json::json!({"event_id": event_id})))
                }

                CallWebhook {
                    url,
                    method,
                    headers,
                    body,
                } => {
                    let method_str = method.as_deref().unwrap_or("POST");
                    info!(run_id = %run_id, step = step_id, "CallWebhook → {method_str} {url}");

                    #[cfg(feature = "reqwest")]
                    {
                        let result = call_webhook_impl(url, method_str, headers, body).await?;
                        Ok(StepResult::Completed(result))
                    }

                    #[cfg(not(feature = "reqwest"))]
                    {
                        // reqwest not enabled — log and return placeholder.
                        warn!(
                            run_id = %run_id, step = step_id,
                            "CallWebhook: reqwest feature not enabled, skipping HTTP call"
                        );
                        let _ = (headers, body); // suppress unused warnings
                        Ok(StepResult::Completed(serde_json::json!({
                            "status": 0,
                            "body": null,
                            "skipped": true
                        })))
                    }
                }

                RequestApproval {
                    from,
                    message,
                    timeout,
                } => {
                    let timeout_str = timeout.as_deref().unwrap_or("24h");
                    info!(
                        run_id = %run_id, step = step_id,
                        "RequestApproval from={from} timeout={timeout_str}: {message}"
                    );

                    let token = generate_approval_token(run_id, step_id);

                    validate_approval_spec(from)?;
                    let duration = parse_duration_secs(timeout_str)?;
                    if duration == 0 || duration > 30 * 24 * 3600 {
                        return Err(WorkflowError::InvalidDefinition("approval timeout must be between one second and 30 days".into()));
                    }

                    Ok(StepResult::Suspended {
                        approval_token: token,
                    })
                }

                Delay { duration } => {
                    let secs = parse_duration_secs(duration)?;
                    // Cap delay at 270 seconds (4.5 minutes) — must be less than default_timeout_secs (300s)
                    // to avoid non-deterministic StepTimeout. Long delays (hours/days)
                    // should use the scheduled resume pattern (future work: WF-09).
                    const MAX_DELAY_SECS: u64 = 270;
                    if secs > MAX_DELAY_SECS {
                        return Err(WorkflowError::InvalidDefinition(format!(
                            "delay exceeds maximum of {MAX_DELAY_SECS} seconds (got {secs}s); \
                     use the scheduled resume pattern for long delays"
                        )));
                    }
                    info!(run_id = %run_id, step = step_id, "Delay {duration} ({secs}s)");
                    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                    Ok(StepResult::Completed(
                        serde_json::json!({ "slept_secs": secs }),
                    ))
                }
            }
        })
        .await
        .map_err(|error| {
            WorkflowError::WebhookError(format!("community write lease lost: {error}"))
        })?;
    let release = serving_write.finish().await.map_err(|error| {
        WorkflowError::WebhookError(format!("community write lease release failed: {error}"))
    });
    match result {
        Ok(value) => {
            release?;
            Ok(value)
        }
        Err(error) => {
            let _ = release;
            Err(error)
        }
    }
}

fn validate_approval_spec(spec: &str) -> Result<(), WorkflowError> {
    if spec == "any" || (spec.len() == 64 && nostr::PublicKey::from_hex(spec).is_ok()) {
        Ok(())
    } else {
        Err(WorkflowError::InvalidDefinition(
            "approval from must be any or an exact public key".into(),
        ))
    }
}

/// Generate a cryptographically random approval token.
///
/// Uses `Uuid::new_v4()` which draws from the OS CSPRNG (via the `getrandom`
/// crate). The `run_id` and `step_id` parameters are accepted for logging
/// context but are not mixed into the token — the UUID's own randomness is
/// sufficient and avoids the predictability of time-based entropy.
pub(super) fn generate_approval_token(_run_id: Uuid, _step_id: &str) -> String {
    Uuid::new_v4().to_string()
}

/// Parse a duration string like "5m", "1h", "30s" into seconds.
///
/// Exposed as `pub(crate)` so `schema.rs` can use it for interval validation.
pub(crate) fn parse_duration_secs(duration: &str) -> Result<u64, WorkflowError> {
    let duration = duration.trim();
    if let Some(n) = duration.strip_suffix('h') {
        let hours: u64 = n.trim().parse().map_err(|_| {
            WorkflowError::InvalidDefinition(format!("invalid duration: {duration}"))
        })?;
        return hours.checked_mul(3600).ok_or_else(|| {
            WorkflowError::InvalidDefinition(format!("duration overflow: {duration}"))
        });
    }
    if let Some(n) = duration.strip_suffix('m') {
        let mins: u64 = n.trim().parse().map_err(|_| {
            WorkflowError::InvalidDefinition(format!("invalid duration: {duration}"))
        })?;
        return mins.checked_mul(60).ok_or_else(|| {
            WorkflowError::InvalidDefinition(format!("duration overflow: {duration}"))
        });
    }
    if let Some(n) = duration.strip_suffix('s') {
        let secs: u64 = n.trim().parse().map_err(|_| {
            WorkflowError::InvalidDefinition(format!("invalid duration: {duration}"))
        })?;
        return Ok(secs);
    }
    // Plain number — assume seconds.
    duration
        .parse()
        .map_err(|_| WorkflowError::InvalidDefinition(format!("invalid duration: {duration}")))
}

// is_private_ip is provided by buzz_core::network::is_private_ip

/// Resolve `host` to IP addresses and reject if any are private/reserved.
///
/// Uses the OS resolver (blocking, run on a threadpool via `spawn_blocking`).
/// Rejects the request if DNS resolution fails or returns zero addresses.
///
/// Returns the first validated IP address so the caller can pin DNS resolution
/// in the HTTP client, preventing DNS rebinding TOCTOU attacks.
#[cfg(feature = "reqwest")]
pub(super) async fn check_ssrf(host: &str, port: u16) -> Result<std::net::IpAddr, WorkflowError> {
    let addr_str = format!("{host}:{port}");
    let addrs: Vec<std::net::IpAddr> = tokio::task::spawn_blocking(move || {
        use std::net::ToSocketAddrs;
        addr_str
            .to_socket_addrs()
            .map(|iter| iter.map(|sa| sa.ip()).collect::<Vec<_>>())
    })
    .await
    .map_err(|e| WorkflowError::WebhookError(format!("SSRF check task failed: {e}")))?
    .map_err(|e| WorkflowError::WebhookError(format!("DNS resolution failed: {e}")))?;

    if addrs.is_empty() {
        return Err(WorkflowError::WebhookError(
            "DNS resolution returned no addresses".into(),
        ));
    }

    debug!("Resolved webhook host '{}' → {:?}", host, addrs);

    for ip in &addrs {
        if buzz_core::network::is_private_ip(ip) {
            return Err(WorkflowError::WebhookError(format!(
                "SSRF blocked: '{host}' resolved to private/reserved address {ip}"
            )));
        }
    }

    Ok(addrs[0])
}

/// Maximum response body size for webhook calls (1 MiB).
#[cfg(feature = "reqwest")]
const WEBHOOK_MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[cfg(feature = "reqwest")]
async fn call_webhook_impl(
    url: &str,
    method: &str,
    headers: &Option<std::collections::HashMap<String, String>>,
    body: &Option<String>,
) -> Result<JsonValue, WorkflowError> {
    use reqwest::Client;
    use std::time::Duration;

    let parsed_url = reqwest::Url::parse(url)
        .map_err(|e| WorkflowError::WebhookError(format!("invalid URL: {e}")))?;

    let host = parsed_url
        .host_str()
        .ok_or_else(|| WorkflowError::WebhookError("URL has no host".into()))?;

    // Default ports: 443 for https, 80 for http.
    let port = parsed_url.port_or_known_default().unwrap_or(80);

    let safe_ip = check_ssrf(host, port).await?;

    // Client is built per-request because `resolve()` pins DNS for a specific host.
    // This disables connection pooling but is required for SSRF safety: without
    // pinning, reqwest performs its own DNS resolution which could return a
    // different address than the one validated above (DNS rebinding TOCTOU).
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        // A system proxy would resolve the original hostname itself, bypassing
        // the validated and pinned address above.
        .no_proxy()
        // Disable redirects — a redirect to an internal host bypasses the SSRF check.
        .redirect(reqwest::redirect::Policy::none())
        .resolve(host, std::net::SocketAddr::new(safe_ip, port))
        .build()
        .map_err(|e| WorkflowError::WebhookError(e.to_string()))?;

    let method_parsed = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|e| WorkflowError::WebhookError(e.to_string()))?;

    let mut req = client.request(method_parsed, url);

    if let Some(hdrs) = headers {
        for (k, v) in hdrs {
            req = req.header(k, v);
        }
    }

    if let Some(b) = body {
        req = req.body(b.clone());
    }

    let resp = req
        .send()
        .await
        .map_err(|e| WorkflowError::WebhookError(e.to_string()))?;

    let status = resp.status().as_u16();

    // Read incrementally to prevent OOM from a malicious server returning a
    // multi-GB payload. `resp.bytes()` would buffer the entire body before we
    // could check the size; chunked reading lets us abort early.
    let mut body_bytes = Vec::new();
    let mut resp = resp;
    loop {
        let chunk = resp
            .chunk()
            .await
            .map_err(|e| WorkflowError::WebhookError(format!("reading response body: {e}")))?;
        match chunk {
            Some(bytes) => {
                body_bytes.extend_from_slice(&bytes);
                if body_bytes.len() > WEBHOOK_MAX_RESPONSE_BYTES {
                    return Err(WorkflowError::WebhookError(format!(
                        "response body exceeds {} byte limit",
                        WEBHOOK_MAX_RESPONSE_BYTES
                    )));
                }
            }
            None => break,
        }
    }

    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

    Ok(serde_json::json!({
        "status": status,
        "body": body_text,
    }))
}

#[cfg(test)]
mod approval_tests {
    use super::*;

    #[test]
    fn approval_specs_fail_closed_before_persisting_a_gate() {
        assert!(validate_approval_spec("any").is_ok());
        assert!(validate_approval_spec(&nostr::Keys::generate().public_key().to_hex()).is_ok());
        for invalid in ["", "@manager", "owner", "anyone", "../user", "not-a-key"] {
            assert!(validate_approval_spec(invalid).is_err(), "{invalid}");
        }
    }
}
