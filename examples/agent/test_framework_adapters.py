import json
import importlib.util
import py_compile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parent


FRAMEWORKS = {
    "openai_agents": {
        "script": "openai_agents_run.py",
        "source": "openai_agents",
        "events": [
            "agent_thinking",
            "plan_created",
            "tool_called",
            "tool_output",
            "llm_request_started",
            "handoff_requested",
        ],
        "required_raw": ["trace_id", "span_id", "plan_steps"],
    },
    "n8n": {
        "script": "n8n_run.py",
        "source": "n8n",
        "events": ["execution.started", "node.started", "node.finished"],
        "required_raw": ["workflowId", "executionId", "nodeType"],
    },
    "crewai": {
        "script": "crewai_run.py",
        "source": "crewai",
        "events": [
            "CrewKickoffStartedEvent",
            "AgentThinkingStartedEvent",
            "AgentPlanCreatedEvent",
            "ToolUsageStartedEvent",
            "ToolUsageFinishedEvent",
        ],
        "required_raw": ["crew_id", "task_id", "tool_name", "plan_steps"],
    },
    "mastra": {
        "script": "mastra_run.py",
        "source": "mastra",
        "events": [
            "workflow.run.started",
            "tool.call.started",
            "llm.call.started",
        ],
        "required_raw": ["traceId", "spanId", "toolName"],
    },
}


def import_script(framework: str, script_name: str):
    path = ROOT / framework / script_name
    spec = importlib.util.spec_from_file_location(f"{framework}_example", path)
    module = importlib.util.module_from_spec(spec)
    assert spec is not None
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


