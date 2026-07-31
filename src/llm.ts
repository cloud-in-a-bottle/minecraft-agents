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

function toOpenAiMessages(params: Anthropic.MessageCreateParamsNonStreaming): any[] {
  const out: any[] = [];
  const sys = systemText(params.system);
  if (sys) out.push({ role: "system", content: sys });
  for (const msg of params.messages) {
    if (msg.role === "assistant") {
      if (typeof msg.content === "string") {
        out.push({ role: "assistant", content: msg.content });
        continue;
      }
      let text = "";
      const toolCalls: any[] = [];
      for (const block of msg.content) {
        if (block.type === "text") text += block.text;
        else if (block.type === "tool_use")
          toolCalls.push({
            id: block.id,
            type: "function",
            function: { name: block.name, arguments: JSON.stringify(block.input ?? {}) },
          });
      }
      const m: any = { role: "assistant", content: toolCalls.length ? text || null : text };
      if (toolCalls.length) m.tool_calls = toolCalls;
      out.push(m);
    } else {
      if (typeof msg.content === "string") {
        out.push({ role: "user", content: msg.content });
        continue;
      }
      // Emit tool messages first, then user text, so every tool_call is answered before other messages.
      const texts: any[] = [];
      for (const block of msg.content) {
        if (block.type === "tool_result")
          out.push({ role: "tool", tool_call_id: block.tool_use_id, content: coerceContent(block.content) });
        else if (block.type === "text") texts.push({ role: "user", content: block.text });
      }
      out.push(...texts);
    }
  }
  return out;
}

function toOpenAiTools(tools: Anthropic.MessageCreateParamsNonStreaming["tools"]): any[] | undefined {
  if (!tools || tools.length === 0) return undefined;
  return tools.map((t: any) => ({
    type: "function",
    function: { name: t.name, description: t.description, parameters: t.input_schema },
  }));
}

class OpenAiPlanner implements Planner {
  private readonly client: OpenAI;
  constructor(apiKey: string) {
    this.client = new OpenAI({ apiKey });
  }

  async create(params: Anthropic.MessageCreateParamsNonStreaming): Promise<Anthropic.Message> {
    const messages = toOpenAiMessages(params);
    const tools = toOpenAiTools(params.tools);
    const res = await this.client.chat.completions.create({
      model: params.model,
      messages,
      ...(tools ? { tools, tool_choice: "auto" } : {}),
      // Reasoning models default effort to non-none, which chat.completions rejects alongside function tools.
      reasoning_effort: "none",
      max_completion_tokens: params.max_tokens,
    } as any);

    const choice = res.choices[0].message;
    const content: any[] = [];
    if (typeof choice.content === "string" && choice.content)
      content.push({ type: "text", text: choice.content, citations: null });
    for (const tc of choice.tool_calls ?? [])
      content.push({
        type: "tool_use",
        id: tc.id,
        name: (tc as any).function.name,
        input: safeJsonParse((tc as any).function.arguments),
      });

    // OpenAI folds cached tokens into prompt_tokens; subtract them so input_tokens
    // means non-cached input, matching Anthropic's usage semantics.
    const cached = res.usage?.prompt_tokens_details?.cached_tokens ?? 0;
    const prompt = res.usage?.prompt_tokens ?? 0;
    return {
      id: res.id,
      type: "message",
      role: "assistant",
      model: res.model,
      content,
      stop_reason: choice.tool_calls?.length ? "tool_use" : "end_turn",
      usage: {
        input_tokens: Math.max(0, prompt - cached),
        output_tokens: res.usage?.completion_tokens ?? 0,
        cache_read_input_tokens: cached,
      },
    } as unknown as Anthropic.Message;
  }
}
