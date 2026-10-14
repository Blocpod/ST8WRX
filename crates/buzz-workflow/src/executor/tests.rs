use super::*;
use serde_json::json;

fn make_trigger() -> TriggerContext {
    TriggerContext {
        text: "P1 incident in production".to_owned(),
        author: "abc123def456".to_owned(),
        channel_id: "channel-uuid-here".to_owned(),
        timestamp: "1700000000".to_owned(),
        emoji: "fire".to_owned(),
        message_id: "event-id-hex".to_owned(),
        is_reply: false,
        webhook_fields: HashMap::new(),
    }
}

#[test]
fn resolve_trigger_text() {
    let ctx = make_trigger();
    let out = resolve_template("Alert: {{trigger.text}}", &ctx, &HashMap::new()).unwrap();
    assert_eq!(out, "Alert: P1 incident in production");
}

#[test]
fn resolve_trigger_author() {
    let ctx = make_trigger();
    let out = resolve_template("By {{trigger.author}}", &ctx, &HashMap::new()).unwrap();
    assert_eq!(out, "By abc123def456");
}

#[test]
fn resolve_step_output() {
    let ctx = make_trigger();
    let mut outputs = HashMap::new();
    outputs.insert("ask".to_owned(), json!({ "replied": "yes" }));
    let out = resolve_template("Reply: {{steps.ask.output.replied}}", &ctx, &outputs).unwrap();
    assert_eq!(out, "Reply: yes");
}

#[test]
fn resolve_unknown_variable_left_literal() {
    let ctx = make_trigger();
    let out = resolve_template("{{unknown.var}}", &ctx, &HashMap::new()).unwrap();
    assert_eq!(out, "{{unknown.var}}");
}

#[test]
fn resolve_truncate_filter() {
    let ctx = make_trigger();
    let out = resolve_template("{{trigger.text | truncate(5)}}", &ctx, &HashMap::new()).unwrap();
    assert_eq!(out, "P1 in");
    assert_eq!(out.chars().count(), 5);
}

#[test]
fn resolve_npub_filter_encodes_hex_pubkey() {
    let mut ctx = make_trigger();
    ctx.author = "e17e5abf7b1dbd363f0ed6fbda2455609727b2555428dea251388c542cd2f03f".to_owned();
    let out = resolve_template("{{trigger.author | npub}}", &ctx, &HashMap::new()).unwrap();
    assert_eq!(
        out,
        "npub1u9l940mmrk7nv0cw6maa5fz4vztj0vj42s5dagj38zx9gtxj7qls94fpux"
    );
}

