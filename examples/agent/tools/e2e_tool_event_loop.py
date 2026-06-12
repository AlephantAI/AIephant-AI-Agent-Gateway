#!/usr/bin/env python3
"""End-to-end Agent Tools loop with event and MCP dispatch validation."""

from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
from pathlib import Path
from typing import Any

if __package__:
    from .agent_event_sink import AgentEventSinkServer
    from .e2e_support import (
        E2EAssertionError,
        env_api_key,
        env_gateway_url,
        get_first,
        load_dotenv,
        parse_metadata,
        post_json,
        random_id,
        read_jsonl,
        require,
        wait_until,
        write_json,
    )
else:
    from agent_event_sink import AgentEventSinkServer
    from e2e_support import (
        E2EAssertionError,
        env_api_key,
        env_gateway_url,
        get_first,
        load_dotenv,
        parse_metadata,
        post_json,
        random_id,
        read_jsonl,
        require,
        wait_until,
        write_json,
    )


SCRIPT_DIR = Path(__file__).resolve().parent


def parse_bool(value: str) -> bool:
    normalized = value.strip().lower()
    if normalized in {"1", "true", "yes", "y", "on"}:
        return True
    if normalized in {"0", "false", "no", "n", "off"}:
        return False
    raise argparse.ArgumentTypeError(f"invalid boolean value: {value!r}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run the local Agent Tools E2E event loop."
    )
    parser.add_argument("--gateway-url", default=None)
    parser.add_argument("--api-key", default=None)
    parser.add_argument(
        "--sink-host",
        default=os.getenv("AGENT_EVENT_SINK_HOST", "127.0.0.1"),
    )
    parser.add_argument(
        "--sink-port",
        type=int,
        default=int(os.getenv("AGENT_EVENT_SINK_PORT", "9877")),
    )
    parser.add_argument(
        "--mcp-host",
        default=os.getenv("MCP_STREAMABLE_MOCK_HOST", "127.0.0.1"),
    )
    parser.add_argument(
        "--mcp-port",
        type=int,
        default=int(os.getenv("MCP_STREAMABLE_MOCK_PORT", "8766")),
    )
    parser.add_argument(
        "--start-mcp",
        dest="start_mcp",
        type=parse_bool,
        nargs="?",
        const=True,
        default=True,
        help="start the local MCP Streamable HTTP mock server (default: true)",
    )
    parser.add_argument(
        "--no-start-mcp",
        dest="start_mcp",
        action="store_false",
        help="do not start the local MCP Streamable HTTP mock server",
    )
    parser.add_argument("--artifact-dir", default=None)
    parser.add_argument(
        "--mcp-record-file",
        default=None,
        help=(
            "MCP mock request JSONL path. Defaults to the artifact directory "
            "when --start-mcp is used."
        ),
    )
    parser.add_argument(
        "--require-mcp-lifecycle",
        action="store_true",
        help=(
            "require initialize -> notifications/initialized -> tools/call in "
            "the MCP record file; useful when Redis session cache is disabled"
        ),
    )
    parser.add_argument("--timeout-seconds", type=float, default=20.0)
    return parser.parse_args()


def start_mcp_mock(
    *,
    host: str,
    port: int,
    record_file: Path,
) -> subprocess.Popen[str]:
    record_file.parent.mkdir(parents=True, exist_ok=True)
    record_file.write_text("", encoding="utf-8")
    env = os.environ.copy()
    env["MCP_STREAMABLE_MOCK_HOST"] = host
    env["MCP_STREAMABLE_MOCK_PORT"] = str(port)
    env["MCP_STREAMABLE_MOCK_RECORD_FILE"] = str(record_file)
    env.setdefault("MCP_STREAMABLE_MOCK_RESPONSE_MODE", "success")
    return subprocess.Popen(
        [sys.executable, str(SCRIPT_DIR / "mcp_streamable_mock_server.py")],
        env=env,
        text=True,
    )


def stop_process(process: subprocess.Popen[str] | None) -> None:
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def wait_for_mcp_ready(
    *,
    process: subprocess.Popen[str],
    host: str,
    port: int,
    timeout_seconds: float,
) -> None:
    def ready() -> bool:
        if process.poll() is not None:
            raise E2EAssertionError(
                f"MCP streamable mock exited early with code {process.returncode}"
            )
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return True
        except OSError:
            return False

    wait_until(
        f"MCP streamable mock on {host}:{port}",
        timeout_seconds,
        ready,
    )


