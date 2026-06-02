use std::{fmt, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::context::{
    AgentConfidence, AgentEventPhase, AgentEventSourceTrust, AgentPolicyMode,
    AgentPolicyStage, AgentStepKind, AgentStepSource, AgentTrustLevel,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AgentEventsRequest {
    Batch {
        #[serde(default)]
        source: Option<AgentEventSource>,
        #[serde(default)]
        framework: Option<AgentEventSource>,
        events: Vec<AgentEventInput>,
    },
    Single(AgentEventInput),
}

impl AgentEventsRequest {
    #[must_use]
    pub fn into_events(self) -> Vec<AgentEventInput> {
        match self {
            Self::Batch { events, .. } => events,
            Self::Single(event) => vec![event],
        }
    }

    #[must_use]
    pub fn into_sourced_events(self) -> Vec<SourcedAgentEventInput> {
        match self {
            Self::Batch {
                source,
                framework,
                events,
            } => {
                let batch_source =
                    source.or(framework).unwrap_or(AgentEventSource::Unknown);
                events
                    .into_iter()
                    .map(|event| {
                        let source = event
                            .source
                            .or(event.framework)
                            .unwrap_or(batch_source);
                        SourcedAgentEventInput { source, event }
                    })
                    .collect()
            }
            Self::Single(event) => {
                let source = event
                    .source
                    .or(event.framework)
                    .unwrap_or(AgentEventSource::Unknown);
                vec![SourcedAgentEventInput { source, event }]
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct SourcedAgentEventInput {
    pub source: AgentEventSource,
    pub event: AgentEventInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventSource {
    Alephant,
    LangGraph,
    OpenAiAgents,
    N8n,
    CrewAi,
    Mastra,
    #[default]
    Unknown,
}

impl AgentEventSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Alephant => "alephant",
            Self::LangGraph => "langgraph",
            Self::OpenAiAgents => "openai_agents",
            Self::N8n => "n8n",
            Self::CrewAi => "crewai",
            Self::Mastra => "mastra",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for AgentEventSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentEventSource {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value.trim().to_ascii_lowercase().as_str() {
            "alephant" | "alephant_agent" | "alephant-agent" => Self::Alephant,
            "langgraph" | "lang_graph" | "lang-graph" => Self::LangGraph,
            "openai_agents" | "openai-agents" | "openaiagents"
            | "openai_agent_sdk" | "openai-agent-sdk" => Self::OpenAiAgents,
            "n8n" => Self::N8n,
            "crew_ai" | "crew-ai" | "crewai" => Self::CrewAi,
            "mastra" => Self::Mastra,
            _ => Self::Unknown,
        })
    }
}

impl<'de> Deserialize<'de> for AgentEventSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(value
            .parse()
            .expect("AgentEventSource parser is infallible"))
    }
}

#[derive(Debug, Clone)]
pub struct AgentEventInput {
    pub source: Option<AgentEventSource>,
    pub framework: Option<AgentEventSource>,
    pub version: String,
    pub event_id: Option<String>,
    pub event_type: String,
    pub event_phase: AgentEventPhase,
    pub policy_stage: AgentPolicyStage,
    pub event_source_trust: AgentEventSourceTrust,
    pub sequence: Option<u64>,
    pub name: Option<String>,
    pub agent_name: Option<String>,
    pub agent_id_external: Option<String>,
    pub run_id: Option<String>,
    pub step_id: Option<String>,
    pub parent_step_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub handoff_id: Option<String>,
    pub graph_node: Option<String>,
    pub step_kind: Option<AgentStepKind>,
    pub step_source: AgentStepSource,
    pub step_confidence: AgentConfidence,
    pub attempt: Option<u32>,
    pub input_hash: Option<String>,
    pub timestamp: Option<DateTime<Utc>>,
    pub metadata: serde_json::Value,
    pub raw_fields: serde_json::Map<String, serde_json::Value>,
}

impl<'de> Deserialize<'de> for AgentEventInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = AgentEventInputWire::deserialize(deserializer)?;
        let agent_name =
            raw.agent_name.or_else(|| raw.agent_name_camel.clone());
        if agent_name.is_some() && raw.agent_name_camel.is_some() {
            tracing::debug!(
                "agent_name and agentName both present in agent event; using \
                 agent_name"
            );
        }

        Ok(Self {
            source: raw.source,
            framework: raw.framework,
            version: raw.version,
            event_id: raw.event_id,
            event_type: raw.event_type,
            event_phase: raw.event_phase,
            policy_stage: raw.policy_stage,
            event_source_trust: raw.event_source_trust,
            sequence: raw.sequence,
            name: raw.name,
            agent_name,
            agent_id_external: raw.agent_id_external,
            run_id: raw.run_id,
            step_id: raw.step_id,
            parent_step_id: raw.parent_step_id,
            tool_call_id: raw.tool_call_id,
            handoff_id: raw.handoff_id,
            graph_node: raw.graph_node,
            step_kind: raw.step_kind,
            step_source: raw.step_source,
            step_confidence: raw.step_confidence,
            attempt: raw.attempt,
            input_hash: raw.input_hash,
            timestamp: raw.timestamp,
            metadata: raw.metadata,
            raw_fields: raw.raw_fields,
        })
    }
}

#[derive(Debug, Deserialize)]
struct AgentEventInputWire {
    #[serde(default)]
    source: Option<AgentEventSource>,
    #[serde(default)]
    framework: Option<AgentEventSource>,
    #[serde(default = "default_version")]
    version: String,
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default, rename = "type", alias = "event")]
    event_type: String,
    #[serde(default)]
    event_phase: AgentEventPhase,
    #[serde(default)]
    policy_stage: AgentPolicyStage,
    #[serde(default)]
    event_source_trust: AgentEventSourceTrust,
    #[serde(default)]
    sequence: Option<u64>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "agent_name")]
    agent_name: Option<String>,
    #[serde(default, rename = "agentName")]
    agent_name_camel: Option<String>,
    #[serde(default, rename = "agent_id")]
    agent_id_external: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    step_id: Option<String>,
    #[serde(default)]
    parent_step_id: Option<String>,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    handoff_id: Option<String>,
    #[serde(default)]
    graph_node: Option<String>,
    #[serde(default)]
    step_kind: Option<AgentStepKind>,
    #[serde(default)]
    step_source: AgentStepSource,
    #[serde(default)]
    step_confidence: AgentConfidence,
    #[serde(default)]
    attempt: Option<u32>,
    #[serde(default)]
    input_hash: Option<String>,
    #[serde(default)]
    timestamp: Option<DateTime<Utc>>,
    #[serde(default = "empty_metadata")]
    metadata: serde_json::Value,
    #[serde(flatten)]
    raw_fields: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentEventEnvelope {
    pub version: String,
    pub event_id: String,
    pub event_type: String,
    pub event_source: AgentEventSource,
    pub event_phase: AgentEventPhase,
    pub policy_stage: AgentPolicyStage,
    pub policy_mode: AgentPolicyMode,
    pub event_source_trust: AgentEventSourceTrust,
    pub sequence: Option<u64>,
    pub observed_at: DateTime<Utc>,
    pub timestamp: Option<DateTime<Utc>>,
    pub name: Option<String>,
    pub alephant_agent_name: Option<String>,
    pub alephant_agent_name_source: Option<String>,
    pub alephant_agent_trust_level: Option<String>,
    pub workspace_id: String,
    pub virtual_key_id: Option<Uuid>,
    pub agent_id_external: Option<String>,
    pub agent_uid: Option<Uuid>,
    pub run_id: Option<String>,
    pub step_id: Option<String>,
    pub parent_step_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub handoff_id: Option<String>,
    pub graph_node: Option<String>,
    pub step_kind: Option<AgentStepKind>,
    pub step_source: AgentStepSource,
    pub step_confidence: AgentConfidence,
    pub trust_level: AgentTrustLevel,
    pub context_conflict: bool,
    pub step_id_conflict: bool,
    pub attempt: Option<u32>,
    pub input_hash: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentEventsResponse {
    pub accepted: usize,
    pub rejected: usize,
    pub allowed: bool,
    pub decisions: Vec<AgentPolicyDecision>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentPolicyDecision {
    pub event_id: String,
    pub event_type: String,
    pub run_id: Option<String>,
    pub step_id: Option<String>,
    pub allowed: bool,
    pub policy_decision: String,
    pub policy_stage: String,
    pub sink_status: String,
    pub reason: String,
    pub blocked_by: String,
    pub route_hint: String,
    pub snapshot_revision: i64,
    pub reason_message: String,
    pub policy_id: String,
    pub policy_scope: String,
    pub violations: Vec<AgentPolicyIssueDto>,
    pub warnings: Vec<AgentPolicyIssueDto>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentPolicyIssueDto {
    pub field: String,
    pub reason: String,
    pub blocked_by: String,
    pub reason_message: String,
    pub actual: String,
    pub expected: Vec<String>,
    pub actual_value: Option<f64>,
    pub expected_value: Option<f64>,
    pub unit: String,
    pub operator: String,
}

fn default_version() -> String {
    "2026-05-27".to_string()
}

fn empty_metadata() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agent::context::{
        AgentConfidence, AgentEventPhase, AgentEventSourceTrust,
        AgentPolicyStage, AgentStepKind, AgentStepSource,
    };

    #[test]
    fn deserializes_batch_request() {
        let raw = json!({
            "events": [{
                "version": "2026-05-27",
                "event_id": "evt_1",
                "type": "step.started",
                "agent_name": "Support Bot",
                "agent_id": "coding-agent",
                "run_id": "run_1",
                "step_id": "step_1",
                "step_kind": "planning",
                "step_source": "runtime",
                "step_confidence": "high",
                "attempt": 1,
                "input_hash": "sha256:a",
                "timestamp": "2026-05-29T12:00:00Z",
                "metadata": { "safe": "value" }
            }]
        });

        let req: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let events = req.into_events();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].version, "2026-05-27");
        assert_eq!(events[0].event_id.as_deref(), Some("evt_1"));
        assert_eq!(events[0].event_type, "step.started");
        assert_eq!(events[0].agent_name.as_deref(), Some("Support Bot"));
        assert_eq!(
            events[0].agent_id_external.as_deref(),
            Some("coding-agent")
        );
        assert_eq!(events[0].run_id.as_deref(), Some("run_1"));
        assert_eq!(events[0].step_id.as_deref(), Some("step_1"));
        assert_eq!(events[0].step_kind, Some(AgentStepKind::Planning));
        assert_eq!(events[0].step_source, AgentStepSource::Runtime);
        assert_eq!(events[0].step_confidence, AgentConfidence::High);
        assert_eq!(events[0].attempt, Some(1));
        assert_eq!(events[0].input_hash.as_deref(), Some("sha256:a"));
        assert!(events[0].timestamp.is_some());
        assert_eq!(events[0].metadata["safe"], "value");
    }

    #[test]
    fn single_event_request_is_accepted() {
        let raw = json!({
            "type": "run.started",
            "agent_id": "coding-agent",
            "run_id": "run_1"
        });

        let req: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let events = req.into_events();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].version, "2026-05-27");
        assert_eq!(events[0].event_type, "run.started");
        assert_eq!(
            events[0].agent_id_external.as_deref(),
            Some("coding-agent")
        );
    }

    #[test]
    fn deserializes_camel_case_agent_name() {
        let raw = json!({
            "type": "run.started",
            "agent_id": "coding-agent",
            "agentName": "Support Bot",
            "run_id": "run_1"
        });

        let req: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let events = req.into_events();

        assert_eq!(events[0].agent_name.as_deref(), Some("Support Bot"));
        assert_eq!(events[0].name, None);
    }

    #[test]
    fn snake_case_agent_name_wins_when_both_spellings_are_present() {
        let raw = json!({
            "type": "run.started",
            "agent_id": "coding-agent",
            "agent_name": "Snake Bot",
            "agentName": "Camel Bot",
            "run_id": "run_1"
        });

        let req: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let events = req.into_events();

        assert_eq!(events[0].agent_name.as_deref(), Some("Snake Bot"));
        assert!(!events[0].raw_fields.contains_key("agentName"));
    }

    #[test]
    fn unsupported_agent_nam_is_not_agent_name() {
        let raw = json!({
            "type": "run.started",
            "agent_id": "coding-agent",
            "agentNam": "Typo Bot",
            "run_id": "run_1"
        });

        let req: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let events = req.into_events();

        assert_eq!(events[0].agent_name, None);
        assert_eq!(events[0].raw_fields["agentNam"], "Typo Bot");
    }

    #[test]
    fn unknown_taxonomy_values_deserialize_as_unknown() {
        let raw = json!({
            "type": "step.started",
            "step_kind": "future_kind",
            "step_source": "adapter",
            "step_confidence": "certain"
        });

        let req: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let events = req.into_events();

        assert_eq!(events[0].step_kind, Some(AgentStepKind::Unknown));
        assert_eq!(events[0].step_source, AgentStepSource::Unknown);
        assert_eq!(events[0].step_confidence, AgentConfidence::Unknown);
    }

    #[test]
    fn framework_sources_deserialize_with_aliases() {
        let cases = [
            ("openai-agent-sdk", AgentEventSource::OpenAiAgents),
            ("openai_agent_sdk", AgentEventSource::OpenAiAgents),
            ("crew-ai", AgentEventSource::CrewAi),
            ("crew_ai", AgentEventSource::CrewAi),
            ("crewai", AgentEventSource::CrewAi),
            ("mastra", AgentEventSource::Mastra),
        ];

        for (source, expected) in cases {
            let raw = json!({
                "source": source,
                "type": "run.started"
            });
            let req: AgentEventsRequest = serde_json::from_value(raw).unwrap();
            let sourced = req.into_sourced_events();
            assert_eq!(sourced[0].source, expected);
        }
    }

    #[test]
    fn deserializes_event_phase_policy_stage_source_trust_and_sequence() {
        let raw = json!({
            "type": "tool.call.requested",
            "event_phase": "before",
            "policy_stage": "pre_action",
            "event_source_trust": "adapter_detected",
            "sequence": 7
        });

        let req: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let events = req.into_events();

        assert_eq!(events[0].event_phase, AgentEventPhase::Before);
        assert_eq!(events[0].policy_stage, AgentPolicyStage::PreAction);
        assert_eq!(
            events[0].event_source_trust,
            AgentEventSourceTrust::AdapterDetected
        );
        assert_eq!(events[0].sequence, Some(7));
    }

    #[test]
    fn agent_events_response_serializes_policy_decisions() {
        let response = AgentEventsResponse {
            accepted: 1,
            rejected: 0,
            allowed: false,
            decisions: vec![AgentPolicyDecision {
                event_id: "evt-1".to_string(),
                event_type: "tool.call.requested".to_string(),
                run_id: Some("run-1".to_string()),
                step_id: Some("step-1".to_string()),
                allowed: false,
                policy_decision: "denied".to_string(),
                policy_stage: "pre_action".to_string(),
                sink_status: String::new(),
                reason: "agent_tool_denied".to_string(),
                blocked_by: "agent.policy.tool".to_string(),
                route_hint: String::new(),
                snapshot_revision: 123,
                reason_message: "Tool call is not allowed.".to_string(),
                policy_id: "policy-1".to_string(),
                policy_scope: "AGENT_POLICY_SCOPE_AGENT".to_string(),
                violations: vec![AgentPolicyIssueDto {
                    field: "tool_name".to_string(),
                    reason: "not_allowed".to_string(),
                    blocked_by: "agent.policy.tool".to_string(),
                    reason_message: "Tool is not allowlisted.".to_string(),
                    actual: "shell".to_string(),
                    expected: vec!["search".to_string()],
                    actual_value: None,
                    expected_value: None,
                    unit: String::new(),
                    operator: "in".to_string(),
                }],
                warnings: Vec::new(),
            }],
        };

        let value = serde_json::to_value(response).unwrap();

        assert_eq!(value["accepted"], 1);
        assert_eq!(value["rejected"], 0);
        assert_eq!(value["allowed"], false);
        assert_eq!(value["decisions"][0]["eventId"], "evt-1");
        assert_eq!(value["decisions"][0]["eventType"], "tool.call.requested");
        assert_eq!(value["decisions"][0]["runId"], "run-1");
        assert_eq!(value["decisions"][0]["stepId"], "step-1");
        assert_eq!(value["decisions"][0]["allowed"], false);
        assert_eq!(value["decisions"][0]["policyDecision"], "denied");
        assert_eq!(value["decisions"][0]["policyStage"], "pre_action");
        assert_eq!(value["decisions"][0]["sinkStatus"], "");
        assert_eq!(
            value["decisions"][0]["policyScope"],
            "AGENT_POLICY_SCOPE_AGENT"
        );
        assert_eq!(
            value["decisions"][0]["violations"][0]["field"],
            "tool_name"
        );
    }
}
