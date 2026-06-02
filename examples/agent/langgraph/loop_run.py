"""LangGraph loop-warning run through Alephant Agent Gateway."""

from __future__ import annotations

import hashlib
from typing import TypedDict

from alephant_adapter import AlephantAgentAdapter, AgentStepContext, sleep_briefly


class LoopState(TypedDict, total=False):
    query: str
    attempts: int
    state_hash: str


def stable_state_hash(query: str) -> str:
    digest = hashlib.sha256(query.encode("utf-8")).hexdigest()
    return f"sha256:{digest}"


def build_graph(adapter: AlephantAgentAdapter):
    try:
        from langgraph.graph import END, StateGraph
    except ImportError as exc:
        raise SystemExit("Install LangGraph first: pip install langgraph") from exc

    def repeated_tool(state: LoopState) -> LoopState:
        attempts = int(state.get("attempts", 0)) + 1
        state_hash = state.get("state_hash") or stable_state_hash(state["query"])
        context = AgentStepContext(
            step_id=f"step_repeat_{attempts}",
            step_kind="tool_call",
            graph_node="repeat_search",
            attempt=attempts,
            input_hash=state_hash,
        )
        adapter.emit(
            "step.started",
            context=context,
            event_phase="state",
            policy_stage="audit_only",
        )
        preflight_event = adapter.build_event(
            "tool.call.requested",
            context=context,
            event_phase="before",
            policy_stage="pre_action",
            metadata={
                "tool_name": "mock_search",
                "normalized_args": {"query": state["query"]},
            },
        )
        preflight_response = adapter.emit_batch([preflight_event])
        if not AlephantAgentAdapter.response_allows_event(
            preflight_response,
            preflight_event["event_id"],
        ):
            raise RuntimeError("tool call blocked by Alephant policy")
        adapter.emit(
            "tool.call.completed",
            context=context,
            event_phase="after",
            policy_stage="audit_only",
            metadata={"result": "same result"},
        )
        return {"attempts": attempts, "state_hash": state_hash}

    def should_continue(state: LoopState) -> str:
        return "repeat" if int(state.get("attempts", 0)) < 3 else "done"

    graph = StateGraph(LoopState)
    graph.add_node("repeat", repeated_tool)
    graph.set_entry_point("repeat")
    graph.add_conditional_edges("repeat", should_continue, {"repeat": "repeat", "done": END})
    return graph.compile()


def main() -> None:
    adapter = AlephantAgentAdapter.from_env(
        agent_id="langgraph-loop-agent",
        default_agent_name="LangGraph Loop Demo Agent",
    )
    graph = build_graph(adapter)
    adapter.emit(
        "run.started",
        event_phase="state",
        policy_stage="audit_only",
        metadata={"framework": "langgraph", "example": "loop_run"},
    )
    result = graph.invoke({"query": "repeat this search", "attempts": 0})
    adapter.emit(
        "loop.warning",
        event_phase="state",
        policy_stage="audit_only",
        metadata={
            "loop_type": "repeated_tool_call",
            "evidence": {
                "graph_node": "repeat_search",
                "attempts": result.get("attempts"),
                "state_hash": result.get("state_hash"),
            },
        },
    )
    sleep_briefly()
    adapter.emit(
        "run.completed",
        event_phase="after",
        policy_stage="audit_only",
        metadata={"result": result},
    )
    print(result)


if __name__ == "__main__":
    main()