def find_tool_descriptor(list_response: dict[str, Any], tool_id: str) -> dict[str, Any]:
    tools = get_first(list_response, ["tools"], [])
    require(isinstance(tools, list), "tools/list response did not contain a tools array")
    for descriptor in tools:
        if not isinstance(descriptor, dict):
            continue
        if get_first(descriptor, ["tool_id", "toolId"]) == tool_id:
            return descriptor
    raise E2EAssertionError(f"tools/list response did not include {tool_id!r}")


def event_body(row: Any) -> dict[str, Any]:
    if isinstance(row, dict):
        body = get_first(row, ["body"], row)
        if isinstance(body, dict):
            return body
    return {}


def event_type(event: dict[str, Any]) -> str:
    value = get_first(event, ["event_type", "eventType", "type"], "")
    return value if isinstance(value, str) else ""


def event_execution_id(event: dict[str, Any]) -> str:
    value = get_first(event, ["tool_execution_id", "toolExecutionId"], "")
    if isinstance(value, str) and value:
        return value
    metadata = safe_parse_metadata(get_first(event, ["metadata"], {}), "event metadata")
    value = get_first(metadata, ["tool_execution_id", "toolExecutionId"], "")
    return value if isinstance(value, str) else ""


def event_text_field(
    event: dict[str, Any],
    names: list[str],
    metadata_names: list[str] | None = None,
) -> str:
    value = get_first(event, names, "")
    if isinstance(value, str) and value:
        return value
    if metadata_names:
        metadata = safe_parse_metadata(get_first(event, ["metadata"], {}), "event metadata")
        value = get_first(metadata, metadata_names, "")
        if isinstance(value, str):
            return value
    return ""


def event_number_field(event: dict[str, Any], names: list[str]) -> int | None:
    value = get_first(event, names)
    if isinstance(value, int):
        return value
    if isinstance(value, str) and value.isdigit():
        return int(value)
    return None


def find_execution_events(
    events_path: Path,
    tool_execution_id: str,
) -> tuple[dict[str, Any], dict[str, Any]] | None:
    requested: dict[str, Any] | None = None
    received: dict[str, Any] | None = None
    requested_index: int | None = None
    received_index: int | None = None
    for index, row in enumerate(read_jsonl(events_path)):
        body = event_body(row)
        if event_execution_id(body) != tool_execution_id:
            continue
        if event_type(body) == "tool.call.requested":
            requested = body
            requested_index = index
        elif event_type(body) == "tool.result.received":
            received = body
            received_index = index
    if requested and received:
        require(
            requested_index is not None
            and received_index is not None
            and requested_index < received_index,
            "tool events were not ordered requested -> result",
        )
        return requested, received
    return None


def safe_parse_metadata(value: Any, label: str) -> dict[str, Any]:
    try:
        return parse_metadata(value)
    except json.JSONDecodeError as exc:
        raise E2EAssertionError(f"{label} was not valid JSON: {exc}") from exc


def metadata_for_event(event: dict[str, Any]) -> dict[str, Any]:
    return safe_parse_metadata(get_first(event, ["metadata"], {}), "event metadata")


def verify_mcp_record_file(
    *,
    record_file: Path | None,
    required: bool,
    expected_arguments: dict[str, Any],
    require_lifecycle: bool,
) -> tuple[bool, str]:
    if record_file is None:
        return False, "not started and --mcp-record-file was not provided"
    records = read_jsonl(record_file)
    if records:
        methods = [
            get_first(record.get("body", {}), ["method"], "")
            for record in records
            if isinstance(record, dict)
        ]
        matching_tools_calls = []
        for record in records:
            if not isinstance(record, dict):
                continue
            body = record.get("body", {})
            if not isinstance(body, dict):
                continue
            params = body.get("params", {})
            arguments = params.get("arguments", {}) if isinstance(params, dict) else {}
            if body.get("method") == "tools/call" and arguments == expected_arguments:
                matching_tools_calls.append(record)
        require(
            bool(matching_tools_calls),
            (
                "MCP records did not contain a tools/call for this run: "
                f"methods={methods}"
            ),
        )
        tools_call = matching_tools_calls[0]
        tools_call_index = records.index(tools_call)

        lifecycle_methods = ["initialize", "notifications/initialized"]
        lifecycle_present = all(method in methods for method in lifecycle_methods)
        if require_lifecycle or lifecycle_present:
            positions: dict[str, int] = {}
            for method in [*lifecycle_methods, "tools/call"]:
                if method == "tools/call":
                    positions[method] = tools_call_index
                elif method in methods:
                    positions[method] = methods.index(method)
            require(
                all(method in positions for method in [*lifecycle_methods, "tools/call"]),
                f"MCP records missing lifecycle methods: methods={methods}",
            )
            require(
                positions["initialize"]
                < positions["notifications/initialized"]
                < positions["tools/call"],
                (
                    "MCP methods were not ordered initialize -> initialized -> "
                    f"tools/call: {methods}"
                ),
            )

        body = tools_call.get("body", {})
        headers = tools_call.get("headers", {})
        params = body.get("params", {}) if isinstance(body, dict) else {}
        arguments = params.get("arguments", {}) if isinstance(params, dict) else {}
        require(body.get("jsonrpc") == "2.0", f"MCP tools/call jsonrpc mismatch: {body}")
        require(params.get("name") == "docs.search", f"MCP tools/call name mismatch: {params}")
        require(
            arguments == expected_arguments,
            f"MCP tools/call arguments mismatch: {arguments}",
        )
        require(
            bool(headers.get("mcp-session-id")),
            f"MCP tools/call missing mcp-session-id header: {headers}",
        )
        if lifecycle_present:
            return True, "record file contains valid MCP initialize/initialized/tools/call"
        return True, "record file contains valid cached-session MCP tools/call"
    if required:
        raise E2EAssertionError(f"MCP record file was empty: {record_file}")
    return False, f"record file was empty: {record_file}"


