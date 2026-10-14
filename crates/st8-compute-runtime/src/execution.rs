use super::*;

pub(super) struct ExecuteArgs {
    pub(super) project_event: Event,
    pub(super) pricing_event: Event,
    pub(super) prompt: String,
    pub(super) model: String,
    pub(super) relay_url: String,
    pub(super) mesh_api_url: String,
    pub(super) mesh_console_url: String,
    pub(super) max_cost_sats: u64,
    pub(super) expires_in_secs: i64,
    pub(super) output: PathBuf,
    pub(super) private_output: PathBuf,
    pub(super) max_output_tokens: u32,
}

async fn execute_inner(args: ExecuteArgs) -> anyhow::Result<ComputeRunArtifact> {
    preflight(&args)?;
    if args.prompt.is_empty() {
        return Err(anyhow!("private prompt is empty"));
    }
    let requester = requester_keys()?;
    let provider = provider_keys()?;
    let project = BuzzProjectContext::from_event(&args.project_event)?;
    let pricing = PricingContext::from_event(&project, &args.pricing_event)?;
    if pricing.policy.provider != nostr_identity(&provider) {
        return Err(anyhow!(
            "provider key does not own the signed pricing policy"
        ));
    }
    validate_budget(&pricing.policy, args.max_output_tokens, args.max_cost_sats)?;
    let owner = mesh_owner()?;
    if owner.owner_id != pricing.policy.node_owner_id {
        return Err(anyhow!("local Mesh owner does not match pricing policy"));
    }
    // Lock the entire local owner, independent of output filename, model, or URL spelling.
    let mut node_lock = StateLock::acquire(&node_lock_path(&owner.owner_id)?)?;
    validate_private_local_provider(
        &fetch_mesh_json(&args.mesh_console_url, "api/status").await?,
        &args.model,
    )?;
    let before = target_counters(&fetch_models(&args.mesh_console_url).await?, &args.model)?;
    let input_commitment: [u8; 32] = Sha256::digest(args.prompt.as_bytes()).into();
    let provider_hex = provider.public_key().to_hex();
    let request_event = EventBuilder::new(
        Kind::Custom(KIND_JOB_REQUEST),
        serde_json::json!({
            "version": 1,
            "workload": "llm_inference",
            "model": args.model,
            "private_input": true,
            "input_commitment": hex::encode(input_commitment)
        })
        .to_string(),
    )
    .tags([
        tag(["a", project.project.as_str()])?,
        tag(["p", provider_hex.as_str()])?,
        tag(["st8-input", hex::encode(input_commitment).as_str()])?,
    ])
    .sign_with_keys(&requester)?;
    let now = chrono::Utc::now().timestamp();
    let expires_at = now
        .checked_add(args.expires_in_secs)
        .context("job expiration overflow")?;
    let mut nonce_bytes = Vec::from(request_event.id.as_bytes());
    nonce_bytes.extend_from_slice(
        &chrono::Utc::now()
            .timestamp_nanos_opt()
            .unwrap_or(0)
            .to_be_bytes(),
    );
    let nonce: [u8; 32] = Sha256::digest(nonce_bytes).into();
    let job = ComputeJob {
        project: project.project.clone(),
        requester: nostr_identity(&requester),
        provider: pricing.policy.provider.clone(),
        node_owner_id: owner.owner_id.clone(),
        workload: WorkloadKind::LlmInference,
        agent: None,
        model: args.model.clone(),
        pricing_policy_id: pricing.policy.id()?,
        input_commitment,
        request_event_id: *request_event.id.as_bytes(),
        max_cost_sats: args.max_cost_sats,
        created_at: now,
        expires_at,
        nonce,
    };
    let job_event = ComputeJobContext::event_builder(&job)?.sign_with_keys(&requester)?;
    let state_path = state_path(&args.output);
    let mut state = RunState {
        relay_url: args.relay_url.clone(),
        events: vec![request_event.clone(), job_event.clone()],
        published: 0,
        terminal: false,
        artifact: None,
        artifact_output: None,
    };
    write_json(&state_path, &state)?;
    state.publish_pending(&state_path).await?;
    let remaining = expires_at - chrono::Utc::now().timestamp();
    if remaining <= 0 {
        return Err(anyhow!("job expired before execution"));
    }

    let started_at_ms = chrono::Utc::now().timestamp_millis();
    let timer = Instant::now();
    // A dropped HTTP request does not prove native inference stopped; require operator recovery.
    node_lock.keep = true;
    let inference = async {
        let response = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?
            .post(format!(
                "{}/chat/completions",
                args.mesh_api_url.trim_end_matches('/')
            ))
            .timeout(Duration::from_secs(u64::try_from(remaining)?))
            .json(&serde_json::json!({
                "model": args.model,
                "stream": false,
                "max_tokens": args.max_output_tokens,
                "messages": [{"role": "user", "content": args.prompt}]
            }))
            .send()
            .await?;
        let status = response.status();
        let response_body: Value = response.json().await?;
        Ok::<_, anyhow::Error>((status, response_body))
    };
    let (status, response_body) = tokio::select! {
        result = inference => result?,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            return Err(anyhow!("execution cancelled"));
        }
    };
    node_lock.keep = false;
    let elapsed_ms = u64::try_from(timer.elapsed().as_millis())?;
    let ended_at_ms = chrono::Utc::now().timestamp_millis();
    let after = target_counters(&fetch_models(&args.mesh_console_url).await?, &args.model)?;
    let (execution_status, measured, private_result) = if status.is_success() {
        let prompt_tokens = response_body
            .pointer("/usage/prompt_tokens")
            .and_then(Value::as_u64);
        let completion_tokens = response_body
            .pointer("/usage/completion_tokens")
            .and_then(Value::as_u64);
        let content = response_body
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .context("Mesh response omitted assistant content")?;
        write_private(&args.private_output, content.as_bytes())?;
        (
            ExecutionStatus::Completed,
            derive_completed_execution(
                &before,
                &after,
                prompt_tokens,
                completion_tokens,
                elapsed_ms,
            )?,
            content.as_bytes().to_vec(),
        )
    } else {
        (
            ExecutionStatus::Failed,
            derive_failed_execution(&before, &after, elapsed_ms)?,
            serde_json::to_vec(&response_body)?,
        )
    };
    finalize_execution(
        args,
        project,
        pricing,
        owner,
        provider,
        request_event,
        job_event,
        job,
        execution_status,
        measured,
        private_result,
        started_at_ms,
        ended_at_ms,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn finalize_execution(
    args: ExecuteArgs,
    project: BuzzProjectContext,
    pricing: PricingContext,
    owner: MeshOwner,
    provider: Keys,
    request_event: Event,
    job_event: Event,
    job: ComputeJob,
    status: ExecutionStatus,
    measured: MeasuredMeshExecution,
    private_result: Vec<u8>,
    started_at_ms: i64,
    ended_at_ms: i64,
) -> anyhow::Result<ComputeRunArtifact> {
    let output_commitment: [u8; 32] = Sha256::digest(&private_result).into();
    let request_hex = request_event.id.to_hex();
    let result_event = EventBuilder::new(
        Kind::Custom(KIND_JOB_RESULT),
        serde_json::json!({
            "version": 1,
            "status": status,
            "model": args.model,
            "private_output": true,
            "output_commitment": hex::encode(output_commitment)
        })
        .to_string(),
    )
    .tags([
        tag(["a", project.project.as_str()])?,
        tag(["e", request_hex.as_str()])?,
        tag(["st8-request", request_hex.as_str()])?,
        tag(["st8-output", hex::encode(output_commitment).as_str()])?,
    ])
    .sign_with_keys(&provider)?;
    let price = pricing.policy.price(status, &measured.usage)?;
    let receipt = ComputeReceipt {
        job_id: job.id()?,
        project: job.project.clone(),
        requester: job.requester.clone(),
        provider: job.provider.clone(),
        node_owner_id: job.node_owner_id.clone(),
        workload: job.workload,
        agent: job.agent.clone(),
        model: job.model.clone(),
        pricing_policy_id: job.pricing_policy_id,
        request_event_id: job.request_event_id,
        result_event_id: *result_event.id.as_bytes(),
        agent_metric_event_id: None,
        started_at_ms,
        ended_at_ms,
        usage: measured.usage,
        price,
        result_commitment: output_commitment,
        status,
        routing_target: measured.routing_target,
        meter_version: "mesh-openai-target-delta-v1".into(),
        evidence: Vec::new(),
    };
    receipt.validate_for(&job, &pricing.policy)?;
    let mut attestation = NodeReceiptAttestation {
        receipt,
        node_public_key: owner.public_key,
        node_signature: Vec::new(),
    };
    attestation.node_signature = owner.sign(&attestation.signing_bytes()?)?;
    let receipt_event = attestation.event_builder()?.sign_with_keys(&provider)?;
    let verified = VerifiedComputeReceipt::from_events(
        project,
        &pricing.event,
        &job_event,
        &request_event,
        &result_event,
        &receipt_event,
    )?;
    let artifact = ComputeRunArtifact {
        version: 1,
        verified,
        output_commitment: hex::encode(output_commitment),
        private_output_persisted: status == ExecutionStatus::Completed,
    };
    let state_path = state_path(&args.output);
    let mut state: RunState = serde_json::from_slice(&std::fs::read(&state_path)?)?;
    state.events.extend([result_event, receipt_event]);
    state.terminal = true;
    state.artifact = Some(artifact.clone());
    state.artifact_output = Some(args.output.clone());
    write_json(&state_path, &state)?;
    state.restore_artifact(&state_path)?;
    state.publish_pending(&state_path).await?;
    Ok(artifact)
}

#[derive(Serialize, Deserialize)]
struct RunState {
    relay_url: String,
    events: Vec<Event>,
    published: usize,
    terminal: bool,
    #[serde(default)]
    artifact: Option<ComputeRunArtifact>,
    #[serde(default)]
    artifact_output: Option<PathBuf>,
}

impl RunState {
    fn restore_artifact(&self, journal: &Path) -> anyhow::Result<()> {
        let Some(artifact) = &self.artifact else {
            if self.artifact_output.is_some() {
                return Err(anyhow!("journal output has no saved artifact"));
            }
            return Ok(());
        };
        artifact.verified.verify()?;
        let expected_events = [
            &artifact.verified.request_event,
            &artifact.verified.job.event,
            &artifact.verified.result_event,
            &artifact.verified.receipt_event,
        ];
        if !self.terminal
            || self.events.iter().collect::<Vec<_>>() != expected_events
            || artifact.version != 1
            || artifact.output_commitment
                != hex::encode(artifact.verified.attestation.receipt.result_commitment)
        {
            return Err(anyhow!("saved artifact does not match journal events"));
        }
        let output = absolute_output(
            self.artifact_output
                .as_deref()
                .context("journal has no artifact output")?,
        )?;
        if state_path(&output) != absolute_output(journal)? {
            return Err(anyhow!("artifact output does not belong to this journal"));
        }
        if output.exists() {
            let existing: Value = serde_json::from_slice(&std::fs::read(&output)?)?;
            if existing != serde_json::to_value(artifact)? {
                return Err(anyhow!("refusing to replace a different existing artifact"));
            }
        } else {
            write_json(&output, artifact)?;
        }
        Ok(())
    }

    async fn publish_pending(&mut self, path: &Path) -> anyhow::Result<()> {
        if self.published > self.events.len() {
            return Err(anyhow!("invalid publication cursor"));
        }
        let requester = requester_keys()?;
        let provider = provider_keys()?;
        self.validate(&requester, &provider)?;
        while let Some(event) = self.events.get(self.published) {
            let keys = if event.pubkey == requester.public_key() {
                &requester
            } else if event.pubkey == provider.public_key() {
                &provider
            } else {
                return Err(anyhow!(
                    "saved event signer does not match configured identities"
                ));
            };
            publish(&self.relay_url, event.clone(), keys).await?;
            self.published += 1;
            write_json(path, self)?;
        }
        Ok(())
    }

    fn validate(&self, requester: &Keys, provider: &Keys) -> anyhow::Result<()> {
        for event in &self.events {
            buzz_core::verify_event(event)?;
        }
        let request = self.events.first().context("state has no request")?;
        let authorization = self.events.get(1).context("state has no authorization")?;
        let job: ComputeJob = serde_json::from_str(&authorization.content)?;
        job.id()?;
        if request.kind != Kind::Custom(KIND_JOB_REQUEST)
            || authorization.kind != Kind::Custom(49_811)
            || request.pubkey != requester.public_key()
            || authorization.pubkey != requester.public_key()
            || job.requester != nostr_identity(requester)
            || job.provider != nostr_identity(provider)
            || job.request_event_id != *request.id.as_bytes()
            || self.published > self.events.len()
        {
            return Err(anyhow!(
                "saved run does not match configured requester/provider identities"
            ));
        }
        Ok(())
    }

    fn terminate(&mut self, status: &str, provider: &Keys) -> anyhow::Result<()> {
        if self.terminal {
            return Ok(());
        }
        let request = self.events.first().context("state has no request")?;
        let project = request
            .tags
            .iter()
            .find_map(|tag| {
                let parts = tag.as_slice();
                (parts.first().map(String::as_str) == Some("a"))
                    .then(|| parts.get(1))
                    .flatten()
            })
            .context("request has no project")?;
        let id = request.id.to_hex();
        // Unknown usage cannot support a billable receipt. Publish only terminal evidence.
        let event = EventBuilder::new(
            Kind::Custom(KIND_JOB_RESULT),
            serde_json::json!({"version": 1, "status": status, "private_output": true,
                "accounting": "unavailable", "reason": "execution_not_receipted"})
            .to_string(),
        )
        .tags([
            tag(["a", project.as_str()])?,
            tag(["e", id.as_str()])?,
            tag(["st8-request", id.as_str()])?,
        ])
        .sign_with_keys(provider)?;
        self.events.push(event);
        self.terminal = true;
        Ok(())
    }
}

fn state_path(output: &Path) -> PathBuf {
    output.with_extension("state.json")
}

fn node_lock_path(owner_id: &str) -> anyhow::Result<PathBuf> {
    let keystore = default_keystore_path()
        .map_err(|error| anyhow!("cannot resolve Mesh keystore: {error}"))?;
    let parent = keystore.parent().context("keystore has no parent")?;
    Ok(parent.join(format!(
        ".st8-compute-{}",
        hex::encode(Sha256::digest(owner_id.as_bytes()))
    )))
}

struct StateLock {
    path: PathBuf,
    keep: bool,
}
impl StateLock {
    fn acquire(path: &Path) -> anyhow::Result<Self> {
        let lock = path.with_extension("lock");
        if let Some(parent) = lock.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir(&lock)
            .context("run is locked; after a crash remove its .lock directory before resume")?;
        Ok(Self {
            path: lock,
            keep: false,
        })
    }
}
impl Drop for StateLock {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir(&self.path);
        }
    }
}

