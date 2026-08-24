#![deny(unsafe_code)]
#![warn(missing_docs)]
//! Fail-closed extraction of per-job measurements from MeshLLM runtime signals.

use serde::{Deserialize, Serialize};
use st8_compute_protocol::{MeasurementQuality, MeterKind, MeteredQuantity};
use thiserror::Error;

/// One local-only per-model routing counter exported by MeshLLM `/api/models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshTargetCounter {
    /// Exact runtime model name.
    pub model: String,
    /// Stable routing target label.
    pub target: String,
    /// `local`, `remote`, or `endpoint`.
    pub kind: String,
    /// Cumulative attempts observed by this node.
    pub attempt_count: u64,
    /// Cumulative successful attempts.
    pub success_count: u64,
    /// Cumulative completion tokens observed at this target.
    pub completion_tokens_observed: u64,
}

/// Exact measured usage returned by one Mesh execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasuredMeshExecution {
    /// Serving target uniquely attributed by runtime counter deltas.
    pub routing_target: String,
    /// Canonical measured dimensions.
    pub usage: Vec<MeteredQuantity>,
}

/// Meter extraction failures. All ambiguous states fail closed.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MeterError {
    /// Mesh management payload did not contain the requested model counters.
    #[error("Mesh model routing counters are unavailable")]
    MissingCounters,
    /// Zero or multiple targets changed during the execution window.
    #[error("Mesh target attribution is ambiguous")]
    AmbiguousTarget,
    /// The execution was routed away from the node whose owner key signs it.
    #[error("Mesh execution was not served by the local provider node")]
    NonLocalTarget,
    /// Counters reset, overflowed, or disagreed with response usage.
    #[error("Mesh runtime counters disagree with response usage")]
    CounterMismatch,
    /// Runtime response omitted authoritative usage.
    #[error("Mesh response omitted exact token usage")]
    MissingUsage,
}

/// Requires the configured Mesh ingress to be a ready, private, local-only
/// provider for the exact requested model before private input is submitted.
pub fn validate_private_local_provider(
    payload: &serde_json::Value,
    model: &str,
) -> Result<(), MeterError> {
    let hosted = payload
        .get("hosted_models")
        .and_then(serde_json::Value::as_array)
        .ok_or(MeterError::NonLocalTarget)?;
    let capabilities = payload
        .pointer("/runtime/capabilities")
        .and_then(serde_json::Value::as_object)
        .ok_or(MeterError::NonLocalTarget)?;
    let peers_are_empty = payload
        .get("peers")
        .and_then(serde_json::Value::as_array)
        .is_some_and(Vec::is_empty);
    let exact_model_is_hosted = hosted.iter().any(|entry| entry.as_str() == Some(model));
    let private_local = payload
        .get("node_status")
        .and_then(serde_json::Value::as_str)
        == Some("Serving")
        && payload.get("is_host").and_then(serde_json::Value::as_bool) == Some(true)
        && payload
            .get("llama_ready")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        && payload
            .get("publication_state")
            .and_then(serde_json::Value::as_str)
            == Some("private")
        && capabilities
            .get("local_serving")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        && capabilities
            .get("proxying")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        && peers_are_empty;
    if !exact_model_is_hosted || !private_local {
        return Err(MeterError::NonLocalTarget);
    }
    Ok(())
}

/// Extracts per-target counters for one exact model from `/api/models` JSON.
pub fn target_counters(
    payload: &serde_json::Value,
    model: &str,
) -> Result<Vec<MeshTargetCounter>, MeterError> {
    let models = payload
        .get("mesh_models")
        .and_then(serde_json::Value::as_array)
        .ok_or(MeterError::MissingCounters)?;
    let model_value = models
        .iter()
        .find(|entry| entry.get("name").and_then(serde_json::Value::as_str) == Some(model))
        .ok_or(MeterError::MissingCounters)?;
    let routing_metrics = model_value.get("routing_metrics");
    if routing_metrics.is_none_or(serde_json::Value::is_null) {
        return Ok(Vec::new());
    }
    let routing_metrics = routing_metrics.ok_or(MeterError::MissingCounters)?;
    let targets = routing_metrics
        .get("targets")
        .and_then(serde_json::Value::as_array)
        .ok_or(MeterError::MissingCounters)?;
    targets
        .iter()
        .map(|target| {
            Ok(MeshTargetCounter {
                model: model.to_owned(),
                target: required_text(target, "target")?,
                kind: required_text(target, "kind")?,
                attempt_count: required_u64(target, "attempt_count")?,
                success_count: required_u64(target, "success_count")?,
                completion_tokens_observed: required_u64(target, "completion_tokens_observed")?,
            })
        })
        .collect()
}

