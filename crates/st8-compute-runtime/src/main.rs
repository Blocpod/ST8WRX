#![deny(unsafe_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use clap::{Parser, Subcommand};
use mesh_llm_host_runtime::crypto::{
    default_keystore_path, keystore_exists, load_keystore, save_keystore, OwnerKeypair,
};
use mesh_llm_sdk::{serve, MeshDiscoveryMode};
use nostr::{Event, EventBuilder, Keys, Kind, Tag};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use st8_compute_engine::{
    ComputeDispute, ComputeJobContext, ComputeResolution, NodeCapabilityAttestation,
    NodeReceiptAttestation, PricingContext, SettlementContext, VerifiedComputeReceipt,
};
use st8_compute_protocol::{
    AcceleratorCapability, ComputeJob, ComputeReceipt, ComputeSettlement, ExecutionStatus,
    MeterKind, ModelCapability, PriceRate, PricingPolicy, WorkloadKind,
};
use st8_compute_runtime::{
    derive_completed_execution, derive_failed_execution, target_counters,
    validate_private_local_provider, MeasuredMeshExecution,
};
use st8_contribution_engine::BuzzProjectContext;

mod execution;
use execution::{execute, ExecuteArgs};

const KIND_JOB_REQUEST: u16 = 43_001;
const KIND_JOB_RESULT: u16 = 43_004;

