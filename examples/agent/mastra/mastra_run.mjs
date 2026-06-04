// Mastra SDK agent example for Alephant Agent Gateway.

import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Agent } from '@mastra/core/agent';
import { createTool } from '@mastra/core/tools';
import { createOpenAICompatible } from '@ai-sdk/openai-compatible';
import { z } from 'zod';

const SOURCE = 'mastra';
const EVENT_VERSION = '2026-05-27';
const __dirname = dirname(fileURLToPath(import.meta.url));

function loadDotenv() {
  const candidates = [
    resolve(process.cwd(), '.env'),
    resolve(__dirname, '../../../.env'),
    resolve(__dirname, '../../../../.env'),
  ];

  for (const path of candidates) {
    let text;
    try {
      text = readFileSync(path, 'utf8');
    } catch {
      continue;
    }

    for (const rawLine of text.split(/\r?\n/)) {
      let line = rawLine.trim();
      if (!line || line.startsWith('#')) {
        continue;
      }
      if (line.startsWith('export ')) {
        line = line.slice('export '.length).trim();
      }
      const eq = line.indexOf('=');
      if (eq < 1) {
        continue;
      }
      const key = line.slice(0, eq).trim();
      let value = line.slice(eq + 1).trim();
      if (!key || process.env[key]) {
        continue;
      }
      if (
        value.length >= 2 &&
        ((value.startsWith('"') && value.endsWith('"')) ||
          (value.startsWith("'") && value.endsWith("'")))
      ) {
        value = value.slice(1, -1);
      }
      process.env[key] = value;
    }
    return;
  }
}

function truthy(value) {
  return ['1', 'true', 'yes', 'on'].includes(String(value || '').trim().toLowerCase());
}

function gatewayChatBaseUrl() {
  loadDotenv();
  const baseUrl = (
    process.env.GATEWAY_BASE ||
    process.env.ALEPHANT_GATEWAY_URL ||
    'http://127.0.0.1:8080'
  ).replace(/\/+$/, '');
  return baseUrl.endsWith('/v1') ? baseUrl : `${baseUrl}/v1`;
}

function gatewayApiKey() {
  loadDotenv();
  return process.env.ALEPHANT_API_KEY || process.env.ALEPHANT_CONTROL_OPENROUTER_API_KEY;
}

function defaultRunId(prefix) {
  return process.env.ALEPHANT_RUN_ID || `${prefix}_${crypto.randomUUID().replaceAll('-', '')}`;
}

function baseEvent(type, { agentId, runId, agentName, metadata = {}, ...rawFields }) {
  return {
    version: EVENT_VERSION,
    event_id: `evt_${crypto.randomUUID().replaceAll('-', '')}`,
    type,
    agent_id: agentId,
    run_id: runId,
    timestamp: new Date().toISOString(),
    metadata,
    ...(agentName ? { agent_name: agentName } : {}),
    ...Object.fromEntries(
      Object.entries(rawFields).filter(([, value]) => value !== undefined && value !== null),
    ),
  };
}

function lookupSupportPolicy(query) {
  const normalized = query.toLowerCase();
  if (normalized.includes('refund')) {
    return 'refund policy: verify account standing, confirm payment state, then draft a concise operator next step.';
  }
  if (normalized.includes('risk')) {
    return 'risk policy: collect evidence, pause automated action, then escalate to a specialist.';
  }
  return 'support policy: gather context, inspect account state, then summarize the recommended action.';
}

export const supportPolicyTool = createTool({
  id: 'supportPolicyLookup',
  description: 'Look up a support policy note for a customer operation request.',
  inputSchema: z.object({
    query: z.string(),
  }),
  outputSchema: z.object({
    policy: z.string(),
  }),
  execute: async ({ context }) => {
    return { policy: lookupSupportPolicy(context.query) };
  },
});

export function buildGatewayModel() {
  const apiKey = gatewayApiKey();
  const provider = createOpenAICompatible({
    name: 'alephant-gateway',
    apiKey,
    baseURL: gatewayChatBaseUrl(),
  });
  const modelName = process.env.ALEPHANT_MODEL || 'openai/gpt-4o-mini';
  return provider.chatModel ? provider.chatModel(modelName) : provider(modelName);
}

export function buildAgent() {
  return new Agent({
    id: 'mastra-support-planner',
    name: 'Mastra Support Planner',
    instructions:
      'Think briefly, make a concise plan, call supportPolicyLookup when policy context is needed, then write the final support operator answer.',
    model: buildGatewayModel(),
    tools: {
      supportPolicyLookup: supportPolicyTool,
    },
  });
}

export function runSupportPlanningPreview({ query = 'refund risk escalation' } = {}) {
  const toolResult = lookupSupportPolicy(query);
  return {
    query,
    thinking: `Reason about the Mastra support workflow before answering: ${query}.`,
    planSteps: [
      'Classify the support request.',
      'Call supportPolicyLookup for grounded policy context.',
      'Ask the model to produce the final operator-facing answer.',
    ],
    toolName: 'supportPolicyLookup',
    toolResult,
    llmPrompt: `Using this policy context: ${toolResult} Answer the support request: ${query}`,
  };
}

