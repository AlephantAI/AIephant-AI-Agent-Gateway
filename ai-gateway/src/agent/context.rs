use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentContext {
    pub agent_id_external: Option<String>,
    pub agent_name: Option<String>,
    pub agent_uid: Option<Uuid>,
    pub run_id: Option<String>,
    pub step_id: Option<String>,
    pub parent_step_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub handoff_id: Option<String>,
    pub graph_node: Option<String>,
    pub iteration: Option<u32>,
    pub state_hash: Option<String>,
    pub step_kind: Option<AgentStepKind>,
    pub step_source: AgentStepSource,
    pub step_confidence: AgentConfidence,
    pub trust_level: AgentTrustLevel,
    pub partial: bool,
}

impl AgentContext {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.agent_id_external.is_none()
            && self.agent_name.is_none()
            && self.agent_uid.is_none()
            && self.run_id.is_none()
            && self.step_id.is_none()
            && self.parent_step_id.is_none()
            && self.tool_call_id.is_none()
            && self.handoff_id.is_none()
            && self.graph_node.is_none()
            && self.iteration.is_none()
            && self.state_hash.is_none()
            && self.step_kind.is_none()
            && self.step_source == AgentStepSource::default()
            && self.step_confidence == AgentConfidence::default()
            && self.trust_level == AgentTrustLevel::default()
    }

    #[must_use]
    pub fn agent_identity_for_namespace(
        &self,
        _workspace_id: Option<Uuid>,
        virtual_key_id: Option<Uuid>,
    ) -> String {
        if let Some(uid) = self.agent_uid {
            return uid.to_string();
        }
        if let Some(external) = self.agent_id_external.as_deref() {
            return external.to_string();
        }
        virtual_key_id
            .map(|id| format!("vk:{id}"))
            .unwrap_or_else(|| "unknown".to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentStepKind {
    Planning,
    Reasoning,
    LlmCall,
    ToolCall,
    ToolResult,
    Handoff,
    Approval,
    Checkpoint,
    FinalAnswer,
    Retry,
    ErrorRecovery,
    #[default]
    Unknown,
}

impl AgentStepKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Reasoning => "reasoning",
            Self::LlmCall => "llm_call",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Handoff => "handoff",
            Self::Approval => "approval",
            Self::Checkpoint => "checkpoint",
            Self::FinalAnswer => "final_answer",
            Self::Retry => "retry",
            Self::ErrorRecovery => "error_recovery",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for AgentStepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentStepKind {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value.trim().to_ascii_lowercase().as_str() {
            "planning" => Self::Planning,
            "reasoning" => Self::Reasoning,
            "llm_call" | "llm-call" => Self::LlmCall,
            "tool_call" | "tool-call" => Self::ToolCall,
            "tool_result" | "tool-result" => Self::ToolResult,
            "handoff" => Self::Handoff,
            "approval" => Self::Approval,
            "checkpoint" => Self::Checkpoint,
            "final_answer" | "final-answer" => Self::FinalAnswer,
            "retry" => Self::Retry,
            "error_recovery" | "error-recovery" => Self::ErrorRecovery,
            _ => Self::Unknown,
        })
    }
}

impl<'de> Deserialize<'de> for AgentStepKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(value.parse().expect("AgentStepKind parser is infallible"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentStepSource {
    Runtime,
    Gateway,
    Rule,
    Heuristic,
    #[default]
    Unknown,
}

impl AgentStepSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Gateway => "gateway",
            Self::Rule => "rule",
            Self::Heuristic => "heuristic",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for AgentStepSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentStepSource {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value.trim().to_ascii_lowercase().as_str() {
            "runtime" => Self::Runtime,
            "gateway" => Self::Gateway,
            "rule" => Self::Rule,
            "heuristic" => Self::Heuristic,
            _ => Self::Unknown,
        })
    }
}

