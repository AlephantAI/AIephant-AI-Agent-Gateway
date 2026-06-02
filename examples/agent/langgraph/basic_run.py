"""LangGraph basic run through Alephant Agent Gateway."""

from __future__ import annotations

from typing import TypedDict

from alephant_adapter import AlephantAgentAdapter, AgentStepContext, sleep_briefly


class BasicState(TypedDict, total=False):
    prompt: str
    answer: str


def build_graph(adapter: AlephantAgentAdapter):
    try:
        from langgraph.graph import END, StateGraph
    except ImportError as exc:
        raise SystemExit("Install LangGraph first: pip install langgraph") from exc

    def planner(state: BasicState) -> BasicState:
        context = AgentStepContext(
            step_id="step_planner",
            step_kind="planning",
            graph_node="planner",
        )
        adapter.emit(
            "step.started",
            context=context,
            event_phase="state",
            policy_stage="audit_only",
        )
        answer = adapter.chat_completion(
            messages=[
                {"role": "system", "content": "You are a concise planning agent."},
                {"role": "user", "content": state["prompt"]},
            ],
            context=context,
        )
        adapter.emit(
            "step.completed",
            context=context,
            event_phase="after",
            policy_stage="audit_only",
            metadata={"answer_preview": answer[:120]},
        )
        return {"answer": answer}

    graph = StateGraph(BasicState)
    graph.add_node("planner", planner)
    graph.set_entry_point("planner")
    graph.add_edge("planner", END)
    return graph.compile()


def main() -> None:
    adapter = AlephantAgentAdapter.from_env(
        agent_id="langgraph-basic-agent",
        default_agent_name="LangGraph Basic Demo Agent",
    )
    graph = build_graph(adapter)
    adapter.emit(
        "run.started",
        event_phase="state",
        policy_stage="audit_only",
        metadata={"framework": "langgraph", "example": "basic_run"},
    )
    result = graph.invoke({"prompt": "Create a one sentence plan for testing an agent gateway."})
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
