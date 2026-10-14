//! Sequential workflow executor.
//!
//! Responsibilities:
//! - Template variable resolution (`{{trigger.X}}`, `{{steps.ID.output.X}}`)
//! - Condition evaluation (`if:` expressions via `evalexpr`)
//! - Sequential step dispatch
//! - Execution trace updates in DB
//!
//! Action dispatch uses placeholder implementations that log intent.
//! Real event emission is wired in WF-07/08 (relay integration).

use std::collections::HashMap;

use buzz_core::tenant::CommunityId;
use evalexpr::HashMapContext;
use nostr::ToBech32;
use serde_json::Value as JsonValue;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::error::WorkflowError;
use crate::schema::{ActionDef, Step, WorkflowDef};
use crate::WorkflowEngine;

/// Data extracted from the triggering event, passed to every step.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TriggerContext {
    /// Message content (message_posted trigger).
    pub text: String,
    /// Pubkey of the event author (hex string).
    pub author: String,
    /// Channel UUID as string.
    pub channel_id: String,
    /// Unix timestamp of the triggering event (as string for template use).
    pub timestamp: String,
    /// Emoji name (reaction_added trigger).
    pub emoji: String,
    /// Event ID of the triggering message (hex string).
    pub message_id: String,
    /// True when the triggering event is itself a threaded reply (carries a
    /// NIP-10 `reply`/`root` marker e-tag). Lets a `message_posted` filter
    /// select only top-level messages via `trigger_is_reply == false`.
    pub is_reply: bool,
    /// Arbitrary webhook body fields (webhook trigger).
    pub webhook_fields: HashMap<String, String>,
}

impl TriggerContext {
    /// Look up a trigger field by name.
    ///
    /// Returns `Some(&str)` for known fields; for webhook triggers, also
    /// checks `webhook_fields`. Returns `None` for unknown names.
    pub fn get_field(&self, name: &str) -> Option<&str> {
        match name {
            "text" => Some(&self.text),
            "author" => Some(&self.author),
            "channel_id" => Some(&self.channel_id),
            "timestamp" => Some(&self.timestamp),
            "emoji" => Some(&self.emoji),
            "message_id" => Some(&self.message_id),
            other => self.webhook_fields.get(other).map(|s| s.as_str()),
        }
    }
}

/// Resolve `{{trigger.X}}` and `{{steps.ID.output.X}}` placeholders in a string.
///
/// Supports filters:
/// - `| truncate(N)` — truncate to N characters
/// - `| npub` — encode a hex pubkey as its full bech32 `npub` (non-pubkey
///   values pass through unchanged); `truncate_pubkey` is a legacy alias
///
/// Unknown `{{keys}}` are left as literal text (no error, no substitution).
pub fn resolve_template(
    template: &str,
    trigger_ctx: &TriggerContext,
    step_outputs: &HashMap<String, JsonValue>,
) -> Result<String, WorkflowError> {
    if !template.contains("{{") {
        return Ok(template.to_owned());
    }

    let mut result = String::with_capacity(template.len());
    let mut remaining = template;

    while let Some(start) = remaining.find("{{") {
        result.push_str(&remaining[..start]);
        remaining = &remaining[start + 2..];

        let end = match remaining.find("}}") {
            Some(e) => e,
            None => {
                // Unclosed `{{` — emit literally and stop.
                result.push_str("{{");
                result.push_str(remaining);
                return Ok(result);
            }
        };

        let expr = remaining[..end].trim();
        remaining = &remaining[end + 2..];

        // Split on `|` to extract filters.
        let mut parts = expr.splitn(2, '|');
        let var_path = parts.next().unwrap_or("").trim();
        let filter = parts.next().map(|s| s.trim());

        let raw_value = resolve_variable(var_path, trigger_ctx, step_outputs);

        let value = match (raw_value, filter) {
            (Some(v), Some(f)) => apply_filter(v, f)?,
            (Some(v), None) => v,
            (None, _) => {
                // Unknown variable — emit the original `{{expr}}` literally.
                result.push_str("{{");
                result.push_str(expr);
                result.push_str("}}");
                continue;
            }
        };

        result.push_str(&value);
    }

    result.push_str(remaining);
    Ok(result)
}