impl<'de> Deserialize<'de> for AgentStepSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(value.parse().expect("AgentStepSource parser is infallible"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentConfidence {
    High,
    Medium,
    Low,
    #[default]
    Unknown,
}

impl AgentConfidence {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for AgentConfidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentConfidence {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value.trim().to_ascii_lowercase().as_str() {
            "high" => Self::High,
            "medium" => Self::Medium,
            "low" => Self::Low,
            _ => Self::Unknown,
        })
    }
}

impl<'de> Deserialize<'de> for AgentConfidence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(value.parse().expect("AgentConfidence parser is infallible"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventPhase {
    Before,
    After,
    State,
    #[default]
    Unknown,
}

impl AgentEventPhase {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
            Self::State => "state",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for AgentEventPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentEventPhase {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value.trim().to_ascii_lowercase().as_str() {
            "before" => Self::Before,
            "after" => Self::After,
            "state" => Self::State,
            _ => Self::Unknown,
        })
    }
}

impl<'de> Deserialize<'de> for AgentEventPhase {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(value.parse().expect("AgentEventPhase parser is infallible"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentPolicyStage {
    PreAction,
    PostAction,
    #[default]
    AuditOnly,
}

impl AgentPolicyStage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreAction => "pre_action",
            Self::PostAction => "post_action",
            Self::AuditOnly => "audit_only",
        }
    }
}

impl fmt::Display for AgentPolicyStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentPolicyStage {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value.trim().to_ascii_lowercase().as_str() {
            "pre_action" | "pre-action" => Self::PreAction,
            "post_action" | "post-action" => Self::PostAction,
            "audit_only" | "audit-only" => Self::AuditOnly,
            _ => Self::AuditOnly,
        })
    }
}

impl<'de> Deserialize<'de> for AgentPolicyStage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(value
            .parse()
            .expect("AgentPolicyStage parser is infallible"))
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AgentPolicyMode {
    #[default]
    Audit,
    Enforce,
}

impl AgentPolicyMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Audit => "audit",
            Self::Enforce => "enforce",
        }
    }
}

impl fmt::Display for AgentPolicyMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentPolicyMode {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value.trim().to_ascii_lowercase().as_str() {
            "enforce" => Self::Enforce,
            _ => Self::Audit,
        })
    }
}

impl<'de> Deserialize<'de> for AgentPolicyMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(value.parse().expect("AgentPolicyMode parser is infallible"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventSourceTrust {
    #[default]
    SelfReported,
    AdapterDetected,
    GatewayObserved,
    Registered,
}

impl AgentEventSourceTrust {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SelfReported => "self_reported",
            Self::AdapterDetected => "adapter_detected",
            Self::GatewayObserved => "gateway_observed",
            Self::Registered => "registered",
        }
    }
}

impl fmt::Display for AgentEventSourceTrust {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentEventSourceTrust {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value.trim().to_ascii_lowercase().as_str() {
            "adapter_detected" | "adapter-detected" => Self::AdapterDetected,
            "gateway_observed" | "gateway-observed" => Self::GatewayObserved,
            "registered" => Self::Registered,
            _ => Self::SelfReported,
        })
    }
}

impl<'de> Deserialize<'de> for AgentEventSourceTrust {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(value
            .parse()
            .expect("AgentEventSourceTrust parser is infallible"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentTrustLevel {
    #[default]
    SelfReported,
    AuthBound,
    RegistryVerified,
    SystemDerived,
}

impl AgentTrustLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SelfReported => "self_reported",
            Self::AuthBound => "auth_bound",
            Self::RegistryVerified => "registry_verified",
            Self::SystemDerived => "system_derived",
        }
    }
}