def verify_event_correlation(
    *,
    requested_event: dict[str, Any],
    received_event: dict[str, Any],
    ids: dict[str, str],
) -> None:
    for event in (requested_event, received_event):
        require(
            event_text_field(event, ["alephantRunId", "alephant_run_id", "run_id"])
            == ids["run_id"],
            f"event run id mismatch: {event}",
        )
        require(
            event_text_field(event, ["alephantStepId", "alephant_step_id", "step_id"])
            == ids["step_id"],
            f"event step id mismatch: {event}",
        )
        require(
            event_text_field(
                event,
                ["toolCallId", "tool_call_id"],
                ["toolCallId", "tool_call_id"],
            )
            == ids["tool_call_id"],
            f"event tool call id mismatch: {event}",
        )
        require(
            event_execution_id(event) == ids["tool_execution_id"],
            f"event tool execution id mismatch: {event}",
        )
    requested_sequence = event_number_field(requested_event, ["sequence"])
    received_sequence = event_number_field(received_event, ["sequence"])
    if requested_sequence is not None and received_sequence is not None:
        require(
            requested_sequence < received_sequence,
            "event sequence was not requested < result",
        )


def verify_result_metadata(received_event: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
    metadata = metadata_for_event(received_event)
    billing = metadata.get("billing") if isinstance(metadata.get("billing"), dict) else {}
    gateway = metadata.get("gateway") if isinstance(metadata.get("gateway"), dict) else {}
    require(
        get_first(billing, ["cost_type", "costType"]) == "tool",
        "result metadata.billing.costType was not tool",
    )
    require(get_first(billing, ["status"]) == "settled", f"billing status mismatch: {billing}")
    require(get_first(billing, ["billable"]) is True, f"billing billable mismatch: {billing}")
    require(
        isinstance(get_first(billing, ["amount_micros", "amountMicros", "costMicros"]), int)
        and get_first(billing, ["amount_micros", "amountMicros", "costMicros"]) > 0,
        f"billing amountMicros missing or non-positive: {billing}",
    )
    require(bool(get_first(billing, ["currency"], "")), f"billing currency missing: {billing}")
    require(
        bool(get_first(billing, ["pricing_source", "pricingSource"], "")),
        f"billing pricingSource missing: {billing}",
    )
    require(
        get_first(billing, ["observed_kind", "observedKind"]) == "runtime_confirmed",
        f"billing observedKind mismatch: {billing}",
    )
    require(
        bool(get_first(billing, ["dedupe_key", "dedupeKey"], "")),
        "result metadata.billing.dedupeKey was empty",
    )
    require(
        get_first(gateway, ["execution_source", "executionSource"])
        == "gateway_executed",
        "result metadata.gateway.executionSource was not gateway_executed",
    )
    require(
        get_first(gateway, ["blocked_before_dispatch", "blockedBeforeDispatch"]) is False,
        "result metadata.gateway.blockedBeforeDispatch was not false",
    )
    require(
        get_first(gateway, ["target_kind", "targetKind"]) == "mcp-streamable-http",
        f"gateway targetKind mismatch: {gateway}",
    )
    require(bool(get_first(gateway, ["target_id", "targetId"], "")), f"targetId missing: {gateway}")
    require(
        isinstance(get_first(gateway, ["latency_ms", "latencyMs"]), int),
        f"gateway latencyMs missing: {gateway}",
    )
    return billing, gateway


def default_mcp_record_file(artifact_dir: Path) -> Path:
    return artifact_dir / "mock-mcp-requests.jsonl"


def write_artifacts(
    *,
    artifact_dir: Path,
    ids: dict[str, str],
    gateway_url: str,
    descriptor: dict[str, Any],
    call_response: dict[str, Any],
    requested_event: dict[str, Any],
    received_event: dict[str, Any],
    mcp_record_file: Path | None,
    mcp_record_verified: bool,
    mcp_record_verification_reason: str,
) -> None:
    metadata = metadata_for_event(received_event)
    billing = metadata.get("billing") if isinstance(metadata.get("billing"), dict) else {}
    gateway = metadata.get("gateway") if isinstance(metadata.get("gateway"), dict) else {}

    write_json(
        artifact_dir / "run-manifest.json",
        {
            "gatewayUrl": gateway_url,
            "agentId": ids["agent_id"],
            "runId": ids["run_id"],
            "stepId": ids["step_id"],
            "toolCallId": ids["tool_call_id"],
            "toolExecutionId": ids["tool_execution_id"],
            "toolId": get_first(descriptor, ["tool_id", "toolId"]),
            "schemaHash": get_first(descriptor, ["schema_hash", "schemaHash"]),
            "costPolicy": get_first(descriptor, ["cost_policy", "costPolicy"], {}),
            "metadata": safe_parse_metadata(
                get_first(descriptor, ["metadata"], {}),
                "tool descriptor metadata",
            ),
            "mcpRecordFile": str(mcp_record_file) if mcp_record_file else None,
            "mcpRecordVerified": mcp_record_verified,
            "mcpRecordVerificationReason": mcp_record_verification_reason,
        },
    )
    write_json(
        artifact_dir / "timeline-summary.json",
        {
            "events": [
                {
                    "eventType": event_type(requested_event),
                    "eventId": get_first(requested_event, ["event_id", "eventId"]),
                    "eventTime": get_first(requested_event, ["event_time", "eventTime"]),
                    "toolExecutionId": event_execution_id(requested_event),
                },
                {
                    "eventType": event_type(received_event),
                    "eventId": get_first(received_event, ["event_id", "eventId"]),
                    "eventTime": get_first(received_event, ["event_time", "eventTime"]),
                    "toolExecutionId": event_execution_id(received_event),
                    "status": get_first(call_response, ["status"]),
                },
            ]
        },
    )
    write_json(
        artifact_dir / "run-cost-summary.json",
        {
            "runId": ids["run_id"],
            "toolExecutionId": ids["tool_execution_id"],
            "billing": {
                "costType": get_first(billing, ["cost_type", "costType"]),
                "costSubtype": get_first(billing, ["cost_subtype", "costSubtype"]),
                "status": get_first(billing, ["status"]),
                "billable": get_first(billing, ["billable"]),
                "amountMicros": get_first(billing, ["amount_micros", "amountMicros", "costMicros"]),
                "currency": get_first(billing, ["currency"]),
                "dedupeKey": get_first(billing, ["dedupe_key", "dedupeKey"]),
            },
            "gateway": {
                "executionSource": get_first(gateway, ["execution_source", "executionSource"]),
                "blockedBeforeDispatch": get_first(
                    gateway,
                    ["blocked_before_dispatch", "blockedBeforeDispatch"],
                ),
                "targetKind": get_first(gateway, ["target_kind", "targetKind"]),
            },
        },
    )


def run() -> Path:
    load_dotenv()
    args = parse_args()
    gateway_url = (args.gateway_url or env_gateway_url()).rstrip("/")
    api_key = args.api_key or env_api_key()

    ids = {
        "agent_id": random_id("agent"),
        "run_id": random_id("run"),
        "step_id": random_id("step"),
        "tool_call_id": random_id("call"),
    }
    artifact_dir = Path(args.artifact_dir or f"/tmp/alephant-agent-tools-e2e/{ids['run_id']}")
    artifact_dir.mkdir(parents=True, exist_ok=True)
    events_path = artifact_dir / "agent-events.jsonl"
    mcp_record_file = (
        Path(args.mcp_record_file)
        if args.mcp_record_file
        else default_mcp_record_file(artifact_dir)
        if args.start_mcp
        else None
    )

    sink: AgentEventSinkServer | None = None
    mcp_process: subprocess.Popen[str] | None = None
    try:
        try:
            sink = AgentEventSinkServer(
                host=args.sink_host,
                port=args.sink_port,
                output_path=events_path,
            ).start()
        except OSError as exc:
            raise E2EAssertionError(
                f"failed to start Agent Event sink on "
                f"{args.sink_host}:{args.sink_port}: {exc}"
            ) from exc
        if args.start_mcp:
            mcp_process = start_mcp_mock(
                host=args.mcp_host,
                port=args.mcp_port,
                record_file=mcp_record_file or default_mcp_record_file(artifact_dir),
            )
            wait_for_mcp_ready(
                process=mcp_process,
                host=args.mcp_host,
                port=args.mcp_port,
                timeout_seconds=args.timeout_seconds,
            )

        common_payload = {
            "source": "e2e_tool_event_loop",
            "agent_id": ids["agent_id"],
            "agent_name": "Agent Tools E2E",
            "run_id": ids["run_id"],
        }
        list_response = post_json(
            base_url=gateway_url,
            path="/v1/agent/tools/list",
            payload={
                **common_payload,
                "capabilities": {"schema_dialect": "openai_function"},
            },
            api_key=api_key,
            timeout_seconds=args.timeout_seconds,
        )
        descriptor = find_tool_descriptor(list_response, "docs.search")
        tool_id = get_first(descriptor, ["tool_id", "toolId"])
        schema_hash = get_first(descriptor, ["schema_hash", "schemaHash"])
        cost_policy = get_first(descriptor, ["cost_policy", "costPolicy"])
        descriptor_metadata = safe_parse_metadata(
            get_first(descriptor, ["metadata"], {}),
            "tool descriptor metadata",
        )
        require(tool_id == "docs.search", "selected tool descriptor mismatch")
        require(
            isinstance(schema_hash, str) and schema_hash,
            "docs.search schemaHash is empty",
        )
        require(isinstance(cost_policy, dict), "docs.search costPolicy is missing")

        tool_arguments = {"query": f"refund policy {ids['run_id']}"}
        call_response = post_json(
            base_url=gateway_url,
            path="/v1/agent/tools/call",
            payload={
                **common_payload,
                "step_id": ids["step_id"],
                "tool_call_id": ids["tool_call_id"],
                "tool_id": tool_id,
                "schemaHash": schema_hash,
                "arguments": tool_arguments,
                "idempotency_key": (
                    f"{ids['run_id']}:{ids['step_id']}:{ids['tool_call_id']}"
                ),
            },
            api_key=api_key,
            timeout_seconds=args.timeout_seconds,
        )
        status = get_first(call_response, ["status"], "")
        tool_execution_id = get_first(
            call_response,
            ["tool_execution_id", "toolExecutionId"],
            "",
        )
        require(status == "completed", f"tool call did not complete: status={status!r}")
        require(
            isinstance(tool_execution_id, str) and tool_execution_id,
            "tool call response did not include a tool execution id",
        )
        ids["tool_execution_id"] = tool_execution_id

        requested_event, received_event = wait_until(
            f"requested and result events for {tool_execution_id}",
            args.timeout_seconds,
            lambda: find_execution_events(events_path, tool_execution_id),
        )
        verify_event_correlation(
            requested_event=requested_event,
            received_event=received_event,
            ids=ids,
        )
        verify_result_metadata(received_event)
        mcp_record_verified, mcp_record_verification_reason = verify_mcp_record_file(
            record_file=mcp_record_file,
            required=args.start_mcp or bool(args.mcp_record_file),
            expected_arguments=tool_arguments,
            require_lifecycle=args.require_mcp_lifecycle,
        )

        descriptor["metadata"] = descriptor_metadata
        write_artifacts(
            artifact_dir=artifact_dir,
            ids=ids,
            gateway_url=gateway_url,
            descriptor=descriptor,
            call_response=call_response,
            requested_event=requested_event,
            received_event=received_event,
            mcp_record_file=mcp_record_file,
            mcp_record_verified=mcp_record_verified,
            mcp_record_verification_reason=mcp_record_verification_reason,
        )
        return artifact_dir
    finally:
        if sink is not None:
            sink.stop()
        stop_process(mcp_process)


def main() -> int:
    try:
        artifact_dir = run()
    except E2EAssertionError as exc:
        print(
            "Agent Tools E2E failed: "
            f"{exc}. Ensure the gateway is running with "
            "examples/agent/tools/e2e.agent-tools.yaml.",
            file=sys.stderr,
        )
        return 1
    print(f"Agent Tools E2E passed. Artifacts: {artifact_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