/// Resolve a single variable path to its string value.
fn resolve_variable(
    path: &str,
    trigger_ctx: &TriggerContext,
    step_outputs: &HashMap<String, JsonValue>,
) -> Option<String> {
    if let Some(field) = path.strip_prefix("trigger.") {
        return trigger_ctx.get_field(field).map(|s| s.to_owned());
    }

    // Pattern: `steps.STEP_ID.output.FIELD`
    if let Some(rest) = path.strip_prefix("steps.") {
        let mut parts = rest.splitn(3, '.');
        let step_id = parts.next()?;
        let middle = parts.next()?; // must be "output"
        let field = parts.next()?;

        if middle != "output" {
            return None;
        }

        let output = step_outputs.get(step_id)?;
        return json_get_str(output, field);
    }

    None
}

/// Navigate a JSON value by a single key and return it as a string.
fn json_get_str(value: &JsonValue, key: &str) -> Option<String> {
    match value {
        JsonValue::Object(map) => {
            let v = map.get(key)?;
            Some(json_to_string(v))
        }
        _ => None,
    }
}

/// Convert a JSON value to a plain string for template substitution.
fn json_to_string(v: &JsonValue) -> String {
    match v {
        JsonValue::String(s) => s.clone(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Number(n) => n.to_string(),
        JsonValue::Null => String::new(),
        other => other.to_string(),
    }
}

/// Apply a filter expression to a resolved value.
fn apply_filter(value: String, filter: &str) -> Result<String, WorkflowError> {
    let filter = filter.trim();

    if let Some(inner) = filter
        .strip_prefix("truncate(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let n: usize = inner.trim().parse().map_err(|_| {
            WorkflowError::TemplateError(format!("truncate() requires a number, got: {inner}"))
        })?;
        let truncated: String = value.chars().take(n).collect();
        return Ok(truncated);
    }

    // `npub` (alias `truncate_pubkey`): full bech32 npub — truncated prefixes are grindable.
    if filter == "npub" || filter == "truncate_pubkey" {
        if let Ok(pk) = nostr::PublicKey::from_hex(&value) {
            return Ok(pk.to_bech32().unwrap_or(value));
        }
        return Ok(value);
    }

    Err(WorkflowError::TemplateError(format!(
        "unknown filter: {filter}"
    )))
}