/// Attributes one completed response by exact pre/post node counters.
pub fn derive_completed_execution(
    before: &[MeshTargetCounter],
    after: &[MeshTargetCounter],
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    wall_time_ms: u64,
) -> Result<MeasuredMeshExecution, MeterError> {
    let prompt_tokens = prompt_tokens.ok_or(MeterError::MissingUsage)?;
    let completion_tokens = completion_tokens.ok_or(MeterError::MissingUsage)?;
    let mut changed = Vec::new();
    for current in after {
        let previous = before
            .iter()
            .find(|candidate| candidate.target == current.target && candidate.kind == current.kind);
        let attempts_before = previous.map_or(0, |counter| counter.attempt_count);
        let successes_before = previous.map_or(0, |counter| counter.success_count);
        let tokens_before = previous.map_or(0, |counter| counter.completion_tokens_observed);
        let attempt_delta = current
            .attempt_count
            .checked_sub(attempts_before)
            .ok_or(MeterError::CounterMismatch)?;
        let success_delta = current
            .success_count
            .checked_sub(successes_before)
            .ok_or(MeterError::CounterMismatch)?;
        let token_delta = current
            .completion_tokens_observed
            .checked_sub(tokens_before)
            .ok_or(MeterError::CounterMismatch)?;
        if attempt_delta != 0 || success_delta != 0 || token_delta != 0 {
            changed.push((current, attempt_delta, success_delta, token_delta));
        }
    }
    let [(target, 1, 1, token_delta)] = changed.as_slice() else {
        return Err(MeterError::AmbiguousTarget);
    };
    if target.kind != "local" {
        return Err(MeterError::NonLocalTarget);
    }
    if *token_delta != completion_tokens {
        return Err(MeterError::CounterMismatch);
    }
    Ok(MeasuredMeshExecution {
        routing_target: format!("{}:{}", target.kind, target.target),
        usage: vec![
            MeteredQuantity {
                kind: MeterKind::InputTokens,
                quantity: prompt_tokens,
                quality: MeasurementQuality::TargetExact,
                source: "mesh.openai.usage.prompt_tokens".into(),
            },
            MeteredQuantity {
                kind: MeterKind::OutputTokens,
                quantity: completion_tokens,
                quality: MeasurementQuality::TargetExact,
                source: "mesh.openai.usage.completion_tokens".into(),
            },
            MeteredQuantity {
                kind: MeterKind::WallTimeMs,
                quantity: wall_time_ms,
                quality: MeasurementQuality::HostExact,
                source: "st8.monotonic.elapsed_ms".into(),
            },
            MeteredQuantity {
                kind: MeterKind::Jobs,
                quantity: 1,
                quality: MeasurementQuality::HostExact,
                source: "st8.completed_job".into(),
            },
        ],
    })
}

/// Attributes one failed execution when exactly one local target attempted it.
pub fn derive_failed_execution(
    before: &[MeshTargetCounter],
    after: &[MeshTargetCounter],
    wall_time_ms: u64,
) -> Result<MeasuredMeshExecution, MeterError> {
    let mut changed = Vec::new();
    for current in after {
        let previous = before
            .iter()
            .find(|candidate| candidate.target == current.target && candidate.kind == current.kind);
        let attempts = current
            .attempt_count
            .checked_sub(previous.map_or(0, |counter| counter.attempt_count))
            .ok_or(MeterError::CounterMismatch)?;
        let successes = current
            .success_count
            .checked_sub(previous.map_or(0, |counter| counter.success_count))
            .ok_or(MeterError::CounterMismatch)?;
        let tokens = current
            .completion_tokens_observed
            .checked_sub(previous.map_or(0, |counter| counter.completion_tokens_observed))
            .ok_or(MeterError::CounterMismatch)?;
        if attempts != 0 || successes != 0 || tokens != 0 {
            changed.push((current, attempts, successes, tokens));
        }
    }
    let [(target, attempts, 0, tokens)] = changed.as_slice() else {
        return Err(MeterError::AmbiguousTarget);
    };
    if *attempts == 0 || target.kind != "local" {
        return Err(if target.kind != "local" {
            MeterError::NonLocalTarget
        } else {
            MeterError::CounterMismatch
        });
    }
    let mut usage = vec![
        MeteredQuantity {
            kind: MeterKind::WallTimeMs,
            quantity: wall_time_ms,
            quality: MeasurementQuality::HostExact,
            source: "st8.monotonic.elapsed_ms".into(),
        },
        MeteredQuantity {
            kind: MeterKind::Jobs,
            quantity: 1,
            quality: MeasurementQuality::HostExact,
            source: "st8.failed_job".into(),
        },
    ];
    if *tokens > 0 {
        usage.push(MeteredQuantity {
            kind: MeterKind::OutputTokens,
            quantity: *tokens,
            quality: MeasurementQuality::AttributedDelta,
            source: "mesh.target.completion_tokens_delta".into(),
        });
    }
    Ok(MeasuredMeshExecution {
        routing_target: format!("{}:{}", target.kind, target.target),
        usage,
    })
}