pub(super) async fn execute(mut args: ExecuteArgs) -> anyhow::Result<ComputeRunArtifact> {
    args.output = absolute_output(&args.output)?;
    args.private_output = absolute_output(&args.private_output)?;
    preflight(&args)?;
    let path = state_path(&args.output);
    let _lock = StateLock::acquire(&path)?;
    if path.exists() || args.output.exists() || args.private_output.exists() {
        return Err(anyhow!(
            "run outputs already exist; use resume for saved publications"
        ));
    }
    let result = execute_inner(args).await;
    if result.is_err() && path.exists() {
        let mut state: RunState = serde_json::from_slice(&std::fs::read(&path)?)?;
        if !state.terminal {
            let status = if result
                .as_ref()
                .err()
                .is_some_and(|error| error.to_string() == "execution cancelled")
            {
                "cancelled"
            } else {
                "failed"
            };
            state.terminate(status, &provider_keys()?)?;
            write_json(&path, &state)?;
            if let Err(error) = state.publish_pending(&path).await {
                eprintln!(
                    "terminal publication pending; resume {}: {error}",
                    path.display()
                );
            }
        }
    }
    result
}

pub(super) async fn resume(path: &Path) -> anyhow::Result<()> {
    let _lock = StateLock::acquire(path)?;
    let mut state: RunState = serde_json::from_slice(&std::fs::read(path)?)?;
    state.validate(&requester_keys()?, &provider_keys()?)?;
    state.restore_artifact(path)?;
    // A crash may have happened after dispatch; never run the same private input twice.
    state.terminate("cancelled", &provider_keys()?)?;
    write_json(path, &state)?;
    state.publish_pending(path).await
}