/// Build an `evalexpr::HashMapContext` from trigger context and step outputs.
///
/// Variable names use underscores (not dots) because `evalexpr` does not
/// support dotted identifiers:
///
/// | YAML reference                    | evalexpr variable         |
/// |-----------------------------------|---------------------------|
/// | `trigger.text`                    | `trigger_text`            |
/// | `trigger.author`                  | `trigger_author`          |
/// | `trigger.channel_id`              | `trigger_channel_id`      |
/// | `trigger.timestamp`               | `trigger_timestamp`       |
/// | `trigger.emoji`                   | `trigger_emoji`           |
/// | `trigger.message_id`              | `trigger_message_id`      |
/// | `trigger.is_reply`                | `trigger_is_reply` (bool) |
/// | `steps.STEP_ID.output.FIELD`      | `steps_STEP_ID_output_FIELD` |
///
/// Also registers string helper functions that the `cron` crate's `evalexpr` v11
/// does not include by default:
/// - `str_contains(haystack, needle)` → bool
/// - `str_starts_with(s, prefix)` → bool
/// - `str_ends_with(s, suffix)` → bool
/// - `str_len(s)` → int
pub fn build_eval_context(
    trigger_ctx: &TriggerContext,
    step_outputs: &HashMap<String, JsonValue>,
) -> Result<HashMapContext, WorkflowError> {
    use evalexpr::*;

    let mut ctx = HashMapContext::new();

    // evalexpr v11 does not ship str_contains / str_starts_with / str_ends_with.
    // Register them as custom functions so workflow YAML can use them.

    ctx.set_function(
        "str_contains".into(),
        Function::new(|args| {
            let args = args.as_fixed_len_tuple(2)?;
            let haystack = args[0].as_string()?;
            let needle = args[1].as_string()?;
            Ok(Value::Boolean(haystack.contains(needle.as_str())))
        }),
    )
    .map_err(|e| WorkflowError::ConditionError(e.to_string()))?;

    ctx.set_function(
        "str_starts_with".into(),
        Function::new(|args| {
            let args = args.as_fixed_len_tuple(2)?;
            let s = args[0].as_string()?;
            let prefix = args[1].as_string()?;
            Ok(Value::Boolean(s.starts_with(prefix.as_str())))
        }),
    )
    .map_err(|e| WorkflowError::ConditionError(e.to_string()))?;

    ctx.set_function(
        "str_ends_with".into(),
        Function::new(|args| {
            let args = args.as_fixed_len_tuple(2)?;
            let s = args[0].as_string()?;
            let suffix = args[1].as_string()?;
            Ok(Value::Boolean(s.ends_with(suffix.as_str())))
        }),
    )
    .map_err(|e| WorkflowError::ConditionError(e.to_string()))?;

    ctx.set_function(
        "str_len".into(),
        Function::new(|arg| {
            let s = arg.as_string()?;
            Ok(Value::Int(s.len() as i64))
        }),
    )
    .map_err(|e| WorkflowError::ConditionError(e.to_string()))?;

    // Register webhook fields first as `trigger_FIELD` so that standard trigger
    // fields inserted below always take precedence and cannot be spoofed.
    for (key, val) in &trigger_ctx.webhook_fields {
        // Skip any key that would collide with a standard trigger_ or steps_ variable.
        if key.starts_with("trigger_") || key.starts_with("steps_") {
            continue;
        }
        let var_name = format!("trigger_{key}");
        ctx.set_value(var_name, Value::String(val.clone()))
            .map_err(|e| WorkflowError::ConditionError(e.to_string()))?;
    }

    let trigger_fields = [
        ("trigger_text", trigger_ctx.text.as_str()),
        ("trigger_author", trigger_ctx.author.as_str()),
        ("trigger_channel_id", trigger_ctx.channel_id.as_str()),
        ("trigger_timestamp", trigger_ctx.timestamp.as_str()),
        ("trigger_emoji", trigger_ctx.emoji.as_str()),
        ("trigger_message_id", trigger_ctx.message_id.as_str()),
    ];

    for (name, val) in &trigger_fields {
        ctx.set_value((*name).into(), Value::String((*val).to_owned()))
            .map_err(|e| WorkflowError::ConditionError(e.to_string()))?;
    }

    // `trigger_is_reply` is boolean (not a string field), so a filter can read
    // `trigger_is_reply == false` to fire only on top-level messages.
    ctx.set_value(
        "trigger_is_reply".into(),
        Value::Boolean(trigger_ctx.is_reply),
    )
    .map_err(|e| WorkflowError::ConditionError(e.to_string()))?;

    for (step_id, output) in step_outputs {
        if let JsonValue::Object(map) = output {
            for (field, val) in map {
                let var_name = format!("steps_{step_id}_output_{field}");
                let eval_val = json_value_to_eval(val);
                ctx.set_value(var_name, eval_val)
                    .map_err(|e| WorkflowError::ConditionError(e.to_string()))?;
            }
        }
    }

    Ok(ctx)
}

/// Convert a `serde_json::Value` to an `evalexpr::Value`.
fn json_value_to_eval(v: &JsonValue) -> evalexpr::Value {
    use evalexpr::Value as EV;
    match v {
        JsonValue::String(s) => EV::String(s.clone()),
        JsonValue::Bool(b) => EV::Boolean(*b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                EV::Int(i)
            } else if let Some(f) = n.as_f64() {
                EV::Float(f)
            } else {
                EV::String(n.to_string())
            }
        }
        JsonValue::Null => EV::Empty,
        other => EV::String(other.to_string()),
    }
}

/// Maximum wall-clock time allowed for a single `evalexpr` evaluation.
///
/// `evalexpr` is not designed for adversarial input — a deeply nested or
/// recursive expression can spin indefinitely. We run the evaluation on a
/// blocking thread and impose a hard timeout.
const EVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