impl fmt::Display for AgentTrustLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    #[test]
    fn step_kind_parses_known_and_unknown_values() {
        assert_eq!(
            "planning".parse::<AgentStepKind>().unwrap(),
            AgentStepKind::Planning
        );
        assert_eq!(
            "tool_call".parse::<AgentStepKind>().unwrap(),
            AgentStepKind::ToolCall
        );
        assert_eq!(
            "not-a-kind".parse::<AgentStepKind>().unwrap(),
            AgentStepKind::Unknown
        );
    }

    #[test]
    fn event_phase_parses_known_values_and_defaults_unknown() {
        assert_eq!(
            "before".parse::<AgentEventPhase>().unwrap(),
            AgentEventPhase::Before
        );
        assert_eq!(
            "after".parse::<AgentEventPhase>().unwrap(),
            AgentEventPhase::After
        );
        assert_eq!(
            "state".parse::<AgentEventPhase>().unwrap(),
            AgentEventPhase::State
        );
        assert_eq!(
            "future".parse::<AgentEventPhase>().unwrap(),
            AgentEventPhase::Unknown
        );
    }

    #[test]
    fn policy_stage_parses_known_values_and_defaults_audit_only() {
        assert_eq!(
            "pre_action".parse::<AgentPolicyStage>().unwrap(),
            AgentPolicyStage::PreAction
        );
        assert_eq!(
            "post-action".parse::<AgentPolicyStage>().unwrap(),
            AgentPolicyStage::PostAction
        );
        assert_eq!(
            "audit_only".parse::<AgentPolicyStage>().unwrap(),
            AgentPolicyStage::AuditOnly
        );
        assert_eq!(
            "future".parse::<AgentPolicyStage>().unwrap(),
            AgentPolicyStage::AuditOnly
        );
    }

    #[test]
    fn policy_mode_parses_known_values_and_defaults_audit() {
        assert_eq!(
            "audit".parse::<AgentPolicyMode>().unwrap(),
            AgentPolicyMode::Audit
        );
        assert_eq!(
            "enforce".parse::<AgentPolicyMode>().unwrap(),
            AgentPolicyMode::Enforce
        );
        assert_eq!(
            "future".parse::<AgentPolicyMode>().unwrap(),
            AgentPolicyMode::Audit
        );
    }

    #[test]
    fn source_trust_parses_known_values_and_defaults_self_reported() {
        assert_eq!(
            "self_reported".parse::<AgentEventSourceTrust>().unwrap(),
            AgentEventSourceTrust::SelfReported
        );
        assert_eq!(
            "adapter-detected".parse::<AgentEventSourceTrust>().unwrap(),
            AgentEventSourceTrust::AdapterDetected
        );
        assert_eq!(
            "gateway_observed".parse::<AgentEventSourceTrust>().unwrap(),
            AgentEventSourceTrust::GatewayObserved
        );
        assert_eq!(
            "registered".parse::<AgentEventSourceTrust>().unwrap(),
            AgentEventSourceTrust::Registered
        );
        assert_eq!(
            "future".parse::<AgentEventSourceTrust>().unwrap(),
            AgentEventSourceTrust::SelfReported
        );
    }

    #[test]
    fn is_empty_detects_each_context_data_field() {
        let contexts = [
            AgentContext {
                agent_name: Some("Support Bot".to_string()),
                ..AgentContext::default()
            },
            AgentContext {
                agent_uid: Some(Uuid::from_u128(42)),
                ..AgentContext::default()
            },
            AgentContext {
                parent_step_id: Some("parent-step".to_string()),
                ..AgentContext::default()
            },
            AgentContext {
                handoff_id: Some("handoff-1".to_string()),
                ..AgentContext::default()
            },
            AgentContext {
                iteration: Some(1),
                ..AgentContext::default()
            },
            AgentContext {
                state_hash: Some("state-hash".to_string()),
                ..AgentContext::default()
            },
            AgentContext {
                step_kind: Some(AgentStepKind::Planning),
                ..AgentContext::default()
            },
            AgentContext {
                step_source: AgentStepSource::Runtime,
                ..AgentContext::default()
            },
            AgentContext {
                step_confidence: AgentConfidence::High,
                ..AgentContext::default()
            },
            AgentContext {
                trust_level: AgentTrustLevel::AuthBound,
                ..AgentContext::default()
            },
        ];

        for ctx in contexts {
            assert!(!ctx.is_empty(), "{ctx:?} should not be empty");
        }

        assert!(
            AgentContext {
                partial: true,
                ..AgentContext::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn namespace_prefers_agent_uid() {
        let workspace_id = Uuid::nil();
        let agent_uid = Uuid::from_u128(42);
        let ctx = AgentContext {
            agent_id_external: Some("external".to_string()),
            agent_uid: Some(agent_uid),
            run_id: Some("run-1".to_string()),
            ..AgentContext::default()
        };

        assert_eq!(
            ctx.agent_identity_for_namespace(Some(workspace_id), Some(Uuid::from_u128(7))),
            "00000000-0000-0000-0000-00000000002a"
        );
    }

    #[test]
    fn namespace_falls_back_to_external_agent_then_virtual_key() {
        let workspace_id = Uuid::nil();
        let vk = Uuid::from_u128(7);
        let with_agent = AgentContext {
            agent_id_external: Some("coding-agent".to_string()),
            ..AgentContext::default()
        };
        let without_agent = AgentContext::default();

        assert_eq!(
            with_agent.agent_identity_for_namespace(Some(workspace_id), Some(vk)),
            "coding-agent"
        );
        assert_eq!(
            without_agent.agent_identity_for_namespace(Some(workspace_id), Some(vk)),
            "vk:00000000-0000-0000-0000-000000000007"
        );
    }
}
