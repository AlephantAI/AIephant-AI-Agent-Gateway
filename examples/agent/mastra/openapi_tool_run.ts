type ToolDescriptor = {
  toolId?: string;
  frameworkToolName?: string;
  description?: string;
  inputSchema?: unknown;
  metadata?: Record<string, unknown>;
};

type ToolEnvelope = {
  status?: string;
  output?: unknown;
  error?: { code?: string; message?: string };
  agentAction?: string;
};

const env = process.env;

const gatewayBaseUrl =
  env.AI_GATEWAY_BASE_URL ||
  env.GATEWAY_URL ||
  env.ALEPHANT_GATEWAY_URL ||
  "http://127.0.0.1:3000";
const apiKey =
  env.API_KEY ||
  env.ALEPHANT_API_KEY ||
  env.ALEPHANT_CONTROL_OPENROUTER_API_KEY ||
  env.OPENAI_API_KEY;
const agentId = env.ALEPHANT_AGENT_ID || "openapi-demo-agent";
const agentName = env.ALEPHANT_AGENT_NAME || "OpenAPI Demo Agent";
const runId =
  env.ALEPHANT_RUN_ID || `run_openapi_${crypto.randomUUID().replaceAll("-", "")}`;
const toolId = env.OPENAPI_TOOL_ID || "support.get_ticket";

function headers(stepId: string, toolCallId?: string): Record<string, string> {
  if (!apiKey) {
    throw new Error(
      "Set API_KEY, ALEPHANT_API_KEY, ALEPHANT_CONTROL_OPENROUTER_API_KEY, or OPENAI_API_KEY",
    );
  }
  return {
    authorization: `Bearer ${apiKey}`,
    "content-type": "application/json",
    "alephant-agent-id": agentId,
    "alephant-agent-name": agentName,
    "alephant-run-id": runId,
    "alephant-step-id": stepId,
    ...(toolCallId ? { "alephant-tool-call-id": toolCallId } : {}),
    "alephant-debug-body": env.ALEPHANT_DEBUG_BODY || "true",
  };
}

async function post(path: string, payload: unknown, stepId: string, toolCallId?: string) {
  const response = await fetch(`${gatewayBaseUrl.replace(/\/$/, "")}${path}`, {
    method: "POST",
    headers: headers(stepId, toolCallId),
    body: JSON.stringify(payload),
  });
  const body = await response.text();
  if (!response.ok) {
    throw new Error(`${path} failed: HTTP ${response.status}: ${body}`);
  }
  return body ? JSON.parse(body) : {};
}

function mastraToolShape(descriptor: ToolDescriptor) {
  return {
    id: descriptor.frameworkToolName,
    description: descriptor.description,
    inputSchema: descriptor.inputSchema,
    metadata: descriptor.metadata || {},
    execute: async (args: Record<string, unknown>) => {
      const toolCallId = `call_openapi_${crypto.randomUUID().replaceAll("-", "")}`;
      const envelope = (await post(
        "/v1/agent/tools/call",
        {
          source: "mastra-openapi",
          agent_id: agentId,
          agent_name: agentName,
          run_id: runId,
          step_id: "step_mastra_openapi_tool",
          tool_call_id: toolCallId,
          tool_id: descriptor.toolId,
          arguments: args,
          idempotency_key: `${runId}:step_mastra_openapi_tool:${toolCallId}`,
        },
        "step_mastra_openapi_tool",
        toolCallId,
      )) as ToolEnvelope;
      if (envelope.agentAction === "refresh_tools") {
        await listTools();
      }
      if (envelope.error) {
        return `${envelope.status}: ${envelope.error.code} - ${envelope.error.message}`;
      }
      return JSON.stringify(envelope.output);
    },
  };
}

async function listTools() {
  return post(
    "/v1/agent/tools/list",
    {
      source: "mastra-openapi",
      agent_id: agentId,
      agent_name: agentName,
      run_id: runId,
      capabilities: { schema_dialect: "openai_function" },
    },
    "step_mastra_list_tools",
  );
}

const listed = await listTools();
const descriptor = (listed.tools || []).find(
  (tool: ToolDescriptor) => tool.toolId === toolId,
);
if (!descriptor) {
  throw new Error(`tool_id ${toolId} not found`);
}
const tool = mastraToolShape(descriptor);
const toolOutput = await tool.execute({
  ticket_id: env.OPENAPI_TICKET_ID || "T-1001",
});
const schemaInvalid = await tool.execute({ ticket_id: 12345 });
console.log(
  JSON.stringify(
    {
      framework: "mastra",
      registered_tool: {
        id: tool.id,
        metadata: tool.metadata,
      },
      tool_id: toolId,
      tool_output: toolOutput,
      schema_invalid: schemaInvalid,
    },
    null,
    2,
  ),
);