/// Evaluate a boolean `if:` expression against the current execution context.
///
/// Returns `true` if the step should run, `false` if it should be skipped.
///
/// The evaluation is wrapped in a [`tokio::time::timeout`] to prevent a
/// malicious or pathological expression from blocking a Tokio worker thread.
pub async fn evaluate_condition(
    expr: &str,
    trigger_ctx: &TriggerContext,
    step_outputs: &HashMap<String, JsonValue>,
) -> Result<bool, WorkflowError> {
    let ctx = build_eval_context(trigger_ctx, step_outputs)?;
    let expr_owned = expr.to_owned();

    // Bound expression complexity to prevent pathological evaluation times.
    // The spawn_blocking thread cannot be cancelled by tokio::time::timeout —
    // it will run to completion even after timeout. Length-limiting the expression
    // prevents worst-case O(2^n) evaluation paths.
    const MAX_EXPR_LEN: usize = 4096;
    if expr_owned.len() > MAX_EXPR_LEN {
        return Err(WorkflowError::ConditionError(format!(
            "condition expression exceeds {} byte limit",
            MAX_EXPR_LEN
        )));
    }

    let result = tokio::time::timeout(
        EVAL_TIMEOUT,
        tokio::task::spawn_blocking(move || evalexpr::eval_boolean_with_context(&expr_owned, &ctx)),
    )
    .await
    .map_err(|_| {
        WorkflowError::ConditionError(format!(
            "'{expr}': evaluation timed out after {}ms",
            EVAL_TIMEOUT.as_millis()
        ))
    })?
    .map_err(|e| WorkflowError::ConditionError(format!("'{expr}': eval task panicked: {e}")))?
    .map_err(|e| WorkflowError::ConditionError(format!("'{expr}': {e}")))?;

    Ok(result)
}

/// Resolve all template variables in a step's action fields.
///
/// Returns a new `ActionDef` with all `{{...}}` placeholders substituted.
pub fn resolve_step_templates(
    step: &Step,
    trigger_ctx: &TriggerContext,
    step_outputs: &HashMap<String, JsonValue>,
) -> Result<ActionDef, WorkflowError> {
    use ActionDef::*;

    let t = |s: &str| resolve_template(s, trigger_ctx, step_outputs);
    let t_opt = |s: &Option<String>| -> Result<Option<String>, WorkflowError> {
        match s {
            Some(v) => Ok(Some(t(v)?)),
            None => Ok(None),
        }
    };

    match &step.action {
        SendMessage {
            text,
            channel,
            reply_in_thread,
        } => Ok(SendMessage {
            text: t(text)?,
            channel: t_opt(channel)?,
            reply_in_thread: *reply_in_thread,
        }),
        SendDm { to, text } => Ok(SendDm {
            to: t(to)?,
            text: t(text)?,
        }),
        SetChannelTopic { topic } => Ok(SetChannelTopic { topic: t(topic)? }),
        AddReaction { emoji } => Ok(AddReaction { emoji: t(emoji)? }),
        CallWebhook {
            url,
            method,
            headers,
            body,
        } => {
            let resolved_headers = match headers {
                Some(h) => {
                    let mut out = std::collections::HashMap::new();
                    for (k, v) in h {
                        out.insert(k.clone(), t(v)?);
                    }
                    Some(out)
                }
                None => None,
            };
            Ok(CallWebhook {
                url: t(url)?,
                method: method.clone(),
                headers: resolved_headers,
                body: t_opt(body)?,
            })
        }
        RequestApproval {
            from,
            message,
            timeout,
        } => Ok(RequestApproval {
            from: t(from)?,
            message: t(message)?,
            timeout: timeout.clone(),
        }),
        Delay { duration } => Ok(Delay {
            duration: duration.clone(),
        }),
    }
}

/// Result of dispatching a single step action.
#[derive(Debug)]
pub enum StepResult {
    /// Step completed normally. Output is stored in `step_outputs`.
    Completed(JsonValue),
    /// Step requests suspension (approval gate). Execution must pause.
    Suspended {
        /// Token used to resume or reject this approval gate.
        approval_token: String,
    },
    /// Step was skipped due to `if:` condition being false.
    Skipped,
}

mod dispatch;
pub use dispatch::dispatch_action;
pub(crate) use dispatch::parse_duration_secs;
#[cfg(test)]
use dispatch::*;