fn preflight(args: &ExecuteArgs) -> anyhow::Result<()> {
    for endpoint in [&args.mesh_api_url, &args.mesh_console_url] {
        let url = url::Url::parse(endpoint)?;
        let local = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if !local || !matches!(url.scheme(), "http" | "https") {
            return Err(anyhow!(
                "private Mesh endpoints must use literal loopback addresses"
            ));
        }
    }

    if !(1..=3600).contains(&args.expires_in_secs) || !(1..=32768).contains(&args.max_output_tokens)
    {
        return Err(anyhow!(
            "expiry must be 1..3600 seconds and output budget 1..32768 tokens"
        ));
    }
    if args.private_output == args.output || args.private_output == state_path(&args.output) {
        return Err(anyhow!(
            "private output must be separate from public receipt and state"
        ));
    }
    if args.prompt.len() > 1024 * 1024 {
        return Err(anyhow!("private prompt exceeds 1 MiB"));
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .context("cannot create private answer file")?;
    file.write_all(bytes)?;
    file.sync_all()?;
    #[cfg(unix)]
    std::fs::File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()?;
    Ok(())
}

fn absolute_output(path: &Path) -> anyhow::Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path.file_name().context("output has no filename")?;
    Ok(std::fs::canonicalize(parent)?.join(name))
}

