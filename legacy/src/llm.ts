import Anthropic from "@anthropic-ai/sdk";
import OpenAI from "openai";

export interface Planner {
  create(params: Anthropic.MessageCreateParamsNonStreaming): Promise<Anthropic.Message>;
}

export function isOpenAiModel(model: string): boolean {
  const m = model.toLowerCase();
  return m.startsWith("gpt") || m.includes("luna");
}

const clients = new Map<string, Planner>();

export function plannerFor(model: string, keys: { anthropic: string; openai: string }): Planner {
  const openai = isOpenAiModel(model);
  const apiKey = openai ? keys.openai : keys.anthropic;
  if (!apiKey) throw new Error(`missing ${openai ? "OpenAI" : "Anthropic"} API key for model "${model}"`);
  const cacheKey = `${openai ? "openai" : "anthropic"}:${apiKey}`;
  let planner = clients.get(cacheKey);
  if (!planner) {
    planner = openai ? new OpenAiPlanner(apiKey) : new AnthropicPlanner(apiKey);
    clients.set(cacheKey, planner);
  }
  return planner;
}

class AnthropicPlanner implements Planner {
  private readonly client: Anthropic;
  constructor(apiKey: string) {
    this.client = new Anthropic({ apiKey });
  }
  create(params: Anthropic.MessageCreateParamsNonStreaming): Promise<Anthropic.Message> {
    return this.client.messages.create(params) as Promise<Anthropic.Message>;
  }
}

function safeJsonParse(s: string): unknown {
  try {
    return JSON.parse(s);
  } catch {
    return {};
  }
}

/** Anthropic tool_result content is a string or text-block array; flatten to a string. */
function coerceContent(content: unknown): string {
  if (typeof content === "string") return content;
  if (Array.isArray(content))
    return content.map((b: any) => (typeof b === "string" ? b : b?.text ?? "")).join("");
  return content == null ? "" : String(content);
}

function systemText(system: Anthropic.MessageCreateParamsNonStreaming["system"]): string {
  if (!system) return "";
  if (typeof system === "string") return system;
  return system.map((b) => b.text).join("\n");
}

/** Anthropic message thread → Responses `input` items (messages, function_call, function_call_output). */
function toResponsesInput(params: Anthropic.MessageCreateParamsNonStreaming): any[] {
  const input: any[] = [];
  for (const msg of params.messages) {
    if (typeof msg.content === "string") {
      input.push({ role: msg.role, content: msg.content });
      continue;
    }
    for (const block of msg.content) {
      if (block.type === "text") input.push({ role: msg.role, content: block.text });
      else if (block.type === "tool_use")
        input.push({ type: "function_call", call_id: block.id, name: block.name, arguments: JSON.stringify(block.input ?? {}) });
      else if (block.type === "tool_result")
        input.push({ type: "function_call_output", call_id: block.tool_use_id, output: coerceContent(block.content) });
    }
  }
  return input;
}

function toResponsesTools(tools: Anthropic.MessageCreateParamsNonStreaming["tools"]): any[] | undefined {
  if (!tools || tools.length === 0) return undefined;
  return tools.map((t: any) => ({ type: "function", name: t.name, description: t.description, parameters: t.input_schema, strict: false }));
}

class OpenAiPlanner implements Planner {
  private readonly client: OpenAI;
  constructor(apiKey: string) {
    this.client = new OpenAI({ apiKey });
  }

  // Responses API supports function tools; run at minimal reasoning (near-zero reasoning tokens) and rely on
  // preamble messages — the model narrates each step in text, which the loop logs as `thinks:`.
  async create(params: Anthropic.MessageCreateParamsNonStreaming): Promise<Anthropic.Message> {
    const tools = toResponsesTools(params.tools);
    const res: any = await this.client.responses.create({
      model: params.model,
      instructions: systemText(params.system) || undefined,
      input: toResponsesInput(params),
      ...(tools ? { tools, tool_choice: "auto" } : {}),
      reasoning: { effort: "minimal" },
      max_output_tokens: params.max_tokens + 512, // room for the preamble alongside the tool call
    } as any);

    const content: any[] = [];
    for (const item of res.output ?? []) {
      if (item.type === "message") {
        const text = (item.content ?? []).filter((c: any) => c.type === "output_text").map((c: any) => c.text).join("").trim();
        if (text) content.push({ type: "text", text, citations: null });
      } else if (item.type === "function_call") {
        content.push({ type: "tool_use", id: item.call_id, name: item.name, input: safeJsonParse(item.arguments) });
      }
    }

    // OpenAI folds cached tokens into input_tokens; subtract them to match Anthropic's usage semantics.
    const cached = res.usage?.input_tokens_details?.cached_tokens ?? 0;
    return {
      id: res.id,
      type: "message",
      role: "assistant",
      model: res.model,
      content,
      stop_reason: content.some((c) => c.type === "tool_use") ? "tool_use" : "end_turn",
      usage: {
        input_tokens: Math.max(0, (res.usage?.input_tokens ?? 0) - cached),
        output_tokens: res.usage?.output_tokens ?? 0,
        cache_read_input_tokens: cached,
      },
    } as unknown as Anthropic.Message;
  }
}
