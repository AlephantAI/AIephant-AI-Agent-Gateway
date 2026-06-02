use crate::agent::{
    context::{
        AgentConfidence, AgentEventPhase, AgentEventSourceTrust,
        AgentPolicyStage, AgentStepKind, AgentStepSource,
    },
    event::{AgentEventInput, AgentEventSource},
};

pub fn adapt_agent_event(
    source: AgentEventSource,
    mut event: AgentEventInput,
) -> AgentEventInput {
    event.source = Some(source);
    match source {
        AgentEventSource::Alephant => {
            copy_allowlisted_raw_fields_to_metadata(&mut event);
            event
        }
        AgentEventSource::LangGraph => adapt_langgraph_event(event),
        AgentEventSource::OpenAiAgents => adapt_openai_agents_event(event),
        AgentEventSource::N8n => adapt_n8n_event(event),
        AgentEventSource::CrewAi => adapt_crewai_event(event),
        AgentEventSource::Mastra => adapt_mastra_event(event),
        AgentEventSource::Unknown => {
            adapt_unknown_event(AgentEventSource::Unknown, event)
        }
    }
}

fn adapt_langgraph_event(mut event: AgentEventInput) -> AgentEventInput {
    let raw_event_type = event.event_type.clone();
    if let Some((event_type, step_kind, phase, stage)) =
        map_langgraph_event_type(&raw_event_type)
    {
        set_mapping(
            &mut event,
            event_type,
            step_kind,
            phase,
            stage,
            AgentStepSource::Runtime,
            AgentConfidence::High,
        );
    }
    if event.graph_node.is_none() {
        event.graph_node = metadata_string(&event.metadata, "langgraph_node")
            .or_else(|| event.name.clone());
    }
    enrich_adapter_metadata(
        &mut event.metadata,
        AgentEventSource::LangGraph,
        &raw_event_type,
        event.name.as_deref(),
    );
    copy_allowlisted_raw_fields_to_metadata(&mut event);
    event
}

fn adapt_openai_agents_event(mut event: AgentEventInput) -> AgentEventInput {
    let run_id = raw_string(&event, "trace_id");
    let step_id =
        raw_string(&event, "span_id").or_else(|| raw_string(&event, "item_id"));
    let parent_step_id = raw_string(&event, "parent_id");
    fill_option(&mut event.run_id, run_id);
    fill_option(&mut event.step_id, step_id);
    fill_option(&mut event.parent_step_id, parent_step_id);

    let raw_event_type = event.event_type.clone();
    let Some((event_type, step_kind, phase, stage)) =
        map_openai_agents_event_type(&raw_event_type)
    else {
        return adapt_unknown_event(AgentEventSource::OpenAiAgents, event);
    };

    set_mapping(
        &mut event,
        event_type,
        step_kind,
        phase,
        stage,
        AgentStepSource::Runtime,
        AgentConfidence::High,
    );
    if is_openai_agents_tool_metadata_event(&raw_event_type) {
        if let Some(name) = event.name.clone() {
            metadata_insert_string(&mut event.metadata, "tool_name", name);
        }
    }
    finish_framework_event(
        event,
        AgentEventSource::OpenAiAgents,
        &raw_event_type,
    )
}

fn adapt_n8n_event(mut event: AgentEventInput) -> AgentEventInput {
    let agent_id_external = raw_string(&event, "workflowId");
    let run_id = raw_string(&event, "executionId");
    let step_id = raw_string(&event, "nodeId");
    let graph_node = raw_string(&event, "nodeName");
    fill_option(&mut event.agent_id_external, agent_id_external);
    fill_option(&mut event.run_id, run_id);
    fill_option(&mut event.step_id, step_id);
    fill_option(&mut event.graph_node, graph_node);
    if let Some(node_type) = raw_string(&event, "nodeType") {
        metadata_insert_string(&mut event.metadata, "node_type", node_type);
    }

    let raw_event_type = event.event_type.clone();
    let node_type = raw_string(&event, "nodeType");
    let Some((event_type, step_kind, phase, stage)) =
        map_n8n_event_type(&raw_event_type, node_type.as_deref())
    else {
        return adapt_unknown_event(AgentEventSource::N8n, event);
    };

    set_mapping(
        &mut event,
        event_type,
        step_kind,
        phase,
        stage,
        AgentStepSource::Runtime,
        AgentConfidence::High,
    );
    finish_framework_event(event, AgentEventSource::N8n, &raw_event_type)
}

fn adapt_crewai_event(mut event: AgentEventInput) -> AgentEventInput {
    let run_id = raw_string(&event, "crew_id");
    let step_id = raw_string(&event, "task_id");
    fill_option(&mut event.run_id, run_id);
    fill_option(&mut event.step_id, step_id);
    if let Some(tool_name) = raw_string(&event, "tool_name") {
        metadata_insert_string(&mut event.metadata, "tool_name", tool_name);
    }

    let raw_event_type = event.event_type.clone();
    let Some((event_type, step_kind, phase, stage)) =
        map_crewai_event_type(&raw_event_type)
    else {
        return adapt_unknown_event(AgentEventSource::CrewAi, event);
    };

    set_mapping(
        &mut event,
        event_type,
        step_kind,
        phase,
        stage,
        AgentStepSource::Runtime,
        AgentConfidence::High,
    );
    finish_framework_event(event, AgentEventSource::CrewAi, &raw_event_type)
}

