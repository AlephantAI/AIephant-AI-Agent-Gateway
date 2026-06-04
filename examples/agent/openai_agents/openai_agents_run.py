"""OpenAI Agents SDK-style event example for Alephant Agent Gateway."""

from __future__ import annotations

import os
import sys
import asyncio
from pathlib import Path
from typing import Any


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from framework_common import (  # noqa: E402
    AgentEventClient,
    base_event,
    default_agent_name,
    default_run_id,
    load_dotenv,
    sleep_briefly,
)


SOURCE = "openai_agents"


def gateway_chat_base_url() -> str:
    load_dotenv()
    base_url = (
        os.getenv("GATEWAY_BASE")
        or os.getenv("ALEPHANT_GATEWAY_URL")
        or "http://127.0.0.1:8080"
    ).rstrip("/")
    if base_url.endswith("/v1"):
        return base_url
    return f"{base_url}/v1"


def lookup_support_policy(query: str) -> str:
    """Look up a support policy note for the given query."""
    normalized = query.lower()
    if "refund" in normalized:
        return "refund policy: verify account standing, then process standard refunds."
    if "escalation" in normalized:
        return "escalation policy: gather context, then route to a specialist."
    return "support policy: collect context, check account status, then summarize next action."


def build_gateway_model():
    load_dotenv()
    from agents import OpenAIChatCompletionsModel
    from openai import AsyncOpenAI

    client = AsyncOpenAI(
        api_key=os.getenv("ALEPHANT_API_KEY")
        or os.getenv("ALEPHANT_CONTROL_OPENROUTER_API_KEY"),
        base_url=gateway_chat_base_url(),
    )
    return OpenAIChatCompletionsModel(
        model=os.getenv("ALEPHANT_MODEL", "openai/gpt-4o-mini"),
        openai_client=client,
    )


def build_agent():
    from agents import Agent, function_tool

    return Agent(
        name="OpenAI Agents Support Planner",
        instructions=(
            "Think briefly, make a concise plan, call lookup_support_policy, "
            "then summarize the next action for a support operator."
        ),
        tools=[function_tool(lookup_support_policy)],
        model=build_gateway_model(),
    )


def run_support_planning_preview(*, query: str) -> dict[str, Any]:
    thinking = f"Reason about the support request before answering: {query}."
    plan_steps = [
        "Identify the support intent.",
        "Call lookup_support_policy for grounded policy context.",
        "Ask the model to produce the final operator-facing answer.",
    ]
    tool_result = lookup_support_policy(query)
    llm_prompt = (
        f"Using this policy context: {tool_result} "
        f"Answer the support request: {query}"
    )
    return {
        "query": query,
        "thinking": thinking,
        "plan_steps": plan_steps,
        "tool_name": "lookup_support_policy",
        "tool_result": tool_result,
        "llm_prompt": llm_prompt,
    }


async def run_agent(*, query: str) -> str:
    from agents import Runner

    result = await Runner.run(build_agent(), query)
    return str(result.final_output)


def build_events(
    *,
    agent_id: str,
    run_id: str,
    agent_name: str | None = None,
    query: str = "agent gateway policy preflight",
    preview: dict[str, Any] | None = None,
) -> list[dict[str, Any]]:
    trace_id = f"trace_{run_id}"
    preview = preview or run_support_planning_preview(query=query)
    return [
        base_event(
            "agent_thinking",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            trace_id=trace_id,
            span_id="span_thinking",
            metadata={"thinking": preview["thinking"]},
        ),
        base_event(
            "plan_created",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            trace_id=trace_id,
            span_id="span_plan",
            parent_id="span_thinking",
            plan_steps=preview["plan_steps"],
            metadata={"tool_name": preview["tool_name"]},
        ),
        base_event(
            "tool_called",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            trace_id=trace_id,
            span_id="span_tool_call",
            parent_id="span_plan",
            item_id="item_lookup_policy",
            name=preview["tool_name"],
            metadata={"query": query},
        ),
        base_event(
            "tool_output",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            trace_id=trace_id,
            span_id="span_tool_output",
            parent_id="span_tool_call",
            item_id="item_lookup_policy_output",
            name=preview["tool_name"],
            metadata={"result_preview": preview["tool_result"]},
        ),
        base_event(
            "llm_request_started",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            trace_id=trace_id,
            span_id="span_llm",
            parent_id="span_tool_output",
            item_id="item_final_answer",
            name=os.getenv("ALEPHANT_MODEL", "openai/gpt-4o-mini"),
            metadata={"prompt": preview["llm_prompt"]},
        ),
        base_event(
            "handoff_requested",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            trace_id=trace_id,
            span_id="span_handoff",
            parent_id="span_llm",
            item_id="item_handoff",
            name="specialist_agent",
            metadata={"handoff_reason": "needs specialist review"},
        ),
    ]


def main() -> None:
    agent_id = os.getenv("ALEPHANT_AGENT_ID", "openai-agents-demo")
    run_id = default_run_id("run_openai_agents")
    agent_name = default_agent_name("OpenAI Agents Demo Agent")
    client = AgentEventClient.from_env()
    query = os.getenv("ALEPHANT_AGENT_QUERY", "refund escalation")
    preview = run_support_planning_preview(query=query)
    events = build_events(
        agent_id=agent_id,
        run_id=run_id,
        agent_name=agent_name,
        query=query,
        preview=preview,
    )
    response = client.emit_events(source=SOURCE, events=events)
    agent_result = None
    if not client.dry_run:
        agent_result = asyncio.run(run_agent(query=query))
    sleep_briefly()
    print(
        {
            "source": SOURCE,
            "run_id": run_id,
            "accepted": response.get("accepted"),
            "agent": "OpenAI Agents Support Planner",
            "agent_result": agent_result,
        }
    )


if __name__ == "__main__":
    main()