export function buildEvents({
  agentId,
  runId,
  agentName,
  query = 'refund risk escalation',
  preview = runSupportPlanningPreview({ query }),
}) {
  const traceId = `trace_${runId}`;
  return [
    baseEvent('workflow.run.started', {
      agentId,
      runId,
      agentName,
      traceId,
      spanId: 'span_workflow',
      metadata: { workflow_name: 'Mastra support planning workflow' },
    }),
    baseEvent('agent.thinking.started', {
      agentId,
      runId,
      agentName,
      traceId,
      spanId: 'span_thinking',
      parentId: 'span_workflow',
      metadata: { thinking: preview.thinking },
    }),
    baseEvent('agent.plan.created', {
      agentId,
      runId,
      agentName,
      traceId,
      spanId: 'span_plan',
      parentId: 'span_thinking',
      planSteps: preview.planSteps,
      metadata: { toolName: preview.toolName },
    }),
    baseEvent('tool.call.started', {
      agentId,
      runId,
      agentName,
      traceId,
      spanId: 'span_tool',
      parentId: 'span_plan',
      toolName: preview.toolName,
      metadata: { query },
    }),
    baseEvent('tool.call.finished', {
      agentId,
      runId,
      agentName,
      traceId,
      spanId: 'span_tool_result',
      parentId: 'span_tool',
      toolName: preview.toolName,
      metadata: { result_preview: preview.toolResult },
    }),
    baseEvent('llm.call.started', {
      agentId,
      runId,
      agentName,
      traceId,
      spanId: 'span_llm',
      parentId: 'span_tool_result',
      metadata: {
        model: process.env.ALEPHANT_MODEL || 'openai/gpt-4o-mini',
        provider: 'alephant-gateway',
        prompt: preview.llmPrompt,
      },
    }),
  ];
}

function requestHeaders({ apiKey, debugHeaders, debugBody }) {
  const headers = {
    'Content-Type': 'application/json',
  };
  if (apiKey) {
    headers.Authorization = `Bearer ${apiKey}`;
  }
  if (debugHeaders) {
    headers['alephant-debug-headers'] = 'true';
  }
  if (debugBody) {
    headers['alephant-debug-body'] = 'true';
  }
  return headers;
}

async function emitEvents({ source, events, dryRun }) {
  const baseUrl = (
    process.env.GATEWAY_BASE ||
    process.env.ALEPHANT_GATEWAY_URL ||
    'http://127.0.0.1:8080'
  ).replace(/\/+$/, '');
  const path = '/v1/agent/events';
  const debugHeaders = truthy(process.env.AI_GATEWAY_DEBUG_HEADERS);
  const debugBody = truthy(process.env.AI_GATEWAY_DEBUG_BODY);
  const headers = requestHeaders({
    apiKey: gatewayApiKey(),
    debugHeaders,
    debugBody,
  });
  const payload = { source, events };

  if (dryRun) {
    if (debugHeaders) {
      console.log(JSON.stringify({ label: `dry_run.request_headers ${path}`, headers }, null, 2));
    }
    if (debugBody) {
      console.log(JSON.stringify({ label: `dry_run.request_body ${path}`, payload }, null, 2));
    }
    if (!debugHeaders && !debugBody) {
      console.log(JSON.stringify({ label: `dry_run.post ${path}`, payload }, null, 2));
    }
    return { dry_run: true, accepted: events.length };
  }

  const response = await fetch(`${baseUrl}${path}`, {
    method: 'POST',
    headers,
    body: JSON.stringify(payload),
  });
  const responseText = await response.text();
  if (!response.ok) {
    throw new Error(`Alephant request failed: ${response.status} ${responseText}`);
  }
  return responseText ? JSON.parse(responseText) : {};
}

async function runAgent({ query }) {
  const agent = buildAgent();
  const result = await agent.generate(query);
  return result.text || String(result);
}

export async function main() {
  loadDotenv();
  const agentId = process.env.ALEPHANT_AGENT_ID || 'mastra-demo-agent';
  const runId = defaultRunId('run_mastra');
  const agentName = process.env.ALEPHANT_AGENT_NAME || 'Mastra Demo Agent';
  const query = process.env.ALEPHANT_AGENT_QUERY || 'refund risk escalation';
  const dryRun = truthy(process.env.ALEPHANT_AGENT_DRY_RUN) || !gatewayApiKey();
  const preview = runSupportPlanningPreview({ query });
  const events = buildEvents({ agentId, runId, agentName, query, preview });
  const response = await emitEvents({ source: SOURCE, events, dryRun });
  let agentResult = null;
  if (!dryRun) {
    agentResult = await runAgent({ query });
  }
  console.log({
    source: SOURCE,
    run_id: runId,
    accepted: response.accepted,
    agent: 'Mastra Support Planner',
    agent_result: agentResult,
  });
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
