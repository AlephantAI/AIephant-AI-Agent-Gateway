import py_compile
import importlib
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parent
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))
REQUIRED_FILES = [
    "README.md",
    "alephant_adapter.py",
    "basic_run.py",
    "tool_run.py",
    "loop_run.py",
]


def import_adapter_module():
    return importlib.import_module("alephant_adapter")


class LangGraphExamplesTest(unittest.TestCase):
    def test_required_example_files_exist(self) -> None:
        missing = [name for name in REQUIRED_FILES if not (ROOT / name).exists()]

        self.assertEqual(missing, [])

    def test_python_examples_compile(self) -> None:
        for name in REQUIRED_FILES:
            if not name.endswith(".py"):
                continue
            with self.subTest(name=name):
                py_compile.compile(str(ROOT / name), doraise=True)

    def test_debug_flags_can_come_from_env_or_request_headers(self) -> None:
        alephant_adapter = import_adapter_module()

        AlephantAgentAdapter = alephant_adapter.AlephantAgentAdapter

        with patch.object(alephant_adapter, "find_dotenv", return_value=None), patch.dict(
            "os.environ", {"AI_GATEWAY_DEBUG_HEADERS": "true"}, clear=True
        ):
            adapter = AlephantAgentAdapter.from_env(agent_id="debug-agent")
            self.assertTrue(adapter._debug_enabled("headers", {}))
            self.assertFalse(adapter._debug_enabled("body", {}))

        with patch.object(alephant_adapter, "find_dotenv", return_value=None), patch.dict(
            "os.environ", {}, clear=True
        ):
            adapter = AlephantAgentAdapter.from_env(
                agent_id="debug-agent",
                default_agent_name="Default Debug Agent",
            )

            self.assertTrue(adapter._debug_enabled("headers", {"alephant-debug-headers": "true"}))
            self.assertTrue(adapter._debug_enabled("body", {"alephant-debug-body": "true"}))
            self.assertFalse(adapter._debug_enabled("headers", {}))
            self.assertEqual(adapter.agent_name, "Default Debug Agent")

    def test_adapter_loads_dotenv_from_current_tree_without_overriding_env(self) -> None:
        AlephantAgentAdapter = import_adapter_module().AlephantAgentAdapter

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            nested = root / "examples" / "agent" / "langgraph"
            nested.mkdir(parents=True)
            (root / ".env").write_text(
                "\n".join(
                    [
                        "GATEWAY_BASE=http://dotenv-gateway:8080",
                        "ALEPHANT_CONTROL_OPENROUTER_API_KEY=dotenv-key",
                        "ALEPHANT_MODEL=dotenv-model",
                        "ALEPHANT_AGENT_NAME=Dotenv Agent",
                    ]
                ),
                encoding="utf-8",
            )

            old_cwd = Path.cwd()
            try:
                os.chdir(nested)
                with patch.dict(
                    "os.environ",
                    {
                        "GATEWAY_BASE": "http://shell-gateway:8080",
                    },
                    clear=True,
                ):
                    adapter = AlephantAgentAdapter.from_env(agent_id="dotenv-agent")

                    self.assertEqual(adapter.base_url, "http://shell-gateway:8080")
                    self.assertEqual(adapter.model, "dotenv-model")
                    self.assertEqual(adapter.agent_name, "Dotenv Agent")
                    self.assertFalse(adapter.dry_run)
            finally:
                os.chdir(old_cwd)

    def test_env_agent_name_overrides_default_demo_name(self) -> None:
        alephant_adapter = import_adapter_module()

        AlephantAgentAdapter = alephant_adapter.AlephantAgentAdapter

        with patch.object(alephant_adapter, "find_dotenv", return_value=None), patch.dict(
            "os.environ",
            {
                "ALEPHANT_API_KEY": "test-key",
                "ALEPHANT_AGENT_NAME": "Env Agent",
            },
            clear=True,
        ):
            adapter = AlephantAgentAdapter.from_env(
                agent_id="demo-agent",
                default_agent_name="Default Demo Agent",
            )

        self.assertEqual(adapter.agent_name, "Env Agent")

    def test_agent_name_is_sent_in_headers_and_event_payload(self) -> None:
        alephant_adapter = import_adapter_module()
        AgentStepContext = alephant_adapter.AgentStepContext
        AlephantAgentAdapter = alephant_adapter.AlephantAgentAdapter

        adapter = AlephantAgentAdapter(
            base_url="http://gateway:8080",
            api_key="test-key",
            agent_id="agent-id",
            run_id="run-id",
            agent_name="Support Bot",
        )
        context = AgentStepContext(step_id="step-1", step_kind="planning")

        self.assertEqual(adapter.agent_headers(context)["Alephant-Agent-Name"], "Support Bot")

        captured: dict[str, object] = {}

        def capture_post(path: str, payload: dict[str, object], **_: object) -> dict[str, object]:
            captured["path"] = path
            captured["payload"] = payload
            return {}

        with patch.object(adapter, "_post_json", side_effect=capture_post):
            adapter.emit("run.started")

        self.assertEqual(captured["path"], "/v1/agent/events")
        events = captured["payload"]["events"]
        self.assertEqual(events[0]["agent_name"], "Support Bot")

    def test_build_event_includes_preflight_phase_and_stage(self) -> None:
        AlephantAgentAdapter = import_adapter_module().AlephantAgentAdapter

        adapter = AlephantAgentAdapter(
            base_url="http://gateway.test",
            api_key="test-key",
            agent_id="agent-1",
            run_id="run-1",
            dry_run=True,
        )

        event = adapter.build_event(
            "tool.call.requested",
            step_id="step-1",
            step_kind="tool_call",
            event_phase="before",
            policy_stage="pre_action",
            metadata={"tool_name": "lookup_ticket"},
        )

        self.assertEqual(event["event_phase"], "before")
        self.assertEqual(event["policy_stage"], "pre_action")
        self.assertEqual(event["metadata"]["tool_name"], "lookup_ticket")

    def test_response_allows_event_reads_matching_decision(self) -> None:
        AlephantAgentAdapter = import_adapter_module().AlephantAgentAdapter

        response = {
            "decisions": [
                {"eventId": "evt-1", "allowed": False, "policyDecision": "denied"},
            ]
        }

        self.assertFalse(AlephantAgentAdapter.response_allows_event(response, "evt-1"))
        self.assertFalse(AlephantAgentAdapter.response_allows_event(response, "evt-2"))
        self.assertTrue(
            AlephantAgentAdapter.response_allows_event({"dry_run": True}, "evt-2")
        )


if __name__ == "__main__":
    unittest.main()