fn adapt_mastra_event(mut event: AgentEventInput) -> AgentEventInput {
    let run_id = raw_string(&event, "traceId");
    let step_id = raw_string(&event, "spanId");
    fill_option(&mut event.run_id, run_id);
    fill_option(&mut event.step_id, step_id);
    if let Some(tool_name) = raw_string(&event, "toolName") {
        metadata_insert_string(&mut event.metadata, "tool_name", tool_name);
    }

    let raw_event_type = event.event_type.clone();
    let Some((event_type, step_kind, phase, stage)) =
        map_mastra_event_type(&raw_event_type)
    else {
        return adapt_unknown_event(AgentEventSource::Mastra, event);
    };

    set_mapping(
        &mut event,
        event_type,
        step_kind,
        phase,
        stage,
        AgentStepSource::Runtime,
        AgentConfidence::High,
    );
    finish_framework_event(event, AgentEventSource::Mastra, &raw_event_type)
}

fn adapt_unknown_event(
    source: AgentEventSource,
    mut event: AgentEventInput,
) -> AgentEventInput {
    if matches!(source, AgentEventSource::Unknown) {
        event.event_source_trust = AgentEventSourceTrust::SelfReported;
    }
    let raw_event_type = event.event_type.clone();
    if let Some((event_type, step_kind)) =
        map_unknown_event_type(&raw_event_type)
    {
        event.event_type = event_type.to_string();
        event.step_kind = Some(step_kind);
        event.step_source = AgentStepSource::Heuristic;
        event.step_confidence = AgentConfidence::Medium;
        apply_unknown_heuristic_phase_and_stage(&mut event);
    } else {
        event.event_type = "unknown".to_string();
        event.step_kind = Some(AgentStepKind::Unknown);
        event.step_source = AgentStepSource::Heuristic;
        event.step_confidence = AgentConfidence::Low;
        event.event_phase = AgentEventPhase::Unknown;
        event.policy_stage = AgentPolicyStage::AuditOnly;
    }
    enrich_adapter_metadata(
        &mut event.metadata,
        source,
        &raw_event_type,
        event.name.as_deref(),
    );
    copy_allowlisted_raw_fields_to_metadata(&mut event);
    event
}

fn finish_framework_event(
    mut event: AgentEventInput,
    source: AgentEventSource,
    raw_event_type: &str,
) -> AgentEventInput {
    enrich_adapter_metadata(
        &mut event.metadata,
        source,
        raw_event_type,
        event.name.as_deref(),
    );
    copy_allowlisted_raw_fields_to_metadata(&mut event);
    event
}

fn apply_unknown_heuristic_phase_and_stage(event: &mut AgentEventInput) {
    match event.event_type.as_str() {
        "tool.call.requested" | "llm.call.started" | "approval.requested" => {
            event.event_phase = AgentEventPhase::Before;
            event.policy_stage = AgentPolicyStage::PreAction;
        }
        "checkpoint.created" => {
            event.event_phase = AgentEventPhase::State;
            event.policy_stage = AgentPolicyStage::AuditOnly;
        }
        event_type if event_type.ends_with(".completed") => {
            event.event_phase = AgentEventPhase::After;
            event.policy_stage = AgentPolicyStage::AuditOnly;
        }
        _ => {}
    }
}

fn set_mapping(
    event: &mut AgentEventInput,
    event_type: &'static str,
    step_kind: AgentStepKind,
    phase: AgentEventPhase,
    stage: AgentPolicyStage,
    source: AgentStepSource,
    confidence: AgentConfidence,
) {
    event.event_type = event_type.to_string();
    event.step_kind = Some(step_kind);
    event.event_phase = phase;
    event.policy_stage = stage;
    event.step_source = source;
    event.step_confidence = confidence;
}