#[test]
fn resolve_truncate_pubkey_is_alias_for_npub() {
    let mut ctx = make_trigger();
    ctx.author = "e17e5abf7b1dbd363f0ed6fbda2455609727b2555428dea251388c542cd2f03f".to_owned();
    let out = resolve_template(
        "{{trigger.author | truncate_pubkey}}",
        &ctx,
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(
        out,
        "npub1u9l940mmrk7nv0cw6maa5fz4vztj0vj42s5dagj38zx9gtxj7qls94fpux"
    );
}

#[test]
fn resolve_no_templates_fast_path() {
    let ctx = make_trigger();
    let out = resolve_template("no templates here", &ctx, &HashMap::new()).unwrap();
    assert_eq!(out, "no templates here");
}

#[test]
fn resolve_multiple_templates_in_one_string() {
    let ctx = make_trigger();
    let out = resolve_template(
        "{{trigger.author}} said: {{trigger.text}}",
        &ctx,
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(out, "abc123def456 said: P1 incident in production");
}

#[test]
fn resolve_webhook_field() {
    let mut ctx = make_trigger();
    ctx.webhook_fields
        .insert("service".to_owned(), "api-gateway".to_owned());
    let out = resolve_template("Service: {{trigger.service}}", &ctx, &HashMap::new()).unwrap();
    assert_eq!(out, "Service: api-gateway");
}

#[tokio::test]
async fn condition_true_when_text_contains_p1() {
    let ctx = make_trigger(); // text = "P1 incident in production"
    let result = evaluate_condition("str_contains(trigger_text, \"P1\")", &ctx, &HashMap::new())
        .await
        .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_false_when_text_does_not_contain_p1() {
    let mut ctx = make_trigger();
    ctx.text = "normal message".to_owned();
    let result = evaluate_condition("str_contains(trigger_text, \"P1\")", &ctx, &HashMap::new())
        .await
        .unwrap();
    assert!(!result);
}

#[tokio::test]
async fn condition_trigger_is_reply_selects_top_level_only() {
    // The top-level-only filter from the feature's use case.
    let mut ctx = make_trigger();

    ctx.is_reply = false;
    assert!(
        evaluate_condition("trigger_is_reply == false", &ctx, &HashMap::new())
            .await
            .unwrap(),
        "top-level message should pass the filter"
    );

    ctx.is_reply = true;
    assert!(
        !evaluate_condition("trigger_is_reply == false", &ctx, &HashMap::new())
            .await
            .unwrap(),
        "threaded reply should be filtered out"
    );
}

#[test]
fn resolve_step_templates_carries_reply_in_thread() {
    let ctx = make_trigger();
    let step = Step {
        id: "reply".to_owned(),
        name: None,
        if_expr: None,
        timeout_secs: None,
        action: ActionDef::SendMessage {
            text: "hi {{trigger.author}}".to_owned(),
            channel: None,
            reply_in_thread: true,
        },
    };
    let resolved = resolve_step_templates(&step, &ctx, &HashMap::new()).unwrap();
    match resolved {
        ActionDef::SendMessage {
            text,
            reply_in_thread,
            ..
        } => {
            assert_eq!(text, "hi abc123def456");
            assert!(reply_in_thread, "reply_in_thread must survive resolution");
        }
        other => panic!("unexpected action: {other:?}"),
    }
}

#[tokio::test]
async fn condition_or_expression() {
    let ctx = make_trigger(); // text contains "P1"
    let result = evaluate_condition(
        "str_contains(trigger_text, \"P1\") || str_contains(trigger_text, \"SEV1\")",
        &ctx,
        &HashMap::new(),
    )
    .await
    .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_step_output_bool() {
    let ctx = make_trigger();
    let mut outputs = HashMap::new();
    outputs.insert("request".to_owned(), json!({ "approved": true }));
    let result = evaluate_condition("steps_request_output_approved == true", &ctx, &outputs)
        .await
        .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_step_output_bool_false() {
    let ctx = make_trigger();
    let mut outputs = HashMap::new();
    outputs.insert("request".to_owned(), json!({ "approved": false }));
    let result = evaluate_condition("steps_request_output_approved == false", &ctx, &outputs)
        .await
        .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_invalid_expression_returns_error() {
    let ctx = make_trigger();
    let err = evaluate_condition("this is not valid evalexpr @@@@", &ctx, &HashMap::new())
        .await
        .unwrap_err();
    assert!(matches!(err, WorkflowError::ConditionError(_)));
}

#[tokio::test]
async fn condition_exceeding_max_expr_len_is_rejected() {
    let ctx = make_trigger();
    // Construct an expression that exceeds MAX_EXPR_LEN (4096 bytes).
    let long_expr = "true || ".repeat(625); // 8 * 625 = 5000 bytes
    let err = evaluate_condition(&long_expr, &ctx, &HashMap::new())
        .await
        .unwrap_err();
    match &err {
        WorkflowError::ConditionError(msg) => {
            assert!(
                msg.contains("exceeds") || msg.contains("limit"),
                "expected 'exceeds' or 'limit' in error message, got: {msg}"
            );
        }
        other => panic!("expected ConditionError, got: {other:?}"),
    }
}

#[test]
fn parse_duration_hours() {
    assert_eq!(parse_duration_secs("1h").unwrap(), 3600);
    assert_eq!(parse_duration_secs("2h").unwrap(), 7200);
}

#[test]
fn parse_duration_minutes() {
    assert_eq!(parse_duration_secs("5m").unwrap(), 300);
    assert_eq!(parse_duration_secs("30m").unwrap(), 1800);
}

#[test]
fn parse_duration_seconds() {
    assert_eq!(parse_duration_secs("10s").unwrap(), 10);
    assert_eq!(parse_duration_secs("60s").unwrap(), 60);
}

#[test]
fn parse_duration_plain_number() {
    assert_eq!(parse_duration_secs("42").unwrap(), 42);
}

#[test]
fn parse_duration_invalid() {
    assert!(parse_duration_secs("not-a-duration").is_err());
}

#[test]
fn resolve_unclosed_template_emits_literally() {
    // An unclosed `{{` should be emitted literally without panicking.
    let ctx = make_trigger();
    let out = resolve_template("Hello {{trigger.text", &ctx, &HashMap::new()).unwrap();
    // The unclosed `{{` and remaining text are emitted as-is.
    assert!(
        out.contains("{{"),
        "unclosed {{ should appear literally in output"
    );
}

#[test]
fn resolve_empty_template_string() {
    let ctx = make_trigger();
    let out = resolve_template("", &ctx, &HashMap::new()).unwrap();
    assert_eq!(out, "");
}

#[test]
fn resolve_template_with_only_literal_text() {
    let ctx = make_trigger();
    let out = resolve_template("no placeholders at all", &ctx, &HashMap::new()).unwrap();
    assert_eq!(out, "no placeholders at all");
}

#[test]
fn resolve_multiple_different_trigger_fields() {
    let ctx = make_trigger();
    let out = resolve_template(
        "channel={{trigger.channel_id}} ts={{trigger.timestamp}} emoji={{trigger.emoji}}",
        &ctx,
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(out, "channel=channel-uuid-here ts=1700000000 emoji=fire");
}

#[test]
fn resolve_trigger_message_id() {
    let ctx = make_trigger();
    let out = resolve_template("msg={{trigger.message_id}}", &ctx, &HashMap::new()).unwrap();
    assert_eq!(out, "msg=event-id-hex");
}

#[test]
fn resolve_step_output_boolean_value() {
    let ctx = make_trigger();
    let mut outputs = HashMap::new();
    outputs.insert("gate".to_owned(), json!({ "approved": true }));
    let out = resolve_template("Approved: {{steps.gate.output.approved}}", &ctx, &outputs).unwrap();
    assert_eq!(out, "Approved: true");
}

#[test]
fn resolve_step_output_number_value() {
    let ctx = make_trigger();
    let mut outputs = HashMap::new();
    outputs.insert("count".to_owned(), json!({ "total": 42 }));
    let out = resolve_template("Total: {{steps.count.output.total}}", &ctx, &outputs).unwrap();
    assert_eq!(out, "Total: 42");
}

#[test]
fn resolve_step_output_null_value_is_empty_string() {
    let ctx = make_trigger();
    let mut outputs = HashMap::new();
    outputs.insert("step".to_owned(), json!({ "val": null }));
    let out = resolve_template("Val: {{steps.step.output.val}}", &ctx, &outputs).unwrap();
    assert_eq!(out, "Val: ");
}

#[test]
fn resolve_unknown_step_id_left_literal() {
    let ctx = make_trigger();
    let out =
        resolve_template("{{steps.nonexistent.output.field}}", &ctx, &HashMap::new()).unwrap();
    assert_eq!(out, "{{steps.nonexistent.output.field}}");
}

#[test]
fn resolve_step_output_missing_field_left_literal() {
    let ctx = make_trigger();
    let mut outputs = HashMap::new();
    outputs.insert("step".to_owned(), json!({ "other": "value" }));
    let out = resolve_template("{{steps.step.output.missing}}", &ctx, &outputs).unwrap();
    assert_eq!(out, "{{steps.step.output.missing}}");
}

#[test]
fn resolve_truncate_zero_chars() {
    let ctx = make_trigger();
    let out = resolve_template("{{trigger.text | truncate(0)}}", &ctx, &HashMap::new()).unwrap();
    assert_eq!(out, "");
}

#[test]
fn resolve_truncate_longer_than_string() {
    let ctx = make_trigger(); // text = "P1 incident in production" (25 chars)
    let out = resolve_template("{{trigger.text | truncate(1000)}}", &ctx, &HashMap::new()).unwrap();
    // Truncating to more than the string length returns the full string.
    assert_eq!(out, "P1 incident in production");
}

#[test]
fn resolve_pubkey_filter_non_pubkey_passes_through() {
    // Values that are not valid hex pubkeys are returned unchanged.
    let mut ctx = make_trigger();
    ctx.author = "short".to_owned();
    let out = resolve_template(
        "{{trigger.author | truncate_pubkey}}",
        &ctx,
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(out, "short");
}

#[test]
fn resolve_npub_filter_passes_npub_through() {
    // Already-encoded npubs are not valid hex, so they pass through intact.
    let mut ctx = make_trigger();
    ctx.author = "npub1u9l940mmrk7nv0cw6maa5fz4vztj0vj42s5dagj38zx9gtxj7qls94fpux".to_owned();
    let out = resolve_template("{{trigger.author | npub}}", &ctx, &HashMap::new()).unwrap();
    assert_eq!(out, ctx.author);
}

#[test]
fn resolve_unknown_filter_returns_error() {
    let ctx = make_trigger();
    let err = resolve_template(
        "{{trigger.text | nonexistent_filter}}",
        &ctx,
        &HashMap::new(),
    )
    .unwrap_err();
    assert!(matches!(err, WorkflowError::TemplateError(_)));
}

#[test]
fn resolve_truncate_invalid_number_returns_error() {
    let ctx = make_trigger();
    let err =
        resolve_template("{{trigger.text | truncate(abc)}}", &ctx, &HashMap::new()).unwrap_err();
    assert!(matches!(err, WorkflowError::TemplateError(_)));
}

#[test]
fn resolve_adjacent_templates_no_separator() {
    let ctx = make_trigger();
    let out =
        resolve_template("{{trigger.author}}{{trigger.emoji}}", &ctx, &HashMap::new()).unwrap();
    assert_eq!(out, "abc123def456fire");
}

#[tokio::test]
async fn condition_and_expression_both_true() {
    let ctx = make_trigger(); // text = "P1 incident in production"
    let result = evaluate_condition(
        "str_contains(trigger_text, \"P1\") && str_contains(trigger_text, \"production\")",
        &ctx,
        &HashMap::new(),
    )
    .await
    .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_and_expression_one_false() {
    let ctx = make_trigger(); // text = "P1 incident in production"
    let result = evaluate_condition(
        "str_contains(trigger_text, \"P1\") && str_contains(trigger_text, \"staging\")",
        &ctx,
        &HashMap::new(),
    )
    .await
    .unwrap();
    assert!(!result);
}

#[tokio::test]
async fn condition_not_expression() {
    let ctx = make_trigger(); // text = "P1 incident in production"
    let result = evaluate_condition("!str_contains(trigger_text, \"P2\")", &ctx, &HashMap::new())
        .await
        .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_str_starts_with() {
    let ctx = make_trigger(); // text = "P1 incident in production"
    let result = evaluate_condition(
        "str_starts_with(trigger_text, \"P1\")",
        &ctx,
        &HashMap::new(),
    )
    .await
    .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_str_ends_with() {
    let ctx = make_trigger(); // text = "P1 incident in production"
    let result = evaluate_condition(
        "str_ends_with(trigger_text, \"production\")",
        &ctx,
        &HashMap::new(),
    )
    .await
    .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_str_len() {
    let ctx = make_trigger(); // text = "P1 incident in production" (25 chars)
    let result = evaluate_condition("str_len(trigger_text) > 10", &ctx, &HashMap::new())
        .await
        .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_str_len_exact() {
    let mut ctx = make_trigger();
    ctx.text = "hello".to_owned(); // exactly 5 chars
    let result = evaluate_condition("str_len(trigger_text) == 5", &ctx, &HashMap::new())
        .await
        .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_emoji_field() {
    let ctx = make_trigger(); // emoji = "fire"
    let result = evaluate_condition("trigger_emoji == \"fire\"", &ctx, &HashMap::new())
        .await
        .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_author_field() {
    let ctx = make_trigger(); // author = "abc123def456"
    let result = evaluate_condition(
        "str_starts_with(trigger_author, \"abc\")",
        &ctx,
        &HashMap::new(),
    )
    .await
    .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_webhook_field_registered() {
    let mut ctx = make_trigger();
    ctx.webhook_fields
        .insert("severity".to_owned(), "critical".to_owned());
    let result = evaluate_condition("trigger_severity == \"critical\"", &ctx, &HashMap::new())
        .await
        .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_step_output_string_comparison() {
    let ctx = make_trigger();
    let mut outputs = HashMap::new();
    outputs.insert("fetch".to_owned(), json!({ "status": "ok" }));
    let result = evaluate_condition("steps_fetch_output_status == \"ok\"", &ctx, &outputs)
        .await
        .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_step_output_integer_comparison() {
    let ctx = make_trigger();
    let mut outputs = HashMap::new();
    outputs.insert("count".to_owned(), json!({ "n": 5 }));
    let result = evaluate_condition("steps_count_output_n >= 5", &ctx, &outputs)
        .await
        .unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_complex_nested_boolean() {
    let ctx = make_trigger(); // text = "P1 incident in production"
    let result = evaluate_condition(
            "(str_contains(trigger_text, \"P1\") || str_contains(trigger_text, \"P2\")) && str_contains(trigger_text, \"production\")",
            &ctx,
            &HashMap::new(),
        )
        .await.unwrap();
    assert!(result);
}

#[tokio::test]
async fn condition_false_literal() {
    let ctx = make_trigger();
    let result = evaluate_condition("false", &ctx, &HashMap::new())
        .await
        .unwrap();
    assert!(!result);
}

#[tokio::test]
async fn condition_true_literal() {
    let ctx = make_trigger();
    let result = evaluate_condition("true", &ctx, &HashMap::new())
        .await
        .unwrap();
    assert!(result);
}

#[test]
fn trigger_context_get_field_known_fields() {
    let ctx = make_trigger();
    assert_eq!(ctx.get_field("text"), Some("P1 incident in production"));
    assert_eq!(ctx.get_field("author"), Some("abc123def456"));
    assert_eq!(ctx.get_field("channel_id"), Some("channel-uuid-here"));
    assert_eq!(ctx.get_field("timestamp"), Some("1700000000"));
    assert_eq!(ctx.get_field("emoji"), Some("fire"));
    assert_eq!(ctx.get_field("message_id"), Some("event-id-hex"));
}

#[test]
fn trigger_context_get_field_unknown_returns_none() {
    let ctx = make_trigger();
    assert!(ctx.get_field("nonexistent").is_none());
    assert!(ctx.get_field("").is_none());
}

#[test]
fn trigger_context_get_field_webhook_field() {
    let mut ctx = make_trigger();
    ctx.webhook_fields
        .insert("repo".to_owned(), "buzz".to_owned());
    assert_eq!(ctx.get_field("repo"), Some("buzz"));
}

#[test]
fn trigger_context_default_has_empty_fields() {
    let ctx = TriggerContext::default();
    assert_eq!(ctx.text, "");
    assert_eq!(ctx.author, "");
    assert_eq!(ctx.channel_id, "");
    assert_eq!(ctx.timestamp, "");
    assert_eq!(ctx.emoji, "");
    assert_eq!(ctx.message_id, "");
    assert!(ctx.webhook_fields.is_empty());
}

#[test]
fn send_message_uses_bound_workflow_channel_by_default() {
    let workflow_channel_id = Uuid::new_v4();
    let resolved = resolve_send_message_channel(None, "", Some(workflow_channel_id))
        .expect("bound channel should be used");
    assert_eq!(resolved, workflow_channel_id.to_string());
}

#[test]
fn send_message_rejects_cross_channel_override_for_bound_workflow() {
    let workflow_channel_id = Uuid::new_v4();
    let other_channel_id = Uuid::new_v4();
    let err = resolve_send_message_channel(
        Some(&other_channel_id.to_string()),
        "",
        Some(workflow_channel_id),
    )
    .unwrap_err();
    assert!(matches!(err, WorkflowError::InvalidDefinition(_)));
    assert!(
        err.to_string().contains("channel override must match"),
        "unexpected error: {err}"
    );
}

#[test]
fn send_message_canonicalizes_valid_explicit_override_for_global_workflow() {
    let override_channel_id = Uuid::new_v4();
    let resolved = resolve_send_message_channel(Some(&override_channel_id.to_string()), "", None)
        .expect("override should be accepted");
    assert_eq!(resolved, override_channel_id.to_string());
}