/// Rich return type from `execute_run` / `execute_from_step`.
///
/// Carries enough information for the caller to:
/// - Persist the approval record when suspended at a `RequestApproval` step.
/// - Update the run's execution trace and current step in the DB.
/// - Resume execution from the correct step after approval.
#[derive(Debug)]
pub struct ExecutionResult {
    /// Set when execution suspended at a `RequestApproval` step.
    /// `None` means the run completed normally.
    pub approval_token: Option<String>,
    /// Index of the step that suspended (or the total step count on completion).
    pub step_index: usize,
    /// Accumulated step outputs at the point of suspension or completion.
    pub step_outputs: HashMap<String, JsonValue>,
    /// Execution trace: one entry per completed/skipped step.
    pub trace: Vec<JsonValue>,
}

/// Execute a workflow run sequentially.
///
/// Steps run in order. Each step:
/// 1. Evaluates `if:` condition (skip if false).
/// 2. Resolves template variables in action fields.
/// 3. Dispatches the action.
/// 4. Stores the step output for use by later steps.
///
/// On `RequestApproval`: returns `ExecutionResult` with `approval_token = Some(token)`.
/// Caller must persist the approval record and update the run status.
///
/// Returns `ExecutionResult` with `approval_token = None` on normal completion.
///
/// Enforces `engine.config.max_concurrent` via a semaphore — returns
/// [`WorkflowError::CapacityExceeded`] immediately if all permits are taken.
/// Transitions the run to `Running` after acquiring a permit.
pub async fn execute_run(
    engine: &WorkflowEngine,
    community_id: CommunityId,
    run_id: Uuid,
    def: &WorkflowDef,
    trigger_ctx: &TriggerContext,
) -> Result<ExecutionResult, (WorkflowError, crate::error::PartialProgress)> {
    // Fail fast if all concurrency permits are in use — no queuing.
    let _permit = engine.run_semaphore.try_acquire().map_err(|_| {
        (
            WorkflowError::CapacityExceeded,
            crate::error::PartialProgress::default(),
        )
    })?;

    engine
        .db
        .update_workflow_run(
            community_id,
            run_id,
            buzz_db::workflow::RunStatus::Running,
            0,
            &serde_json::json!([]),
            None,
        )
        .await
        .map_err(|e| {
            (
                WorkflowError::from(e),
                crate::error::PartialProgress::default(),
            )
        })?;

    execute_steps(engine, community_id, run_id, def, trigger_ctx, 0, None).await
}

/// Resume execution from a specific step index (used for approval resume).
///
/// Acquires a concurrency permit from `engine.run_semaphore` before executing —
/// returns [`WorkflowError::CapacityExceeded`] immediately if all permits are
/// taken.
///
/// Transitions the run to `Running` after acquiring a permit, so that
/// approval-resumed runs correctly reflect their active state.
///
/// `initial_outputs` should be reconstructed from the execution trace before
/// calling this function on resume, so that steps after the resume point can
/// reference `{{steps.PREV_STEP.output.X}}` correctly.
pub async fn execute_from_step(
    engine: &WorkflowEngine,
    community_id: CommunityId,
    run_id: Uuid,
    def: &WorkflowDef,
    trigger_ctx: &TriggerContext,
    start_index: usize,
    initial_outputs: Option<HashMap<String, JsonValue>>,
) -> Result<ExecutionResult, (WorkflowError, crate::error::PartialProgress)> {
    // Fail fast if all concurrency permits are in use — no queuing.
    let _permit = engine.run_semaphore.try_acquire().map_err(|_| {
        (
            WorkflowError::CapacityExceeded,
            crate::error::PartialProgress::default(),
        )
    })?;

    // Mark run as Running now that we have a permit (resume from approval).
    // Preserve the existing execution trace from pre-approval steps.
    let existing_trace = engine
        .db
        .get_workflow_run(community_id, run_id)
        .await
        .map_err(|error| {
            (
                WorkflowError::from(error),
                crate::error::PartialProgress::default(),
            )
        })?
        .execution_trace;
    engine
        .db
        .update_workflow_run(
            community_id,
            run_id,
            buzz_db::workflow::RunStatus::Running,
            start_index as i32,
            &existing_trace,
            None,
        )
        .await
        .map_err(|e| {
            (
                WorkflowError::from(e),
                crate::error::PartialProgress::default(),
            )
        })?;

    execute_steps(
        engine,
        community_id,
        run_id,
        def,
        trigger_ctx,
        start_index,
        initial_outputs,
    )
    .await
}