fn map_langgraph_event_type(
    event_type: &str,
) -> Option<(
    &'static str,
    AgentStepKind,
    AgentEventPhase,
    AgentPolicyStage,
)> {
    match event_type {
        "on_tool_start" => Some((
            "tool.call.requested",
            AgentStepKind::ToolCall,
            AgentEventPhase::Before,
            AgentPolicyStage::PreAction,
        )),
        "on_tool_end" => Some((
            "tool.call.completed",
            AgentStepKind::ToolCall,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        "on_chat_model_start" | "on_llm_start" => Some((
            "llm.call.started",
            AgentStepKind::LlmCall,
            AgentEventPhase::Before,
            AgentPolicyStage::PreAction,
        )),
        "on_chat_model_end" | "on_llm_end" => Some((
            "llm.call.completed",
            AgentStepKind::LlmCall,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        "on_chain_start" => Some((
            "step.started",
            AgentStepKind::Unknown,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        "on_chain_end" => Some((
            "step.completed",
            AgentStepKind::Unknown,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        "on_chain_error" | "on_tool_error" | "on_llm_error" => Some((
            "step.failed",
            AgentStepKind::ErrorRecovery,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        _ => None,
    }
}

fn map_openai_agents_event_type(
    event_type: &str,
) -> Option<(
    &'static str,
    AgentStepKind,
    AgentEventPhase,
    AgentPolicyStage,
)> {
    match event_type {
        "tool_called" => Some((
            "tool.call.requested",
            AgentStepKind::ToolCall,
            AgentEventPhase::Before,
            AgentPolicyStage::PreAction,
        )),
        "tool_output" => Some((
            "tool.result.received",
            AgentStepKind::ToolResult,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        "handoff_requested" => Some((
            "handoff.requested",
            AgentStepKind::Handoff,
            AgentEventPhase::Before,
            AgentPolicyStage::PreAction,
        )),
        "handoff_occurred" => Some((
            "handoff.completed",
            AgentStepKind::Handoff,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        "tool_approval_requested" => Some((
            "approval.requested",
            AgentStepKind::Approval,
            AgentEventPhase::Before,
            AgentPolicyStage::PreAction,
        )),
        "reasoning_item_created" => Some((
            "step.started",
            AgentStepKind::Reasoning,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        _ => None,
    }
}

fn is_openai_agents_tool_metadata_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "tool_called" | "tool_output" | "tool_approval_requested"
    )
}

fn map_n8n_event_type(
    event_type: &str,
    node_type: Option<&str>,
) -> Option<(
    &'static str,
    AgentStepKind,
    AgentEventPhase,
    AgentPolicyStage,
)> {
    let is_tool_node = node_type.is_some_and(is_tool_node_type);
    match event_type {
        "execution.started" => Some((
            "run.started",
            AgentStepKind::Planning,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        "execution.success" => Some((
            "run.completed",
            AgentStepKind::Unknown,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        "execution.error" => Some((
            "run.failed",
            AgentStepKind::ErrorRecovery,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        "node.started" if is_tool_node => Some((
            "tool.call.requested",
            AgentStepKind::ToolCall,
            AgentEventPhase::Before,
            AgentPolicyStage::PreAction,
        )),
        "node.finished" if is_tool_node => Some((
            "tool.result.received",
            AgentStepKind::ToolResult,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        "node.started" => Some((
            "step.started",
            AgentStepKind::Unknown,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        "node.finished" => Some((
            "step.completed",
            AgentStepKind::Unknown,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        "node.error" if is_tool_node => Some((
            "tool.call.failed",
            AgentStepKind::ToolCall,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        "node.error" => Some((
            "step.failed",
            AgentStepKind::ErrorRecovery,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        "waiting" => Some((
            "approval.requested",
            AgentStepKind::Approval,
            AgentEventPhase::Before,
            AgentPolicyStage::PreAction,
        )),
        _ => None,
    }
}

fn map_crewai_event_type(
    event_type: &str,
) -> Option<(
    &'static str,
    AgentStepKind,
    AgentEventPhase,
    AgentPolicyStage,
)> {
    match event_type {
        "CrewKickoffStartedEvent" => Some((
            "run.started",
            AgentStepKind::Unknown,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        "CrewKickoffCompletedEvent" => Some((
            "run.completed",
            AgentStepKind::Unknown,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        "CrewKickoffFailedEvent" => Some((
            "run.failed",
            AgentStepKind::ErrorRecovery,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        "TaskStartedEvent" => Some((
            "step.started",
            AgentStepKind::Unknown,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        "TaskCompletedEvent" => Some((
            "step.completed",
            AgentStepKind::Unknown,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        "ToolUsageStartedEvent" => Some((
            "tool.call.requested",
            AgentStepKind::ToolCall,
            AgentEventPhase::Before,
            AgentPolicyStage::PreAction,
        )),
        "ToolUsageFinishedEvent" => Some((
            "tool.result.received",
            AgentStepKind::ToolResult,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        "ToolUsageErrorEvent" => Some((
            "tool.call.failed",
            AgentStepKind::ToolCall,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        _ => None,
    }
}

fn map_mastra_event_type(
    event_type: &str,
) -> Option<(
    &'static str,
    AgentStepKind,
    AgentEventPhase,
    AgentPolicyStage,
)> {
    match event_type {
        "workflow.run.started" => Some((
            "run.started",
            AgentStepKind::Unknown,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        "workflow.run.completed" => Some((
            "run.completed",
            AgentStepKind::Unknown,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        "workflow.run.failed" => Some((
            "run.failed",
            AgentStepKind::ErrorRecovery,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        "workflow.step.started" => Some((
            "step.started",
            AgentStepKind::Unknown,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        "workflow.step.completed" => Some((
            "step.completed",
            AgentStepKind::Unknown,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        "tool.call.started" => Some((
            "tool.call.requested",
            AgentStepKind::ToolCall,
            AgentEventPhase::Before,
            AgentPolicyStage::PreAction,
        )),
        "tool.call.completed" => Some((
            "tool.result.received",
            AgentStepKind::ToolResult,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        "llm.call.started" => Some((
            "llm.call.started",
            AgentStepKind::LlmCall,
            AgentEventPhase::Before,
            AgentPolicyStage::PreAction,
        )),
        "llm.call.completed" => Some((
            "llm.call.completed",
            AgentStepKind::LlmCall,
            AgentEventPhase::After,
            AgentPolicyStage::AuditOnly,
        )),
        "checkpoint.created" => Some((
            "checkpoint.created",
            AgentStepKind::Checkpoint,
            AgentEventPhase::State,
            AgentPolicyStage::AuditOnly,
        )),
        _ => None,
    }
}

fn map_unknown_event_type(
    event_type: &str,
) -> Option<(&'static str, AgentStepKind)> {
    let compact = event_type.to_ascii_lowercase();
    if is_ambiguous_incomplete_event(&compact) {
        None
    } else if has_tool_token(&compact) {
        if is_generic_completion_event(&compact) {
            Some(("tool.call.completed", AgentStepKind::ToolCall))
        } else {
            Some(("tool.call.requested", AgentStepKind::ToolCall))
        }
    } else if has_llm_token(&compact) {
        if is_generic_completion_event(&compact) {
            Some(("llm.call.completed", AgentStepKind::LlmCall))
        } else {
            Some(("llm.call.started", AgentStepKind::LlmCall))
        }
    } else if has_approval_token(&compact) {
        Some(("approval.requested", AgentStepKind::Approval))
    } else if compact.contains("checkpoint") {
        Some(("checkpoint.created", AgentStepKind::Checkpoint))
    } else if !has_false_positive_tool_or_llm_token(&compact)
        && is_generic_completion_event(&compact)
    {
        Some(("step.completed", AgentStepKind::Unknown))
    } else {
        None
    }
}

fn is_ambiguous_incomplete_event(event_type: &str) -> bool {
    event_tokens(event_type).any(|token| token == "incomplete")
        || has_not_completion_tokens(event_type)
        || [
            ".complete",
            "_complete",
            "-complete",
            ":complete",
            "/complete",
        ]
        .iter()
        .any(|suffix| {
            event_type
                .strip_suffix(suffix)
                .and_then(|prefix| {
                    prefix.rsplit(['.', '_', '-', ':', '/']).next()
                })
                .is_some_and(|token| token == "not")
        })
}

fn has_not_completion_tokens(event_type: &str) -> bool {
    let mut previous = None;
    for token in event_tokens(event_type) {
        if previous == Some("not")
            && matches!(token, "completed" | "complete" | "end")
        {
            return true;
        }
        previous = Some(token);
    }
    false
}

fn has_tool_token(event_type: &str) -> bool {
    event_tokens(event_type).any(|token| matches!(token, "tool" | "tools"))
}

fn has_llm_token(event_type: &str) -> bool {
    let mut tokens = event_tokens(event_type).peekable();
    while let Some(token) = tokens.next() {
        if token == "llm" {
            return true;
        }
        if token == "chat" && tokens.peek().is_some_and(|next| *next == "model")
        {
            return true;
        }
    }
    false
}

fn has_approval_token(event_type: &str) -> bool {
    event_tokens(event_type).any(|token| token == "approval")
}

fn event_tokens(event_type: &str) -> impl Iterator<Item = &str> {
    event_type
        .split(|character: char| {
            matches!(character, '.' | '_' | '-' | ':' | '/')
                || character.is_whitespace()
        })
        .filter(|token| !token.is_empty())
}

fn has_false_positive_tool_or_llm_token(event_type: &str) -> bool {
    event_tokens(event_type).any(|token| {
        !matches!(token, "tool" | "tools" | "llm" | "chat" | "model")
            && (token.contains("tool") || token.contains("llm"))
    })
}

fn is_generic_completion_event(event_type: &str) -> bool {
    matches!(event_type, "completed" | "complete" | "end")
        || has_completion_suffix(event_type, ".completed")
        || has_completion_suffix(event_type, "_completed")
        || has_completion_suffix(event_type, "-completed")
        || has_completion_suffix(event_type, ":completed")
        || has_completion_suffix(event_type, "/completed")
        || has_completion_suffix(event_type, ".complete")
        || has_completion_suffix(event_type, "_complete")
        || has_completion_suffix(event_type, "-complete")
        || has_completion_suffix(event_type, ":complete")
        || has_completion_suffix(event_type, "/complete")
        || has_completion_suffix(event_type, ".end")
        || has_completion_suffix(event_type, "_end")
        || has_completion_suffix(event_type, "-end")
        || has_completion_suffix(event_type, ":end")
        || has_completion_suffix(event_type, "/end")
}

fn has_completion_suffix(event_type: &str, suffix: &str) -> bool {
    let Some(prefix) = event_type.strip_suffix(suffix) else {
        return false;
    };
    prefix
        .rsplit(['.', '_', '-', ':', '/'])
        .next()
        .is_none_or(|token| token != "not")
}

fn metadata_string(metadata: &serde_json::Value, key: &str) -> Option<String> {
    metadata
        .as_object()
        .and_then(|object| object.get(key))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn raw_string(event: &AgentEventInput, key: &str) -> Option<String> {
    event
        .raw_fields
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn fill_option(target: &mut Option<String>, value: Option<String>) {
    if target.is_none() {
        *target = value;
    }
}

fn metadata_insert_string(
    metadata: &mut serde_json::Value,
    key: &str,
    value: String,
) {
    if !metadata.is_object() {
        *metadata = serde_json::json!({ "value": metadata.take() });
    }
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    object
        .entry(key.to_string())
        .or_insert_with(|| serde_json::Value::String(value));
}

fn is_tool_node_type(node_type: &str) -> bool {
    let compact = node_type.to_ascii_lowercase();
    has_tool_token(&compact)
}

fn enrich_adapter_metadata(
    metadata: &mut serde_json::Value,
    source: AgentEventSource,
    raw_event_type: &str,
    name: Option<&str>,
) {
    if !metadata.is_object() {
        *metadata = serde_json::json!({ "value": metadata.take() });
    }
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    object.entry("framework").or_insert_with(|| {
        serde_json::Value::String(source.as_str().to_string())
    });
    object.entry("rawEventType").or_insert_with(|| {
        serde_json::Value::String(raw_event_type.to_string())
    });
    if let Some(name) = name {
        object
            .entry("rawName")
            .or_insert_with(|| serde_json::Value::String(name.to_string()));
    }
}

fn copy_allowlisted_raw_fields_to_metadata(event: &mut AgentEventInput) {
    const ALLOWLIST: &[&str] = &[
        "frameworkVersion",
        "rawEventType",
        "rawEventName",
        "traceId",
        "trace_id",
        "spanId",
        "span_id",
        "executionId",
        "execution_id",
        "workflowId",
        "workflow_id",
        "nodeId",
        "node_id",
        "taskId",
        "task_id",
        "toolName",
        "tool_name",
        "sequence",
    ];

    if !event.metadata.is_object() {
        event.metadata = serde_json::json!({ "value": event.metadata.take() });
    }
    let Some(object) = event.metadata.as_object_mut() else {
        return;
    };
    for key in ALLOWLIST {
        if let Some(value) = event.raw_fields.get(*key) {
            object
                .entry((*key).to_string())
                .or_insert_with(|| value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::agent::{
        context::{
            AgentConfidence, AgentEventPhase, AgentEventSourceTrust,
            AgentPolicyStage, AgentStepKind, AgentStepSource,
        },
        event::{AgentEventSource, AgentEventsRequest},
    };

    #[test]
    fn langgraph_tool_start_maps_to_standard_tool_call_event() {
        let raw = json!({
            "source": "langgraph",
            "events": [{
                "event": "on_tool_start",
                "agent_id": "agent-1",
                "run_id": "run-1",
                "step_id": "span-tool-1",
                "name": "mock_search",
                "metadata": {
                    "langgraph_node": "search_tool"
                },
                "data": {
                    "input": { "query": "hello" }
                }
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();

        assert_eq!(sourced.len(), 1);
        assert_eq!(sourced[0].source, AgentEventSource::LangGraph);

        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "tool.call.requested");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::ToolCall));
        assert_eq!(adapted.event_phase, AgentEventPhase::Before);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::PreAction);
        assert_eq!(adapted.step_source, AgentStepSource::Runtime);
        assert_eq!(adapted.step_confidence, AgentConfidence::High);
        assert_eq!(adapted.graph_node.as_deref(), Some("search_tool"));
        assert_eq!(adapted.source, Some(AgentEventSource::LangGraph));
        assert_eq!(adapted.metadata["framework"], "langgraph");
        assert_eq!(adapted.metadata["rawEventType"], "on_tool_start");
    }

    #[test]
    fn langgraph_llm_start_maps_to_pre_action_llm_call() {
        let raw = json!({
            "source": "langgraph",
            "events": [{
                "event": "on_llm_start",
                "agent_id": "agent-1",
                "run_id": "run-1",
                "step_id": "span-llm-1",
                "name": "planner",
                "metadata": {
                    "langgraph_node": "planner"
                }
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();

        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "llm.call.started");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::LlmCall));
        assert_eq!(adapted.event_phase, AgentEventPhase::Before);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::PreAction);
        assert_eq!(adapted.step_source, AgentStepSource::Runtime);
        assert_eq!(adapted.step_confidence, AgentConfidence::High);
        assert_eq!(adapted.graph_node.as_deref(), Some("planner"));
    }

    #[test]
    fn unknown_source_infers_tool_call_from_event_name() {
        let raw = json!({
            "framework": "unknown",
            "events": [{
                "type": "tool.start",
                "agent_id": "agent-1",
                "run_id": "run-1",
                "step_id": "step-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(sourced[0].source, AgentEventSource::Unknown);
        assert_eq!(adapted.event_type, "tool.call.requested");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::ToolCall));
        assert_eq!(adapted.step_source, AgentStepSource::Heuristic);
        assert_eq!(adapted.step_confidence, AgentConfidence::Medium);
    }

    #[test]
    fn omitted_source_is_treated_as_unknown_and_conservative() {
        let raw = json!({
            "events": [{
                "type": "tool.start",
                "event_source_trust": "registered",
                "step_kind": "planning",
                "step_source": "runtime",
                "step_confidence": "high",
                "agent_id": "agent-1",
                "run_id": "run-1",
                "step_id": "step-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(sourced[0].source, AgentEventSource::Unknown);
        assert_eq!(adapted.event_type, "tool.call.requested");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::ToolCall));
        assert_eq!(adapted.step_source, AgentStepSource::Heuristic);
        assert_eq!(adapted.step_confidence, AgentConfidence::Medium);
        assert_eq!(adapted.event_phase, AgentEventPhase::Before);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::PreAction);
        assert_eq!(
            adapted.event_source_trust,
            AgentEventSourceTrust::SelfReported
        );
    }

    #[test]
    fn unknown_recognized_event_overwrites_conflicting_step_taxonomy() {
        let raw = json!({
            "framework": "unknown",
            "events": [{
                "type": "tool.start",
                "step_kind": "planning",
                "event_phase": "state",
                "policy_stage": "audit_only",
                "agent_id": "agent-1",
                "run_id": "run-1",
                "step_id": "step-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "tool.call.requested");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::ToolCall));
        assert_eq!(adapted.event_phase, AgentEventPhase::Before);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::PreAction);
    }

    #[test]
    fn unknown_source_downgrades_client_registered_source_trust() {
        let raw = json!({
            "framework": "unknown",
            "events": [{
                "type": "tool.start",
                "event_source_trust": "registered",
                "agent_id": "agent-1",
                "run_id": "run-1",
                "step_id": "step-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "tool.call.requested");
        assert_eq!(
            adapted.event_source_trust,
            AgentEventSourceTrust::SelfReported
        );
    }

    #[test]
    fn unknown_unrecognized_event_is_audit_only_low_confidence() {
        let raw = json!({
            "framework": "unknown",
            "events": [{
                "type": "custom.native.event",
                "agent_id": "agent-1",
                "run_id": "run-1",
                "step_id": "step-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "unknown");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::Unknown));
        assert_eq!(adapted.step_source, AgentStepSource::Heuristic);
        assert_eq!(adapted.step_confidence, AgentConfidence::Low);
        assert_eq!(adapted.event_phase, AgentEventPhase::Unknown);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
    }

    #[test]
    fn unknown_unrecognized_event_overwrites_payload_step_kind() {
        let raw = json!({
            "framework": "unknown",
            "events": [{
                "type": "custom.native.event",
                "step_kind": "tool_call",
                "agent_id": "agent-1",
                "run_id": "run-1",
                "step_id": "step-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "unknown");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::Unknown));
        assert_eq!(adapted.step_confidence, AgentConfidence::Low);
    }

    #[test]
    fn unknown_completed_event_maps_to_step_completed_audit_only() {
        let raw = json!({
            "framework": "unknown",
            "events": [{
                "type": "task.completed",
                "agent_id": "agent-1",
                "run_id": "run-1",
                "step_id": "step-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "step.completed");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::Unknown));
        assert_eq!(adapted.step_source, AgentStepSource::Heuristic);
        assert_eq!(adapted.step_confidence, AgentConfidence::Medium);
        assert_eq!(adapted.event_phase, AgentEventPhase::After);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
    }

    #[test]
    fn unknown_tool_colon_and_slash_end_markers_map_to_completed() {
        for raw_event_type in ["tool:end", "tool/end"] {
            let raw = json!({
                "framework": "unknown",
                "events": [{
                    "type": raw_event_type,
                    "agent_id": "agent-1",
                    "run_id": "run-1",
                    "step_id": "step-1"
                }]
            });
            let request: AgentEventsRequest =
                serde_json::from_value(raw).unwrap();
            let sourced = request.into_sourced_events();
            let adapted = super::adapt_agent_event(
                sourced[0].source,
                sourced[0].event.clone(),
            );

            assert_eq!(adapted.event_type, "tool.call.completed");
            assert_eq!(adapted.step_kind, Some(AgentStepKind::ToolCall));
            assert_eq!(adapted.event_phase, AgentEventPhase::After);
            assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
        }
    }

    #[test]
    fn unknown_completed_event_overwrites_client_source_and_confidence() {
        let raw = json!({
            "framework": "unknown",
            "events": [{
                "type": "task.completed",
                "step_source": "runtime",
                "step_confidence": "high",
                "agent_id": "agent-1",
                "run_id": "run-1",
                "step_id": "step-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "step.completed");
        assert_eq!(adapted.step_source, AgentStepSource::Heuristic);
        assert_eq!(adapted.step_confidence, AgentConfidence::Medium);
    }

    #[test]
    fn unknown_incomplete_markers_remain_conservative_unknowns() {
        for raw_event_type in ["run.incomplete", "task.not_complete"] {
            let raw = json!({
                "framework": "unknown",
                "events": [{
                    "type": raw_event_type,
                    "agent_id": "agent-1",
                    "run_id": "run-1",
                    "step_id": "step-1"
                }]
            });
            let request: AgentEventsRequest =
                serde_json::from_value(raw).unwrap();
            let sourced = request.into_sourced_events();
            let adapted = super::adapt_agent_event(
                sourced[0].source,
                sourced[0].event.clone(),
            );

            assert_eq!(adapted.event_type, "unknown");
            assert_eq!(adapted.step_confidence, AgentConfidence::Low);
            assert_eq!(adapted.event_phase, AgentEventPhase::Unknown);
            assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
        }
    }

    #[test]
    fn unknown_tool_incomplete_markers_remain_conservative_unknowns() {
        for raw_event_type in [
            "tool.incomplete",
            "tool.not_complete",
            "tool.not.completed",
            "tool.not-completed",
        ] {
            let raw = json!({
                "framework": "unknown",
                "events": [{
                    "type": raw_event_type,
                    "agent_id": "agent-1",
                    "run_id": "run-1",
                    "step_id": "step-1"
                }]
            });
            let request: AgentEventsRequest =
                serde_json::from_value(raw).unwrap();
            let sourced = request.into_sourced_events();
            let adapted = super::adapt_agent_event(
                sourced[0].source,
                sourced[0].event.clone(),
            );

            assert_eq!(adapted.event_type, "unknown");
            assert_eq!(adapted.step_kind, Some(AgentStepKind::Unknown));
            assert_eq!(adapted.step_confidence, AgentConfidence::Low);
            assert_eq!(adapted.event_phase, AgentEventPhase::Unknown);
            assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
        }
    }

    #[test]
    fn unknown_llm_incomplete_markers_remain_conservative_unknowns() {
        for raw_event_type in [
            "llm.incomplete",
            "llm.not_complete",
            "llm.not.completed",
            "llm.not-completed",
        ] {
            let raw = json!({
                "framework": "unknown",
                "events": [{
                    "type": raw_event_type,
                    "agent_id": "agent-1",
                    "run_id": "run-1",
                    "step_id": "step-1"
                }]
            });
            let request: AgentEventsRequest =
                serde_json::from_value(raw).unwrap();
            let sourced = request.into_sourced_events();
            let adapted = super::adapt_agent_event(
                sourced[0].source,
                sourced[0].event.clone(),
            );

            assert_eq!(adapted.event_type, "unknown");
            assert_eq!(adapted.step_kind, Some(AgentStepKind::Unknown));
            assert_eq!(adapted.step_confidence, AgentConfidence::Low);
            assert_eq!(adapted.event_phase, AgentEventPhase::Unknown);
            assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
        }
    }

    #[test]
    fn unknown_tool_and_llm_substrings_remain_conservative_unknowns() {
        for raw_event_type in
            ["tooling.started", "workflow.tooling.end", "allm.started"]
        {
            let raw = json!({
                "framework": "unknown",
                "events": [{
                    "type": raw_event_type,
                    "agent_id": "agent-1",
                    "run_id": "run-1",
                    "step_id": "step-1"
                }]
            });
            let request: AgentEventsRequest =
                serde_json::from_value(raw).unwrap();
            let sourced = request.into_sourced_events();
            let adapted = super::adapt_agent_event(
                sourced[0].source,
                sourced[0].event.clone(),
            );

            assert_eq!(adapted.event_type, "unknown");
            assert_eq!(adapted.step_confidence, AgentConfidence::Low);
            assert_eq!(adapted.event_phase, AgentEventPhase::Unknown);
            assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
        }
    }

    #[test]
    fn unknown_approval_substrings_remain_conservative_unknowns() {
        let raw = json!({
            "framework": "unknown",
            "events": [{
                "type": "preapproval.started",
                "agent_id": "agent-1",
                "run_id": "run-1",
                "step_id": "step-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "unknown");
        assert_eq!(adapted.step_confidence, AgentConfidence::Low);
        assert_eq!(adapted.event_phase, AgentEventPhase::Unknown);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
    }

    #[test]
    fn openai_agents_tool_called_maps_to_pre_action_tool_call() {
        let raw = json!({
            "source": "openai_agents",
            "events": [{
                "event": "tool_called",
                "name": "web_search",
                "trace_id": "trace-1",
                "span_id": "item-tool-1",
                "parent_id": "span-parent"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "tool.call.requested");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::ToolCall));
        assert_eq!(adapted.event_phase, AgentEventPhase::Before);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::PreAction);
        assert_eq!(adapted.run_id.as_deref(), Some("trace-1"));
        assert_eq!(adapted.step_id.as_deref(), Some("item-tool-1"));
        assert_eq!(adapted.parent_step_id.as_deref(), Some("span-parent"));
        assert_eq!(adapted.metadata["tool_name"], "web_search");
    }

    #[test]
    fn openai_agents_tool_output_maps_to_tool_result_audit() {
        let raw = json!({
            "source": "openai_agents",
            "events": [{
                "event": "tool_output",
                "trace_id": "trace-1",
                "span_id": "item-tool-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "tool.result.received");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::ToolResult));
        assert_eq!(adapted.event_phase, AgentEventPhase::After);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
    }

    #[test]
    fn openai_agents_handoff_name_does_not_set_tool_name_metadata() {
        let raw = json!({
            "source": "openai_agents",
            "events": [{
                "event": "handoff_requested",
                "name": "handoff_to_billing",
                "trace_id": "trace-1",
                "span_id": "span-handoff-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "handoff.requested");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::Handoff));
        assert_eq!(adapted.event_phase, AgentEventPhase::Before);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::PreAction);
        assert!(adapted.metadata.get("tool_name").is_none());
    }

    #[test]
    fn n8n_node_started_maps_workflow_execution_to_step() {
        let raw = json!({
            "source": "n8n",
            "events": [{
                "event": "node.started",
                "workflowId": "wf-1",
                "executionId": "exec-1",
                "nodeId": "node-1",
                "nodeName": "Fetch ticket",
                "nodeType": "tool"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "tool.call.requested");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::ToolCall));
        assert_eq!(adapted.event_phase, AgentEventPhase::Before);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::PreAction);
        assert_eq!(adapted.agent_id_external.as_deref(), Some("wf-1"));
        assert_eq!(adapted.run_id.as_deref(), Some("exec-1"));
        assert_eq!(adapted.step_id.as_deref(), Some("node-1"));
        assert_eq!(adapted.graph_node.as_deref(), Some("Fetch ticket"));
    }

    #[test]
    fn n8n_non_tool_node_started_maps_to_step_started_audit() {
        let raw = json!({
            "source": "n8n",
            "events": [{
                "event": "node.started",
                "workflowId": "wf-1",
                "executionId": "exec-1",
                "nodeId": "node-1",
                "nodeName": "Normalize ticket",
                "nodeType": "code"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "step.started");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::Unknown));
        assert_eq!(adapted.event_phase, AgentEventPhase::State);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
    }

    #[test]
    fn n8n_non_tool_node_finished_maps_to_step_completed_audit() {
        let raw = json!({
            "source": "n8n",
            "events": [{
                "event": "node.finished",
                "workflowId": "wf-1",
                "executionId": "exec-1",
                "nodeId": "node-1",
                "nodeName": "Normalize ticket",
                "nodeType": "code"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "step.completed");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::Unknown));
        assert_eq!(adapted.event_phase, AgentEventPhase::After);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
    }

    #[test]
    fn n8n_tooling_node_started_maps_to_normal_step_not_tool() {
        let raw = json!({
            "source": "n8n",
            "events": [{
                "event": "node.started",
                "workflowId": "wf-1",
                "executionId": "exec-1",
                "nodeId": "node-1",
                "nodeName": "Prepare tooling context",
                "nodeType": "tooling"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "step.started");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::Unknown));
        assert_eq!(adapted.event_phase, AgentEventPhase::State);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
    }

    #[test]
    fn n8n_toolbox_node_finished_maps_to_normal_step_not_tool() {
        let raw = json!({
            "source": "n8n",
            "events": [{
                "event": "node.finished",
                "workflowId": "wf-1",
                "executionId": "exec-1",
                "nodeId": "node-1",
                "nodeName": "Archive toolbox state",
                "nodeType": "toolbox"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "step.completed");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::Unknown));
        assert_eq!(adapted.event_phase, AgentEventPhase::After);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
    }

    #[test]
    fn n8n_execution_success_maps_to_run_completed() {
        let raw = json!({
            "source": "n8n",
            "events": [{
                "event": "execution.success",
                "workflowId": "wf-1",
                "executionId": "exec-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "run.completed");
        assert_eq!(adapted.event_phase, AgentEventPhase::State);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
    }

    #[test]
    fn n8n_execution_error_maps_to_run_failed_state_audit() {
        let raw = json!({
            "source": "n8n",
            "events": [{
                "event": "execution.error",
                "workflowId": "wf-1",
                "executionId": "exec-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "run.failed");
        assert_eq!(adapted.event_phase, AgentEventPhase::State);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
    }

    #[test]
    fn crewai_tool_usage_started_maps_to_pre_action_tool_call() {
        let raw = json!({
            "source": "crewai",
            "events": [{
                "event": "ToolUsageStartedEvent",
                "crew_id": "crew-1",
                "task_id": "task-1",
                "tool_name": "lookup_ticket"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "tool.call.requested");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::ToolCall));
        assert_eq!(adapted.event_phase, AgentEventPhase::Before);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::PreAction);
        assert_eq!(adapted.run_id.as_deref(), Some("crew-1"));
        assert_eq!(adapted.step_id.as_deref(), Some("task-1"));
        assert_eq!(adapted.metadata["tool_name"], "lookup_ticket");
    }

    #[test]
    fn crewai_kickoff_failed_maps_to_run_failed_state_audit() {
        let raw = json!({
            "source": "crewai",
            "events": [{
                "event": "CrewKickoffFailedEvent",
                "crew_id": "crew-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "run.failed");
        assert_eq!(adapted.event_phase, AgentEventPhase::State);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
    }

    #[test]
    fn mastra_tool_call_span_maps_to_pre_action_tool_call() {
        let raw = json!({
            "source": "mastra",
            "events": [{
                "event": "tool.call.started",
                "traceId": "trace-1",
                "spanId": "span-tool-1",
                "toolName": "lookup_ticket"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "tool.call.requested");
        assert_eq!(adapted.step_kind, Some(AgentStepKind::ToolCall));
        assert_eq!(adapted.event_phase, AgentEventPhase::Before);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::PreAction);
        assert_eq!(adapted.run_id.as_deref(), Some("trace-1"));
        assert_eq!(adapted.step_id.as_deref(), Some("span-tool-1"));
        assert_eq!(adapted.metadata["tool_name"], "lookup_ticket");
    }

    #[test]
    fn mastra_workflow_run_failed_maps_to_run_failed_state_audit() {
        let raw = json!({
            "source": "mastra",
            "events": [{
                "event": "workflow.run.failed",
                "traceId": "trace-1"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.event_type, "run.failed");
        assert_eq!(adapted.event_phase, AgentEventPhase::State);
        assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
    }

    #[test]
    fn framework_adapters_unknown_native_events_fall_back_conservatively() {
        for source in [
            AgentEventSource::OpenAiAgents,
            AgentEventSource::N8n,
            AgentEventSource::CrewAi,
            AgentEventSource::Mastra,
        ] {
            let raw_event_type = "native.unrecognized.event";
            let raw = json!({
                "source": source.as_str(),
                "events": [{
                    "event": raw_event_type,
                    "name": "native_name"
                }]
            });
            let request: AgentEventsRequest =
                serde_json::from_value(raw).unwrap();
            let sourced = request.into_sourced_events();
            let adapted = super::adapt_agent_event(
                sourced[0].source,
                sourced[0].event.clone(),
            );

            assert_eq!(adapted.event_type, "unknown");
            assert_eq!(adapted.step_kind, Some(AgentStepKind::Unknown));
            assert_eq!(adapted.step_confidence, AgentConfidence::Low);
            assert_eq!(adapted.policy_stage, AgentPolicyStage::AuditOnly);
            assert_eq!(adapted.metadata["framework"], source.as_str());
            assert_eq!(adapted.metadata["rawEventType"], raw_event_type);
        }
    }

    #[test]
    fn adapter_preserves_allowlisted_raw_fields_in_metadata() {
        let raw = json!({
            "source": "n8n",
            "events": [{
                "event": "node.started",
                "executionId": "exec-1",
                "workflowId": "workflow-1",
                "nodeId": "node-1",
                "authorization": "Bearer secret",
                "input": { "secret": "value" }
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.metadata["executionId"], "exec-1");
        assert_eq!(adapted.metadata["workflowId"], "workflow-1");
        assert_eq!(adapted.metadata["nodeId"], "node-1");
        assert!(adapted.metadata.get("authorization").is_none());
        assert!(adapted.metadata.get("input").is_none());
    }

    #[test]
    fn allowlisted_raw_fields_do_not_overwrite_existing_metadata() {
        let raw = json!({
            "source": "n8n",
            "events": [{
                "event": "node.started",
                "metadata": {
                    "executionId": "metadata-exec"
                },
                "executionId": "raw-exec",
                "rawEventType": "raw.override"
            }]
        });
        let request: AgentEventsRequest = serde_json::from_value(raw).unwrap();
        let sourced = request.into_sourced_events();
        let adapted = super::adapt_agent_event(
            sourced[0].source,
            sourced[0].event.clone(),
        );

        assert_eq!(adapted.metadata["executionId"], "metadata-exec");
        assert_eq!(adapted.metadata["rawEventType"], "node.started");
    }
}