fn validate_budget(policy: &PricingPolicy, max_output_tokens: u32, cap: u64) -> anyhow::Result<()> {
    use st8_compute_protocol::{MeasurementQuality, MeteredQuantity};
    let minimum = [
        (MeterKind::InputTokens, 1),
        (MeterKind::OutputTokens, u64::from(max_output_tokens)),
        (MeterKind::WallTimeMs, 1),
        (MeterKind::Jobs, 1),
    ]
    .into_iter()
    .map(|(kind, quantity)| MeteredQuantity {
        kind,
        quantity,
        quality: MeasurementQuality::HostExact,
        source: "preflight.budget".into(),
    })
    .collect::<Vec<_>>();
    if policy
        .price(ExecutionStatus::Completed, &minimum)?
        .total_sats
        > cap
    {
        return Err(anyhow!(
            "cost cap cannot fund fixed/minimum input usage and requested output budget"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unaffordable_execution_before_dispatch() {
        let event: Event =
            serde_json::from_str(include_str!("../../../docs/st8wrx/compute-m1/pricing.json"))
                .unwrap();
        let policy: PricingPolicy = serde_json::from_str(&event.content).unwrap();
        assert!(validate_budget(&policy, 512, 0).is_err());
        assert!(validate_budget(&policy, 512, 100).is_ok());
        assert!(validate_budget(&policy, 32768, 2).is_err());
    }

    #[test]
    fn private_answer_is_exclusive_and_restricted() {
        let path = std::env::temp_dir().join(format!(
            "st8-private-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        write_private(&path, b"private answer").unwrap();
        assert!(write_private(&path, b"replacement").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"private answer");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn journal_retains_exact_signed_events_and_cursor() {
        let artifact: ComputeRunArtifact =
            serde_json::from_str(include_str!("../../../docs/st8wrx/compute-m1/receipt.json"))
                .unwrap();
        let events = vec![
            artifact.verified.request_event.clone(),
            artifact.verified.job.event.clone(),
            artifact.verified.result_event.clone(),
            artifact.verified.receipt_event.clone(),
        ];
        let state = RunState {
            relay_url: "ws://localhost:3000".into(),
            events,
            published: 2,
            terminal: true,
            artifact: None,
            artifact_output: None,
        };
        let recovered: RunState =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(recovered.published, 2);
        assert_eq!(recovered.events, state.events);
        assert!(recovered.terminal);
    }
    #[test]
    fn interrupted_execution_has_one_durable_terminal_event_without_charge() {
        let artifact: ComputeRunArtifact =
            serde_json::from_str(include_str!("../../../docs/st8wrx/compute-m1/receipt.json"))
                .unwrap();
        let mut state = RunState {
            relay_url: "ws://localhost:3000".into(),
            events: vec![artifact.verified.request_event],
            published: 1,
            terminal: false,
            artifact: None,
            artifact_output: None,
        };
        let provider = Keys::generate();
        state.terminate("cancelled", &provider).unwrap();
        let id = state.events[1].id;
        state.terminate("failed", &provider).unwrap();
        assert_eq!(state.events.len(), 2);
        assert_eq!(state.events[1].id, id);
        let body: Value = serde_json::from_str(&state.events[1].content).unwrap();
        assert_eq!(body["status"], "cancelled");
        assert_eq!(body["accounting"], "unavailable");
        assert!(body.get("price").is_none());
        buzz_core::verify_event(&state.events[1]).unwrap();
    }

    #[tokio::test]
    async fn invalid_resume_cursor_fails_before_publication() {
        let mut state = RunState {
            relay_url: "not a url".into(),
            events: vec![],
            published: 1,
            terminal: true,
            artifact: None,
            artifact_output: None,
        };
        assert!(state
            .publish_pending(Path::new("unused"))
            .await
            .unwrap_err()
            .to_string()
            .contains("cursor"));
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    fn directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "st8-recovery-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn public_writes_preserve_private_answer_and_old_temporary_symlink() {
        let dir = directory();
        let answer = dir.join(".receipt.json.tmp");
        write_private(&answer, b"answer").unwrap();
        write_json(
            &dir.join("receipt.json"),
            &serde_json::json!({"public": true}),
        )
        .unwrap();
        assert_eq!(std::fs::read(&answer).unwrap(), b"answer");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&answer, dir.join(".state.json.tmp")).unwrap();
            write_json(
                &dir.join("state.json"),
                &serde_json::json!({"public": true}),
            )
            .unwrap();
            assert_eq!(std::fs::read(&answer).unwrap(), b"answer");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn terminal_journal_recovers_missing_artifact_and_rejects_wrong_destination() {
        let dir = directory();
        let output = dir.join("receipt.json");
        let journal = state_path(&output);
        let artifact: ComputeRunArtifact =
            serde_json::from_str(include_str!("../../../docs/st8wrx/compute-m1/receipt.json"))
                .unwrap();
        let mut state = RunState {
            relay_url: "ws://127.0.0.1:3000".into(),
            events: vec![
                artifact.verified.request_event.clone(),
                artifact.verified.job.event.clone(),
                artifact.verified.result_event.clone(),
                artifact.verified.receipt_event.clone(),
            ],
            published: 2,
            terminal: true,
            artifact: Some(artifact),
            artifact_output: Some(output.clone()),
        };
        write_json(&journal, &state).unwrap();
        // Simulate an output write failure after the terminal journal was persisted.
        std::fs::create_dir(&output).unwrap();
        assert!(state.restore_artifact(&journal).is_err());
        std::fs::remove_dir(&output).unwrap();
        let recovered: RunState =
            serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
        recovered.restore_artifact(&journal).unwrap();
        recovered.restore_artifact(&journal).unwrap();
        let artifact: ComputeRunArtifact =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        artifact.verified.verify().unwrap();
        state.artifact_output = Some(dir.join("unrelated.json"));
        assert!(state.restore_artifact(&journal).is_err());
        assert!(!dir.join("unrelated.json").exists());
        state.artifact_output = Some(output.clone());
        state.events.swap(2, 3);
        assert!(state.restore_artifact(&journal).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn node_lock_excludes_concurrent_runs_and_keeps_uncertain_execution_locked() {
        assert_eq!(
            node_lock_path("owner").unwrap(),
            node_lock_path("owner").unwrap()
        );
        assert_ne!(
            node_lock_path("owner").unwrap(),
            node_lock_path("other").unwrap()
        );
        let dir = directory();
        let path = dir.join("node");
        let lock = StateLock::acquire(&path).unwrap();
        assert!(StateLock::acquire(&path).is_err());
        drop(lock);
        let mut lock = StateLock::acquire(&path).unwrap();
        lock.keep = true;
        drop(lock);
        assert!(StateLock::acquire(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