class FrameworkAdapterExamplesTest(unittest.TestCase):
    def test_required_example_files_exist(self) -> None:
        for framework, config in FRAMEWORKS.items():
            with self.subTest(framework=framework):
                directory = ROOT / framework
                self.assertTrue((directory / "README.md").is_file())
                self.assertTrue((directory / config["script"]).is_file())

    def test_python_examples_compile(self) -> None:
        files = [ROOT / "framework_common.py"]
        files.extend(
            ROOT / framework / config["script"]
            for framework, config in FRAMEWORKS.items()
        )
        for path in files:
            with self.subTest(path=path):
                py_compile.compile(str(path), doraise=True)

    def test_framework_payloads_include_expected_native_events(self) -> None:
        for framework, config in FRAMEWORKS.items():
            with self.subTest(framework=framework):
                module = import_script(framework, config["script"])
                events = module.build_events(
                    agent_id=f"{framework}-agent",
                    run_id=f"{framework}-run",
                    agent_name=f"{framework} Demo Agent",
                )

                self.assertEqual(module.SOURCE, config["source"])
                self.assertGreaterEqual(len(events), len(config["events"]))
                self.assertEqual(
                    [event["type"] for event in events[: len(config["events"])]],
                    config["events"],
                )
                for event in events:
                    self.assertEqual(event["agent_id"], f"{framework}-agent")
                    self.assertEqual(event["agent_name"], f"{framework} Demo Agent")
                    self.assertIn("event_id", event)
                    self.assertIn("timestamp", event)
                    self.assertIn("metadata", event)
                for raw_key in config["required_raw"]:
                    self.assertTrue(
                        any(raw_key in event for event in events),
                        f"{framework} payload should include native field {raw_key}",
                    )

    def test_crewai_example_builds_real_crewai_crew(self) -> None:
        module = import_script("crewai", "crewai_run.py")

        with tempfile.TemporaryDirectory() as home:
            with patch.dict(
                "os.environ",
                {
                    "HOME": home,
                    "CREWAI_DISABLE_TELEMETRY": "true",
                    "CREWAI_DISABLE_TRACKING": "true",
                    "OTEL_SDK_DISABLED": "true",
                },
                clear=False,
            ):
                from crewai import Agent, Crew, Task
                from crewai.tools import BaseTool

                crew = module.build_crew(customer_id="cus_123")

        self.assertIsInstance(crew, Crew)
        self.assertEqual(len(crew.agents), 1)
        self.assertEqual(len(crew.tasks), 1)
        self.assertIsInstance(crew.agents[0], Agent)
        self.assertIsInstance(crew.tasks[0], Task)
        self.assertGreaterEqual(len(crew.agents[0].tools), 1)
        self.assertIsInstance(crew.agents[0].tools[0], BaseTool)

    def test_crewai_example_uses_gateway_openrouter_llm(self) -> None:
        module = import_script("crewai", "crewai_run.py")

        with tempfile.TemporaryDirectory() as home:
            with patch.dict(
                "os.environ",
                {
                    "HOME": home,
                    "GATEWAY_BASE": "http://gateway.local:8080",
                    "ALEPHANT_API_KEY": "vk-test",
                    "ALEPHANT_MODEL": "openai/gpt-4o-mini",
                    "CREWAI_DISABLE_TELEMETRY": "true",
                    "CREWAI_DISABLE_TRACKING": "true",
                    "OTEL_SDK_DISABLED": "true",
                },
                clear=False,
            ):
                llm = module.build_gateway_llm()
                crew = module.build_crew(customer_id="cus_123")

        self.assertEqual(llm.model, "openai/gpt-4o-mini")
        self.assertEqual(llm.api_key, "vk-test")
        self.assertEqual(llm.base_url, "http://gateway.local:8080/v1")
        self.assertEqual(llm.provider, "openai")
        self.assertEqual(crew.agents[0].llm.model, "openai/gpt-4o-mini")
        self.assertEqual(crew.agents[0].llm.api_key, "vk-test")
        self.assertEqual(crew.agents[0].llm.base_url, "http://gateway.local:8080/v1")

    def test_crewai_example_has_think_plan_tool_preview(self) -> None:
        module = import_script("crewai", "crewai_run.py")

        preview = module.run_support_triage_preview(customer_id="cus_123")
        self.assertEqual(preview["customer_id"], "cus_123")
        self.assertIn("thinking", preview)
        self.assertGreaterEqual(len(preview["plan_steps"]), 2)
        self.assertIn("customer is in good standing", preview["tool_result"])

        events = module.build_events(
            agent_id="crewai-agent",
            run_id="crewai-run",
            agent_name="CrewAI Demo Agent",
            customer_id="cus_123",
            preview=preview,
        )
        self.assertEqual(
            [event["type"] for event in events[:5]],
            [
                "CrewKickoffStartedEvent",
                "AgentThinkingStartedEvent",
                "AgentPlanCreatedEvent",
                "ToolUsageStartedEvent",
                "ToolUsageFinishedEvent",
            ],
        )
        self.assertEqual(events[1]["metadata"]["thinking"], preview["thinking"])
        self.assertEqual(events[2]["plan_steps"], preview["plan_steps"])
        self.assertEqual(events[4]["metadata"]["result_preview"], preview["tool_result"])

    def test_openai_agents_example_builds_real_agent_with_gateway_model(self) -> None:
        module = import_script("openai_agents", "openai_agents_run.py")

        with patch.dict(
            "os.environ",
            {
                "GATEWAY_BASE": "http://gateway.local:8080",
                "ALEPHANT_API_KEY": "vk-test",
                "ALEPHANT_MODEL": "openai/gpt-4o-mini",
            },
            clear=False,
        ):
            model = module.build_gateway_model()
            agent = module.build_agent()

        self.assertEqual(model.model, "openai/gpt-4o-mini")
        self.assertEqual(str(model._client.base_url), "http://gateway.local:8080/v1/")
        self.assertEqual(agent.name, "OpenAI Agents Support Planner")
        self.assertGreaterEqual(len(agent.tools), 1)
        self.assertEqual(agent.tools[0].name, "lookup_support_policy")
        self.assertEqual(agent.model.model, "openai/gpt-4o-mini")

    def test_openai_agents_example_has_think_plan_tool_llm_preview(self) -> None:
        module = import_script("openai_agents", "openai_agents_run.py")

        preview = module.run_support_planning_preview(query="refund escalation")
        self.assertIn("thinking", preview)
        self.assertGreaterEqual(len(preview["plan_steps"]), 2)
        self.assertIn("refund", preview["tool_result"])
        self.assertIn("llm_prompt", preview)

        events = module.build_events(
            agent_id="openai-agent",
            run_id="openai-run",
            agent_name="OpenAI Agents Demo Agent",
            query="refund escalation",
            preview=preview,
        )
        self.assertEqual(
            [event["type"] for event in events[:6]],
            [
                "agent_thinking",
                "plan_created",
                "tool_called",
                "tool_output",
                "llm_request_started",
                "handoff_requested",
            ],
        )
        self.assertEqual(events[0]["metadata"]["thinking"], preview["thinking"])
        self.assertEqual(events[1]["plan_steps"], preview["plan_steps"])
        self.assertEqual(events[3]["metadata"]["result_preview"], preview["tool_result"])
        self.assertEqual(events[4]["metadata"]["prompt"], preview["llm_prompt"])

    def test_mastra_example_declares_real_sdk_project(self) -> None:
        package_json = ROOT / "mastra" / "package.json"
        script = ROOT / "mastra" / "mastra_run.mjs"

        self.assertTrue(package_json.is_file())
        self.assertTrue(script.is_file())

        package_text = package_json.read_text(encoding="utf-8")
        script_text = script.read_text(encoding="utf-8")

        self.assertIn('"@mastra/core"', package_text)
        self.assertIn('"@ai-sdk/openai-compatible"', package_text)
        self.assertIn("from '@mastra/core/agent'", script_text)
        self.assertIn("from '@mastra/core/tools'", script_text)
        self.assertIn("createOpenAICompatible", script_text)
        self.assertIn("ALEPHANT_API_KEY", script_text)
        self.assertIn("GATEWAY_BASE", script_text)
        self.assertIn("/v1/agent/events", script_text)

    def test_n8n_example_declares_importable_workflow(self) -> None:
        workflow_path = ROOT / "n8n" / "workflow.json"

        self.assertTrue(workflow_path.is_file())

        workflow = json.loads(workflow_path.read_text(encoding="utf-8"))
        nodes = workflow["nodes"]
        node_names = {node["name"] for node in nodes}
        node_types = {node["type"] for node in nodes}
        urls = [
            node.get("parameters", {}).get("url", "")
            for node in nodes
            if node["type"] == "n8n-nodes-base.httpRequest"
        ]
        headers_text = json.dumps(
            [
                node.get("parameters", {}).get("headerParameters")
                for node in nodes
            ]
        )

        self.assertIn("Manual Trigger", node_names)
        self.assertIn("n8n-nodes-base.manualTrigger", node_types)
        self.assertIn("Call Gateway LLM", node_names)
        self.assertIn("Emit Alephant Agent Events", node_names)
        self.assertTrue(any("/v1/chat/completions" in url for url in urls))
        self.assertTrue(any("/v1/agent/events" in url for url in urls))
        self.assertIn("ALEPHANT_API_KEY", headers_text)
        self.assertIn("connections", workflow)


if __name__ == "__main__":
    unittest.main()
