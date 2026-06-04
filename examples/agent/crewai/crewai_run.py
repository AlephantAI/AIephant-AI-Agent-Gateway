"""CrewAI event example for Alephant Agent Gateway."""

from __future__ import annotations

import os
import sys
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


SOURCE = "crewai"


def _disable_crewai_noise_for_examples() -> None:
    os.environ.setdefault("CREWAI_DISABLE_TELEMETRY", "true")
    os.environ.setdefault("CREWAI_DISABLE_TRACKING", "true")
    os.environ.setdefault("OTEL_SDK_DISABLED", "true")


def _import_crewai():
    _disable_crewai_noise_for_examples()
    from crewai import Agent, Crew, LLM, Process, Task
    from crewai.tools import BaseTool

    return Agent, Crew, LLM, Process, Task, BaseTool


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


def build_gateway_llm():
    _, _, LLM, _, _, _ = _import_crewai()
    load_dotenv()
    return LLM(
        model=os.getenv("ALEPHANT_MODEL", "openai/gpt-4o-mini"),
        api_key=os.getenv("ALEPHANT_API_KEY")
        or os.getenv("ALEPHANT_CONTROL_OPENROUTER_API_KEY"),
        base_url=gateway_chat_base_url(),
        provider="openai",
        timeout=float(os.getenv("ALEPHANT_CREWAI_LLM_TIMEOUT_SECONDS", "60")),
    )


def lookup_customer(customer_id: str) -> str:
    return (
        f"{customer_id}: customer is in good standing; "
        "refund eligibility is standard."
    )


def run_support_triage_preview(*, customer_id: str = "cus_123") -> dict[str, Any]:
    thinking = (
        f"Review customer {customer_id} before recommending a support action."
    )
    plan_steps = [
        "Check account health with crm.lookup_customer.",
        "Use the account signal to decide whether normal support can proceed.",
        "Summarize the next action for the support operator.",
    ]
    tool_result = lookup_customer(customer_id)
    return {
        "customer_id": customer_id,
        "thinking": thinking,
        "plan_steps": plan_steps,
        "tool_name": "crm.lookup_customer",
        "tool_result": tool_result,
    }


def build_customer_lookup_tool():
    _, _, _, _, _, BaseTool = _import_crewai()

    class CustomerLookupTool(BaseTool):
        name: str = "crm.lookup_customer"
        description: str = (
            "Look up a support customer's account health by customer id."
        )

        def _run(self, customer_id: str = "cus_123") -> str:
            return lookup_customer(customer_id)

    return CustomerLookupTool()


def build_crew(*, customer_id: str = "cus_123"):
    Agent, Crew, _, Process, Task, _ = _import_crewai()
    lookup_tool = build_customer_lookup_tool()
    llm = build_gateway_llm()

    support_researcher = Agent(
        role="Support account researcher",
        goal=(
            "Find the customer's account status and produce a concise support "
            "triage note."
        ),
        backstory=(
            "You are a careful support operations agent that checks account "
            "health before recommending next actions."
        ),
        tools=[lookup_tool],
        llm=llm,
        allow_delegation=False,
        verbose=False,
    )
    account_task = Task(
        description=(
            "Use crm.lookup_customer to inspect customer {customer_id}, then "
            "summarize whether support can proceed normally."
        ),
        expected_output=(
            "A short support triage note with account status and next action."
        ),
        agent=support_researcher,
        tools=[lookup_tool],
    )

    return Crew(
        agents=[support_researcher],
        tasks=[account_task],
        process=Process.sequential,
        verbose=False,
        memory=False,
        cache=False,
        name="Alephant CrewAI Support Demo",
        checkpoint_inputs={"customer_id": customer_id},
    )


def build_events(
    *,
    agent_id: str,
    run_id: str,
    agent_name: str | None = None,
    customer_id: str = "cus_123",
    preview: dict[str, Any] | None = None,
) -> list[dict[str, Any]]:
    task_id = "task_research_customer"
    preview = preview or run_support_triage_preview(customer_id=customer_id)
    return [
        base_event(
            "CrewKickoffStartedEvent",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            crew_id=run_id,
            metadata={"crew_name": "Support crew"},
        ),
        base_event(
            "AgentThinkingStartedEvent",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            crew_id=run_id,
            task_id=task_id,
            metadata={"thinking": preview["thinking"]},
        ),
        base_event(
            "AgentPlanCreatedEvent",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            crew_id=run_id,
            task_id=task_id,
            plan_steps=preview["plan_steps"],
            metadata={"tool_name": preview["tool_name"]},
        ),
        base_event(
            "ToolUsageStartedEvent",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            crew_id=run_id,
            task_id=task_id,
            tool_name=preview["tool_name"],
            metadata={"customer_id": customer_id},
        ),
        base_event(
            "ToolUsageFinishedEvent",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            crew_id=run_id,
            task_id=task_id,
            tool_name=preview["tool_name"],
            metadata={"result_preview": preview["tool_result"]},
        ),
    ]


def main() -> None:
    agent_id = os.getenv("ALEPHANT_AGENT_ID", "crewai-demo-agent")
    run_id = default_run_id("crew")
    agent_name = default_agent_name("CrewAI Demo Agent")
    client = AgentEventClient.from_env()
    customer_id = os.getenv("ALEPHANT_CUSTOMER_ID", "cus_123")
    crew = build_crew(customer_id=customer_id)
    preview = run_support_triage_preview(customer_id=customer_id)
    events = build_events(
        agent_id=agent_id,
        run_id=run_id,
        agent_name=agent_name,
        customer_id=customer_id,
        preview=preview,
    )
    response = client.emit_events(source=SOURCE, events=events)
    crew_result = None
    if not client.dry_run:
        crew_result = str(crew.kickoff(inputs={"customer_id": customer_id}))
    sleep_briefly()
    print(
        {
            "source": SOURCE,
            "run_id": run_id,
            "accepted": response.get("accepted"),
            "crew": crew.name,
            "crew_result": crew_result,
        }
    )


if __name__ == "__main__":
    main()
