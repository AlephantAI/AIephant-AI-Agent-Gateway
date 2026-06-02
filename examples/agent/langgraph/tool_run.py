"""LangGraph tool-call run through Alephant Agent Gateway."""

from __future__ import annotations

import uuid
from typing import TypedDict

from alephant_adapter import AlephantAgentAdapter, AgentStepContext, sleep_briefly


class ToolState(TypedDict, total=False):
    query: str
    plan: str
    tool_result: str
    answer: str
    tool_call_id: str


def mock_search(query: str) -> str:
    return f"mock search result for: {query}"


def build_graph(adapter: AlephantAgentAdapter):
    try:
        from langgraph.graph import END, StateGraph
    except ImportError as exc:
        raise SystemExit("Install LangGraph first: pip install langgraph") from exc

    def planner(state: ToolState) -> ToolState:
        context = AgentStepContext(
            step_id="step_plan_tool",
            step_kind="llm_call",
            graph_node="planner",
        )
        adapter.emit(
            "step.started",
            context=context,
            event_phase="state",
            policy_stage="audit_only",
        )
        plan = adapter.chat_completion(
            messages=[
                {"role": "system", "content": "Decide the next tool call."},
                {"role": "user", "content": state["query"]},
            ],
            context=context,
        )
        tool_call_id = f"call_{uuid.uuid4().hex}"
        preflight_event = adapter.build_event(
            "tool.call.requested",
            context=AgentStepContext(
                step_id="step_plan_tool",
                step_kind="tool_call",
                graph_node="planner",
                tool_call_id=tool_call_id,
            ),
            event_phase="before",
            policy_stage="pre_action",
            metadata={"tool_name": "mock_search", "query": state["query"]},
        )
        preflight_response = adapter.emit_batch([preflight_event])
        if not AlephantAgentAdapter.response_allows_event(
            preflight_response,
            preflight_event["event_id"],
        ):
            raise RuntimeError("tool call blocked by Alephant policy")
        return {"plan": plan, "tool_call_id": tool_call_id}

    def tool_node(state: ToolState) -> ToolState:
        context = AgentStepContext(
            step_id="step_search_tool",
            step_kind="tool_call",
            graph_node="search_tool",
            parent_step_id="step_plan_tool",
            tool_call_id=state["tool_call_id"],
        )
        adapter.emit(
            "step.started",
            context=context,
            event_phase="state",
            policy_stage="audit_only",
        )
        result = mock_search(state["query"])
        adapter.emit(
            "tool.call.completed",
            context=context,
            event_phase="after",
            policy_stage="audit_only",
            metadata={"tool_name": "mock_search", "result_preview": result},
        )
        adapter.emit(
            "step.completed",
            context=context,
            event_phase="after",
            policy_stage="audit_only",
        )
        return {"tool_result": result}

    def final_answer(state: ToolState) -> ToolState:
        context = AgentStepContext(
            step_id="step_final",
            step_kind="final_answer",
            graph_node="final_answer",
            parent_step_id="step_search_tool",
        )
        answer = f"Final answer based on tool result: {state['tool_result']}"
        adapter.emit(
            "step.completed",
            context=context,
            event_phase="after",
            policy_stage="audit_only",
            metadata={"answer": answer},
        )
        return {"answer": answer}

    graph = StateGraph(ToolState)
    graph.add_node("planner", planner)
    graph.add_node("tool", tool_node)
    graph.add_node("final", final_answer)
    graph.set_entry_point("planner")
    graph.add_edge("planner", "tool")
    graph.add_edge("tool", "final")
    graph.add_edge("final", END)
    return graph.compile()


def main() -> None:
    adapter = AlephantAgentAdapter.from_env(
        agent_id="langgraph-tool-agent",
        default_agent_name="LangGraph Tool Demo Agent",
    )
    graph = build_graph(adapter)
    adapter.emit(
        "run.started",
        event_phase="state",
        policy_stage="audit_only",
        metadata={"framework": "langgraph", "example": "tool_run"},
    )
    result = graph.invoke({"query": "Find the safest way to test an agent tool call."})
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
