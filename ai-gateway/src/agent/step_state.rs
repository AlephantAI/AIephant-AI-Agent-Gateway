use std::borrow::Cow;

use uuid::Uuid;

use crate::{
    agent::context::AgentStepKind, app_redis::AppRedis,
    config::agent::AgentConflictAction,
};

#[derive(Debug, Clone)]
pub struct StepFingerprintInput {
    pub parent_step_id: Option<String>,
    pub step_kind: Option<AgentStepKind>,
    pub graph_node: Option<String>,
    pub tool_call_id: Option<String>,
    pub attempt: Option<u32>,
    pub input_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepConflictDecision {
    NoConflict,
    ConflictWarn,
    ConflictStrict,
    Disabled,
}

#[must_use]
pub fn step_fingerprint(input: &StepFingerprintInput) -> String {
    format!(
        "parent={}|kind={}|graph={}|tool={}|attempt={}|input={}",
        encode_component(input.parent_step_id.as_deref().unwrap_or(""), false),
        input.step_kind.map_or("unknown", AgentStepKind::as_str),
        encode_component(input.graph_node.as_deref().unwrap_or(""), false),
        encode_component(input.tool_call_id.as_deref().unwrap_or(""), false),
        input.attempt.map_or_else(String::new, |v| v.to_string()),
        encode_component(input.input_hash.as_deref().unwrap_or(""), false)
    )
}

#[must_use]
pub fn step_state_key(
    workspace_id: Uuid,
    agent_identity: &str,
    run_id: &str,
    step_id: &str,
) -> String {
    format!(
        "agent:step:{workspace_id}:{}:{}:{}",
        encode_component(agent_identity, true),
        encode_component(run_id, true),
        encode_component(step_id, true)
    )
}

fn encode_component(value: &str, encode_colon: bool) -> Cow<'_, str> {
    if !value
        .chars()
        .any(|ch| should_percent_encode(ch, encode_colon))
    {
        return Cow::Borrowed(value);
    }

    let mut encoded = String::with_capacity(value.len());
    for ch in value.chars() {
        if should_percent_encode(ch, encode_colon) {
            let byte = ch as u8;
            encoded.push('%');
            encoded.push(hex_digit(byte >> 4));
            encoded.push(hex_digit(byte & 0x0f));
        } else {
            encoded.push(ch);
        }
    }
    Cow::Owned(encoded)
}

const fn should_percent_encode(ch: char, encode_colon: bool) -> bool {
    ch == '%' || ch == '|' || ch == '=' || (encode_colon && ch == ':')
}

const fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'A' + (value - 10)) as char,
        _ => unreachable!(),
    }
}

pub async fn detect_step_conflict(
    redis: Option<&AppRedis>,
    key: &str,
    fingerprint: &str,
    ttl_secs: u64,
    action: AgentConflictAction,
) -> Result<StepConflictDecision, redis::RedisError> {
    if matches!(action, AgentConflictAction::Disabled) {
        return Ok(StepConflictDecision::Disabled);
    }
    let Some(redis) = redis else {
        return Ok(StepConflictDecision::NoConflict);
    };
    if redis.set_nx_ex(key, fingerprint, ttl_secs).await? {
        return Ok(StepConflictDecision::NoConflict);
    }
    let existing = redis.get_string(key).await?;
    if existing.as_deref() == Some(fingerprint) {
        Ok(StepConflictDecision::NoConflict)
    } else if matches!(action, AgentConflictAction::Strict) {
        Ok(StepConflictDecision::ConflictStrict)
    } else {
        Ok(StepConflictDecision::ConflictWarn)
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::{
        agent::context::AgentStepKind, config::agent::AgentConflictAction,
    };

    #[test]
    fn fingerprint_is_stable_and_distinguishes_attempts() {
        let first = StepFingerprintInput {
            parent_step_id: Some("parent".to_string()),
            step_kind: Some(AgentStepKind::ToolCall),
            graph_node: Some("tools".to_string()),
            tool_call_id: Some("call".to_string()),
            attempt: Some(1),
            input_hash: Some("sha256:a".to_string()),
        };
        let second = StepFingerprintInput {
            attempt: Some(2),
            ..first.clone()
        };

        assert_eq!(
            step_fingerprint(&first),
            "parent=parent|kind=tool_call|graph=tools|tool=call|attempt=1|input=sha256:a"
        );
        assert_ne!(step_fingerprint(&first), step_fingerprint(&second));
    }

    #[test]
    fn step_state_key_namespaces_by_workspace_agent_run_and_step() {
        let workspace_id =
            Uuid::parse_str("018fdc6b-b65f-7c20-8000-000000000001").unwrap();

        assert_eq!(
            step_state_key(workspace_id, "agent-a", "run-1", "step-1"),
            "agent:step:018fdc6b-b65f-7c20-8000-000000000001:agent-a:run-1:\
             step-1"
        );
    }

    #[test]
    fn fingerprint_components_with_delimiters_do_not_collide() {
        let first = StepFingerprintInput {
            parent_step_id: Some("parent".to_string()),
            step_kind: Some(AgentStepKind::ToolCall),
            graph_node: Some("g|tool=t".to_string()),
            tool_call_id: Some("u".to_string()),
            attempt: Some(1),
            input_hash: Some("sha256:%a=b".to_string()),
        };
        let second = StepFingerprintInput {
            graph_node: Some("g".to_string()),
            tool_call_id: Some("t|tool=u".to_string()),
            ..first.clone()
        };

        assert_ne!(step_fingerprint(&first), step_fingerprint(&second));
    }

    #[test]
    fn step_state_key_components_with_delimiters_do_not_collide() {
        let workspace_id =
            Uuid::parse_str("018fdc6b-b65f-7c20-8000-000000000001").unwrap();

        assert_ne!(
            step_state_key(workspace_id, "agent:a", "run", "step%1"),
            step_state_key(workspace_id, "agent", "a:run", "step%1")
        );
    }

    #[tokio::test]
    async fn conflict_detection_returns_disabled_without_redis() {
        let decision = detect_step_conflict(
            None,
            "key",
            "fingerprint",
            60,
            AgentConflictAction::Disabled,
        )
        .await
        .unwrap();

        assert_eq!(decision, StepConflictDecision::Disabled);
    }

    #[tokio::test]
    async fn conflict_detection_treats_missing_redis_as_no_conflict() {
        let decision = detect_step_conflict(
            None,
            "key",
            "fingerprint",
            60,
            AgentConflictAction::Strict,
        )
        .await
        .unwrap();

        assert_eq!(decision, StepConflictDecision::NoConflict);
    }
}