fn required_text(value: &serde_json::Value, name: &str) -> Result<String, MeterError> {
    value
        .get(name)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or(MeterError::MissingCounters)
}

fn required_u64(value: &serde_json::Value, name: &str) -> Result<u64, MeterError> {
    value
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .ok_or(MeterError::MissingCounters)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counter(
        target: &str,
        kind: &str,
        attempts: u64,
        successes: u64,
        tokens: u64,
    ) -> MeshTargetCounter {
        MeshTargetCounter {
            model: "qwen".into(),
            target: target.into(),
            kind: kind.into(),
            attempt_count: attempts,
            success_count: successes,
            completion_tokens_observed: tokens,
        }
    }

    #[test]
    fn attributes_one_exact_local_delta() {
        let measured = derive_completed_execution(
            &[counter("node-a", "local", 4, 4, 100)],
            &[counter("node-a", "local", 5, 5, 109)],
            Some(12),
            Some(9),
            42,
        )
        .expect("measured");
        assert_eq!(measured.routing_target, "local:node-a");
        assert_eq!(measured.usage[1].quantity, 9);
    }

    #[test]
    fn rejects_concurrency_and_remote_substitution() {
        let before = [
            counter("node-a", "local", 1, 1, 2),
            counter("node-b", "local", 1, 1, 2),
        ];
        let after = [
            counter("node-a", "local", 2, 2, 3),
            counter("node-b", "local", 2, 2, 3),
        ];
        assert_eq!(
            derive_completed_execution(&before, &after, Some(1), Some(1), 1),
            Err(MeterError::AmbiguousTarget)
        );
        assert_eq!(
            derive_completed_execution(
                &[counter("peer", "remote", 1, 1, 2)],
                &[counter("peer", "remote", 2, 2, 3)],
                Some(1),
                Some(1),
                1
            ),
            Err(MeterError::NonLocalTarget)
        );
    }

    #[test]
    fn rejects_usage_counter_disagreement() {
        assert_eq!(
            derive_completed_execution(
                &[counter("node", "local", 1, 1, 2)],
                &[counter("node", "local", 2, 2, 4)],
                Some(1),
                Some(1),
                1
            ),
            Err(MeterError::CounterMismatch)
        );
    }

    #[test]
    fn accepts_uninitialized_model_as_zero_baseline() {
        for payload in [
            serde_json::json!({"mesh_models": [{"name": "qwen"}]}),
            serde_json::json!({
                "mesh_models": [{"name": "qwen", "routing_metrics": null}]
            }),
        ] {
            assert_eq!(target_counters(&payload, "qwen").unwrap(), Vec::new());
        }
    }

    #[test]
    fn private_input_requires_ready_local_non_proxying_provider() {
        let model = "qwen";
        let mut status = serde_json::json!({
            "node_status": "Serving",
            "is_host": true,
            "llama_ready": true,
            "publication_state": "private",
            "hosted_models": [model],
            "peers": [],
            "runtime": {"capabilities": {"local_serving": true, "proxying": false}}
        });
        validate_private_local_provider(&status, model).expect("private local provider");
        status["runtime"]["capabilities"]["proxying"] = serde_json::Value::Bool(true);
        assert_eq!(
            validate_private_local_provider(&status, model),
            Err(MeterError::NonLocalTarget)
        );
    }
}