#[derive(Debug, Parser)]
#[command(name = "st8-compute")]
#[command(about = "Measured MeshLLM execution and signed ST8 Compute receipts")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Starts a closed local MeshLLM serving node with the persistent machine identity.
    Serve {
        #[arg(long)]
        model: String,
        #[arg(long, default_value_t = 9337)]
        api_port: u16,
        #[arg(long, default_value_t = 3131)]
        console_port: u16,
        #[arg(long)]
        mesh_name: Option<String>,
    },
    /// Publishes immutable project/node pricing signed by the provider identity.
    PublishPricing {
        #[arg(long)]
        project_event: PathBuf,
        #[arg(long)]
        relay_url: String,
        #[arg(long, default_value = "mesh-llm-v0.75.1-pricing-v1")]
        version: String,
        #[arg(long, default_value_t = 1)]
        fixed_sats: u64,
        #[arg(long, default_value_t = 1_000)]
        input_units: u64,
        #[arg(long, default_value_t = 2)]
        input_sats: u64,
        #[arg(long, default_value_t = 1_000)]
        output_units: u64,
        #[arg(long, default_value_t = 5)]
        output_sats: u64,
        #[arg(long, default_value_t = 1_000)]
        wall_time_units: u64,
        #[arg(long, default_value_t = 1)]
        wall_time_sats: u64,
        #[arg(long, default_value_t = false)]
        failed_jobs_billable: bool,
        #[arg(long)]
        output: PathBuf,
    },
    /// Publishes current Mesh node hardware/model availability signed by both identities.
    PublishCapability {
        #[arg(long)]
        pricing_event: PathBuf,
        #[arg(long)]
        relay_url: String,
        #[arg(long, default_value = "http://127.0.0.1:3131")]
        mesh_console_url: String,
        #[arg(long, default_value_t = 900)]
        available_for_secs: i64,
        #[arg(long)]
        output: PathBuf,
    },
    /// Runs real private input through local MeshLLM and publishes the signed receipt chain.
    Run {
        #[arg(long)]
        project_event: PathBuf,
        #[arg(long)]
        pricing_event: PathBuf,
        #[arg(long)]
        prompt_file: PathBuf,
        #[arg(long)]
        model: String,
        #[arg(long)]
        relay_url: String,
        #[arg(long, default_value = "http://127.0.0.1:9337/v1")]
        mesh_api_url: String,
        #[arg(long, default_value = "http://127.0.0.1:3131")]
        mesh_console_url: String,
        #[arg(long, default_value_t = 100)]
        max_cost_sats: u64,
        #[arg(long, default_value_t = 600)]
        expires_in_secs: i64,
        #[arg(long, default_value_t = 512)]
        max_output_tokens: u32,
        /// Local private answer file; never included in public receipts.
        #[arg(long)]
        private_output: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Retries saved publications without rerunning private inference.
    Resume {
        #[arg(long)]
        state: PathBuf,
    },
    /// Independently verifies retained signed job/receipt material without runtime access.
    VerifyReceipt {
        #[arg(long)]
        receipt: PathBuf,
    },
    /// Publishes a signed dispute that freezes one exact receipt before settlement.
    Dispute {
        #[arg(long)]
        project_event: PathBuf,
        #[arg(long)]
        receipt: PathBuf,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        relay_url: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Publishes the project owner's final uphold-or-void decision on an exact dispute.
    Resolve {
        #[arg(long)]
        project_event: PathBuf,
        #[arg(long)]
        receipt: PathBuf,
        #[arg(long)]
        dispute_event: PathBuf,
        #[arg(long, value_parser = ["uphold_charge", "void_charge"])]
        outcome: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        relay_url: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Aggregates receipts and publishes a project-owner signed settlement.
    Settle {
        #[arg(long)]
        project_event: PathBuf,
        #[arg(long, required = true)]
        receipts: Vec<PathBuf>,
        #[arg(long)]
        disputed_receipt_id: Vec<String>,
        #[arg(long)]
        period_start: i64,
        #[arg(long)]
        period_end: i64,
        #[arg(long)]
        relay_url: String,
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ComputeRunArtifact {
    version: u16,
    verified: VerifiedComputeReceipt,
    output_commitment: String,
    private_output_persisted: bool,
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(8 * 1024 * 1024)
        .enable_all()
        .build();
    let result = runtime
        .map_err(anyhow::Error::from)
        .and_then(|runtime| runtime.block_on(run()));
    if let Err(error) = result {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Serve {
            model,
            api_port,
            console_port,
            mesh_name,
        } => {
            serve_mesh(model, api_port, console_port, mesh_name).await?;
        }
        Command::PublishPricing {
            project_event,
            relay_url,
            version,
            fixed_sats,
            input_units,
            input_sats,
            output_units,
            output_sats,
            wall_time_units,
            wall_time_sats,
            failed_jobs_billable,
            output,
        } => {
            let project = BuzzProjectContext::from_event(&read_event(&project_event)?)?;
            let provider = provider_keys()?;
            let owner = mesh_owner()?;
            let policy = PricingPolicy {
                project: project.project,
                provider: nostr_identity(&provider),
                node_owner_id: owner.owner_id.clone(),
                version,
                fixed_sats,
                rates: vec![
                    PriceRate {
                        meter: MeterKind::InputTokens,
                        units_per_rate: input_units,
                        rate_sats: input_sats,
                    },
                    PriceRate {
                        meter: MeterKind::OutputTokens,
                        units_per_rate: output_units,
                        rate_sats: output_sats,
                    },
                    PriceRate {
                        meter: MeterKind::WallTimeMs,
                        units_per_rate: wall_time_units,
                        rate_sats: wall_time_sats,
                    },
                ],
                failed_jobs_billable,
            };
            let event = PricingContext::event_builder(&policy)?.sign_with_keys(&provider)?;
            write_json(&output, &event)?;
            publish(&relay_url, event.clone(), &provider).await?;
            println!(
                "pricing_policy_id={} event_id={} node_owner_id={}",
                hex::encode(policy.id()?),
                event.id,
                policy.node_owner_id
            );
        }
        Command::PublishCapability {
            pricing_event,
            relay_url,
            mesh_console_url,
            available_for_secs,
            output,
        } => {
            if available_for_secs <= 0 {
                return Err(anyhow!("capability availability must be positive"));
            }
            let provider = provider_keys()?;
            let pricing_event = read_event(&pricing_event)?;
            let policy: PricingPolicy = serde_json::from_str(&pricing_event.content)?;
            if policy.provider != nostr_identity(&provider) {
                return Err(anyhow!("provider key does not own pricing policy"));
            }
            let owner = mesh_owner()?;
            if owner.owner_id != policy.node_owner_id {
                return Err(anyhow!("pricing policy targets a different Mesh owner"));
            }
            let status = fetch_mesh_json(&mesh_console_url, "api/status").await?;
            let models = hosted_model_names(&status)?;
            for model in &models {
                validate_private_local_provider(&status, model)?;
            }
            let survey = mesh_llm_system::hardware::survey();
            let accelerators = survey
                .gpus
                .iter()
                .map(|gpu| AcceleratorCapability {
                    name: gpu.display_name.clone(),
                    memory_bytes: (gpu.vram_bytes > 0).then_some(gpu.vram_bytes),
                })
                .collect();
            let now = chrono::Utc::now().timestamp();
            let capability = st8_compute_protocol::NodeCapability {
                node_owner_id: owner.owner_id.clone(),
                node_public_key: owner.public_key,
                provider: nostr_identity(&provider),
                runtime_version: "mesh-llm-v0.75.1/st8-meter-v1".into(),
                cpu_cores: u32::try_from(std::thread::available_parallelism()?.get())?,
                ram_bytes: system_ram_bytes()?,
                accelerators,
                models: models
                    .into_iter()
                    .map(|model_id| ModelCapability {
                        model_id,
                        context_tokens: None,
                    })
                    .collect(),
                workloads: vec![WorkloadKind::LlmInference],
                ingress_bits_per_second: None,
                egress_bits_per_second: None,
                available_until: now
                    .checked_add(available_for_secs)
                    .context("capability availability overflow")?,
                pricing_policy_id: policy.id()?,
                reputation_refs: Vec::new(),
            };
            let mut attestation = NodeCapabilityAttestation {
                capability,
                node_signature: Vec::new(),
            };
            attestation.node_signature = owner.sign(&attestation.signing_bytes()?)?;
            let event = attestation.event_builder()?.sign_with_keys(&provider)?;
            write_json(&output, &event)?;
            publish(&relay_url, event.clone(), &provider).await?;
            println!(
                "capability_id={} event_id={} node_owner_id={}",
                hex::encode(attestation.capability.id()?),
                event.id,
                attestation.capability.node_owner_id
            );
        }
        Command::Run {
            project_event,
            pricing_event,
            prompt_file,
            model,
            relay_url,
            mesh_api_url,
            mesh_console_url,
            max_cost_sats,
            expires_in_secs,
            max_output_tokens,
            private_output,
            output,
        } => {
            let artifact = execute(ExecuteArgs {
                project_event: read_event(&project_event)?,
                pricing_event: read_event(&pricing_event)?,
                prompt: std::fs::read_to_string(prompt_file)?,
                model,
                relay_url,
                mesh_api_url,
                mesh_console_url,
                max_cost_sats,
                expires_in_secs,
                max_output_tokens,
                private_output,
                output,
            })
            .await?;
            println!(
                "job_id={} receipt_id={} cost_sats={} node_owner_id={} routing_target={}",
                hex::encode(artifact.verified.job.job.id()?),
                hex::encode(artifact.verified.attestation.receipt.id()?),
                artifact.verified.attestation.receipt.price.total_sats,
                artifact.verified.attestation.receipt.node_owner_id,
                artifact.verified.attestation.receipt.routing_target
            );
        }
        Command::Resume { state } => execution::resume(&state).await?,
        Command::VerifyReceipt { receipt } => {
            let artifact: ComputeRunArtifact = serde_json::from_slice(&std::fs::read(receipt)?)?;
            artifact.verified.verify()?;
            if artifact.output_commitment
                != hex::encode(artifact.verified.attestation.receipt.result_commitment)
            {
                return Err(anyhow!("compute artifact privacy/commitment mismatch"));
            }
            println!(
                "verified job_id={} receipt_id={} cost_sats={}",
                hex::encode(artifact.verified.job.job.id()?),
                hex::encode(artifact.verified.attestation.receipt.id()?),
                artifact.verified.attestation.receipt.price.total_sats
            );
        }
        Command::Dispute {
            project_event,
            receipt,
            reason,
            relay_url,
            output,
        } => {
            let project = BuzzProjectContext::from_event(&read_event(&project_event)?)?;
            let artifact: ComputeRunArtifact = serde_json::from_slice(&std::fs::read(receipt)?)?;
            artifact.verified.verify()?;
            let signer = dispute_keys()?;
            let body = ComputeDispute {
                project: project.project.clone(),
                receipt_id: artifact.verified.attestation.receipt.id()?,
                job_id: artifact.verified.job.job.id()?,
                disputed_at: chrono::Utc::now().timestamp(),
                reason,
                evidence: Vec::new(),
            };
            let event = body.event_builder()?.sign_with_keys(&signer)?;
            let _ = ComputeDispute::from_event(&project, &artifact.verified, &event)?;
            write_json(&output, &event)?;
            publish(&relay_url, event.clone(), &signer).await?;
            println!(
                "dispute_event_id={} receipt_id={}",
                event.id,
                hex::encode(body.receipt_id)
            );
        }
        Command::Resolve {
            project_event,
            receipt,
            dispute_event,
            outcome,
            reason,
            relay_url,
            output,
        } => {
            let project = BuzzProjectContext::from_event(&read_event(&project_event)?)?;
            let artifact: ComputeRunArtifact = serde_json::from_slice(&std::fs::read(receipt)?)?;
            let dispute = read_event(&dispute_event)?;
            let owner = project_owner_keys()?;
            let resolution = ComputeResolution {
                project: project.project.clone(),
                receipt_id: artifact.verified.attestation.receipt.id()?,
                dispute_event_id: *dispute.id.as_bytes(),
                outcome: serde_json::from_value(Value::String(outcome))?,
                resolved_at: chrono::Utc::now().timestamp(),
                reason,
            };
            let event = resolution.event_builder()?.sign_with_keys(&owner)?;
            ComputeResolution::from_event(&project, &artifact.verified, &dispute, &event)?;
            write_json(&output, &event)?;
            publish(&relay_url, event.clone(), &owner).await?;
            println!(
                "resolution_event_id={} receipt_id={}",
                event.id,
                hex::encode(resolution.receipt_id)
            );
        }
        Command::Settle {
            project_event,
            receipts,
            disputed_receipt_id,
            period_start,
            period_end,
            relay_url,
            output,
        } => {
            let project = BuzzProjectContext::from_event(&read_event(&project_event)?)?;
            let owner = project_owner_keys()?;
            if owner.public_key() != project.project_event.pubkey {
                return Err(anyhow!("settlement key is not the signed project owner"));
            }
            let verified = receipts
                .iter()
                .map(|path| -> anyhow::Result<VerifiedComputeReceipt> {
                    let artifact: ComputeRunArtifact =
                        serde_json::from_slice(&std::fs::read(path)?)?;
                    artifact.verified.verify()?;
                    Ok(artifact.verified)
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let disputed = disputed_receipt_id
                .iter()
                .map(|value| parse_digest(value))
                .collect::<anyhow::Result<Vec<_>>>()?;
            let bodies = verified
                .iter()
                .map(|receipt| receipt.attestation.receipt.clone())
                .collect::<Vec<_>>();
            let settlement = ComputeSettlement::from_receipts(
                project.project.clone(),
                period_start,
                period_end,
                &bodies,
                &disputed,
            )?;
            let event = SettlementContext::event_builder(&settlement)?.sign_with_keys(&owner)?;
            let _ = SettlementContext::from_event(&project, &verified, &disputed, &event)?;
            write_json(&output, &event)?;
            publish(&relay_url, event.clone(), &owner).await?;
            println!(
                "settlement_id={} receipt_merkle_root={} total_sats={} event_id={}",
                hex::encode(settlement.id()?),
                hex::encode(settlement.receipt_merkle_root),
                settlement.total_sats,
                event.id
            );
        }
    }
    Ok(())
}

struct MeshOwner {
    keypair: mesh_llm_host_runtime::crypto::OwnerKeypair,
    owner_id: String,
    public_key: [u8; 32],
}

impl MeshOwner {
    fn sign(&self, bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        Ok(self.keypair.sign_bytes(bytes).to_vec())
    }
}

fn mesh_owner() -> anyhow::Result<MeshOwner> {
    let path = default_keystore_path()
        .map_err(|error| anyhow!("cannot resolve Mesh owner keystore: {error}"))?;
    if !keystore_exists(&path) {
        let keypair = OwnerKeypair::generate();
        save_keystore(&path, &keypair, None, false).map_err(|error| {
            anyhow!(
                "cannot persist Mesh owner keystore at {}: {error}",
                path.display()
            )
        })?;
    }
    let keypair = load_keystore(&path, None)
        .map_err(|error| anyhow!("cannot load persistent Mesh owner keystore: {error}"))?;
    let public_key = *keypair.verifying_key().as_bytes();
    Ok(MeshOwner {
        owner_id: keypair.owner_id(),
        keypair,
        public_key,
    })
}

async fn serve_mesh(
    model: String,
    api_port: u16,
    console_port: u16,
    mesh_name: Option<String>,
) -> anyhow::Result<()> {
    if model.trim().is_empty() {
        return Err(anyhow!("model is required"));
    }
    mesh_llm_host_runtime::initialize_host_runtime()
        .await
        .map_err(|error| anyhow!("Mesh native runtime failed to initialize: {error:#}"))?;
    mesh_llm_host_runtime::models::download_model_ref_with_progress_details(&model, true)
        .await
        .map_err(|error| anyhow!("downloading Mesh model {model} failed: {error:#}"))?;
    let path = default_keystore_path()
        .map_err(|error| anyhow!("cannot resolve Mesh owner keystore: {error}"))?;
    let owner = mesh_owner()?;
    let mut builder = serve::EmbeddedServeConfig::builder()
        .model(model.clone())
        .api_port(api_port)
        .console_port(console_port)
        .publish(false)
        .auto_join(false)
        .discovery_mode(MeshDiscoveryMode::Nostr)
        .startup_timeout(Duration::from_secs(180))
        .console_ui(true)
        .owner_key(path)
        .disable_iroh_relays(false);
    if let Some(name) = mesh_name.filter(|name| !name.trim().is_empty()) {
        builder = builder.mesh_name(name);
    }
    let handle = serve::start(builder.build()).await?;
    println!(
        "mesh_ready model={model} owner_id={} api_url={} console_url={}",
        owner.owner_id,
        handle.api_base_url(),
        handle.console_url()
    );
    tokio::signal::ctrl_c().await?;
    handle.stop().await?;
    Ok(())
}

async fn fetch_models(console_url: &str) -> anyhow::Result<Value> {
    fetch_mesh_json(console_url, "api/models").await
}

async fn fetch_mesh_json(console_url: &str, resource: &str) -> anyhow::Result<Value> {
    let response = reqwest::Client::new()
        .get(format!(
            "{}/{}",
            console_url.trim_end_matches('/'),
            resource.trim_start_matches('/')
        ))
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(anyhow!("Mesh /{resource} returned {}", response.status()));
    }
    Ok(response.json().await?)
}

fn hosted_model_names(payload: &Value) -> anyhow::Result<Vec<String>> {
    let mut models = payload
        .get("hosted_models")
        .and_then(Value::as_array)
        .context("Mesh status payload is missing hosted_models")?
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    if models.is_empty() {
        return Err(anyhow!("Mesh node reports no hosted models"));
    }
    Ok(models)
}

async fn publish(relay_url: &str, event: Event, keys: &Keys) -> anyhow::Result<()> {
    let ws_url = relay_websocket_url(relay_url)?;
    let response = buzz_ws_client::publish_event(&ws_url, event, keys, None, 75).await?;
    if !response.accepted {
        return Err(anyhow!("relay rejected event: {}", response.message));
    }
    Ok(())
}

fn relay_websocket_url(relay_url: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse(relay_url)?;
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        "wss" | "ws" => return Ok(url.to_string()),
        _ => return Err(anyhow!("relay URL must use http(s) or ws(s)")),
    };
    url.set_scheme(scheme)
        .map_err(|_| anyhow!("invalid relay URL scheme"))?;
    Ok(url.to_string())
}

fn requester_keys() -> anyhow::Result<Keys> {
    keys_from_env("ST8_REQUESTER_PRIVATE_KEY").or_else(|_| keys_from_env("BUZZ_PRIVATE_KEY"))
}

fn provider_keys() -> anyhow::Result<Keys> {
    keys_from_env("ST8_PROVIDER_PRIVATE_KEY").or_else(|_| keys_from_env("BUZZ_PRIVATE_KEY"))
}

fn project_owner_keys() -> anyhow::Result<Keys> {
    keys_from_env("ST8_PROJECT_OWNER_PRIVATE_KEY").or_else(|_| keys_from_env("BUZZ_PRIVATE_KEY"))
}

fn dispute_keys() -> anyhow::Result<Keys> {
    keys_from_env("ST8_DISPUTE_PRIVATE_KEY")
        .or_else(|_| keys_from_env("ST8_REQUESTER_PRIVATE_KEY"))
        .or_else(|_| keys_from_env("ST8_PROJECT_OWNER_PRIVATE_KEY"))
        .or_else(|_| keys_from_env("BUZZ_PRIVATE_KEY"))
}

fn keys_from_env(name: &str) -> anyhow::Result<Keys> {
    let secret = std::env::var(name).with_context(|| format!("{name} is required"))?;
    Keys::parse(secret.trim()).map_err(|error| anyhow!("invalid {name}: {error}"))
}

fn nostr_identity(keys: &Keys) -> String {
    format!("nostr:{}", keys.public_key().to_hex())
}

fn parse_digest(value: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = hex::decode(value)?;
    bytes
        .try_into()
        .map_err(|_| anyhow!("digest must be exactly 32 bytes"))
}

fn read_event(path: &Path) -> anyhow::Result<Event> {
    let event: Event = serde_json::from_slice(&std::fs::read(path)?)?;
    buzz_core::verify_event(&event)?;
    Ok(event)
}

fn write_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    let parent = path.parent().context("output path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("invalid output filename")?;
    let temporary = parent.join(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        chrono::Utc::now()
            .timestamp_nanos_opt()
            .context("timestamp overflow")?
    ));
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| -> anyhow::Result<()> {
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

fn tag<const N: usize>(values: [&str; N]) -> anyhow::Result<Tag> {
    Tag::parse(values).map_err(|error| anyhow!("invalid event tag: {error}"))
}

fn system_ram_bytes() -> anyhow::Result<u64> {
    #[cfg(target_os = "linux")]
    {
        let content = std::fs::read_to_string("/proc/meminfo")?;
        let kb = content
            .lines()
            .find(|line| line.starts_with("MemTotal:"))
            .and_then(|line| line.split_whitespace().nth(1))
            .context("/proc/meminfo is missing MemTotal")?
            .parse::<u64>()?;
        return kb.checked_mul(1_024).context("RAM byte count overflow");
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()?;
        if !output.status.success() {
            return Err(anyhow!("sysctl hw.memsize failed"));
        }
        return String::from_utf8(output.stdout)?
            .trim()
            .parse()
            .map_err(Into::into);
    }
    #[cfg(target_os = "windows")]
    {
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
            ])
            .output()?;
        if !output.status.success() {
            return Err(anyhow!("Windows physical-memory query failed"));
        }
        return String::from_utf8(output.stdout)?
            .trim()
            .parse()
            .map_err(Into::into);
    }
    #[allow(unreachable_code)]
    Err(anyhow!("RAM survey is unsupported on this platform"))
}