/// Internal: execute workflow steps starting from `start_index`, without
/// acquiring the semaphore. Called by both [`execute_run`] and
/// [`execute_from_step`] after they have already acquired a permit.
///
/// On error, returns `(WorkflowError, PartialProgress)` so callers can persist
/// the trace of steps completed before the failure.
async fn execute_steps(
    engine: &WorkflowEngine,
    community_id: CommunityId,
    run_id: Uuid,
    def: &WorkflowDef,
    trigger_ctx: &TriggerContext,
    start_index: usize,
    initial_outputs: Option<HashMap<String, JsonValue>>,
) -> Result<ExecutionResult, (WorkflowError, crate::error::PartialProgress)> {
    let mut step_outputs: HashMap<String, JsonValue> = initial_outputs.unwrap_or_default();
    let mut trace: Vec<JsonValue> = Vec::new();

    for (i, step) in def.steps.iter().enumerate() {
        if i < start_index {
            debug!(run_id = %run_id, step = %step.id, "Skipping already-executed step");
            continue;
        }

        if let Some(expr) = &step.if_expr {
            match evaluate_condition(expr, trigger_ctx, &step_outputs).await {
                Ok(true) => {
                    debug!(run_id = %run_id, step = %step.id, "Condition true — running step");
                }
                Ok(false) => {
                    info!(run_id = %run_id, step = %step.id, "Condition false — skipping step");
                    trace.push(serde_json::json!({
                        "step_id": step.id,
                        "status": "skipped",
                    }));
                    continue;
                }
                Err(e) => {
                    warn!(run_id = %run_id, step = %step.id, "Condition error: {e}");
                    let progress = crate::error::PartialProgress {
                        step_index: i,
                        trace,
                    };
                    return Err((e, progress));
                }
            }
        }

        let resolved_action = match resolve_step_templates(step, trigger_ctx, &step_outputs) {
            Ok(a) => a,
            Err(e) => {
                let progress = crate::error::PartialProgress {
                    step_index: i,
                    trace,
                };
                return Err((e, progress));
            }
        };

        let timeout_secs = step
            .timeout_secs
            .unwrap_or(engine.config.default_timeout_secs);
        let dispatch_result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            dispatch_action(
                &step.id,
                &resolved_action,
                engine,
                community_id,
                run_id,
                trigger_ctx,
            ),
        )
        .await;

        let result = match dispatch_result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                let progress = crate::error::PartialProgress {
                    step_index: i,
                    trace,
                };
                return Err((e, progress));
            }
            Err(_timeout) => {
                let progress = crate::error::PartialProgress {
                    step_index: i,
                    trace,
                };
                return Err((
                    WorkflowError::StepTimeout {
                        step_id: step.id.clone(),
                        timeout_secs,
                    },
                    progress,
                ));
            }
        };

        match result {
            StepResult::Completed(output) => {
                debug!(run_id = %run_id, step = %step.id, "Step completed");
                trace.push(serde_json::json!({
                    "step_id": step.id,
                    "status": "completed",
                    "output": output,
                }));
                step_outputs.insert(step.id.clone(), output);
            }
            StepResult::Suspended { approval_token } => {
                info!(
                    run_id = %run_id, step = %step.id,
                    "Step suspended — awaiting approval (token: <redacted>)"
                );
                trace.push(serde_json::json!({
                    "step_id": step.id,
                    "status": "waiting_approval",
                    "approval_action": resolved_action,
                    "workflow_snapshot": def,
                }));
                return Ok(ExecutionResult {
                    approval_token: Some(approval_token),
                    step_index: i,
                    step_outputs,
                    trace,
                });
            }
            StepResult::Skipped => {
                debug!(run_id = %run_id, step = %step.id, "Step skipped");
                trace.push(serde_json::json!({
                    "step_id": step.id,
                    "status": "skipped",
                }));
            }
        }
    }

    info!(run_id = %run_id, "Workflow run completed");
    Ok(ExecutionResult {
        approval_token: None,
        step_index: def.steps.len(),
        step_outputs,
        trace,
    })
}

#[cfg(test)]
mod tests;
