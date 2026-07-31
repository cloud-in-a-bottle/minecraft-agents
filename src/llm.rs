//! Planner layer (port of llm.ts). Provider-agnostic request/response over hand-rolled HTTP.
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

/// One block of message content. Provider-neutral; translated per-backend.
#[derive(Clone, Debug)]
pub enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: String },
}

#[derive(Clone, Debug)]
pub struct Message {
    pub role: String, // "user" | "assistant"
    pub content: Vec<ContentBlock>,
}

#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
}

/// `effort: None` and `thinking_disabled: false` mean omit those params (agent.rs self-heals here).
#[derive(Clone, Debug)]
pub struct PlanRequest {
    pub model: String,
    pub system: String,
    pub tools: Vec<ToolDef>,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    pub effort: Option<String>,
    /// true = server rejected `thinking`, so omit it.
    pub thinking_disabled: bool,
}

#[derive(Clone, Debug)]
pub struct PlanResponse {
    pub content: Vec<ContentBlock>,
    pub usage: Usage,
}

#[async_trait]
pub trait Planner: Send + Sync {
    async fn create(&self, req: &PlanRequest) -> Result<PlanResponse>;
}

pub fn is_openai_model(model: &str) -> bool {
    let m = model.to_lowercase();
    m.starts_with("gpt") || m.contains("luna")
}

pub fn planner_for(model: &str, anthropic_key: &str, openai_key: &str) -> Result<Arc<dyn Planner>> {
    let openai = is_openai_model(model);
    let api_key = if openai { openai_key } else { anthropic_key };
    if api_key.is_empty() {
        return Err(anyhow!(
            "missing {} API key for model \"{}\"",
            if openai { "OpenAI" } else { "Anthropic" },
            model
        ));
    }
    Ok(if openai {
        Arc::new(OpenAiPlanner::new(api_key))
    } else {
        Arc::new(AnthropicPlanner::new(api_key))
    })
}

fn safe_json_parse(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| json!({}))
}

// --- Anthropic ---

struct AnthropicPlanner {
    api_key: String,
    http: reqwest::Client,
}

impl AnthropicPlanner {
    fn new(api_key: &str) -> Self {
        Self { api_key: api_key.to_string(), http: reqwest::Client::new() }
    }
}

fn to_anthropic_blocks(content: &[ContentBlock]) -> Vec<Value> {
    content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => json!({ "type": "text", "text": text }),
            ContentBlock::ToolUse { id, name, input } => {
                json!({ "type": "tool_use", "id": id, "name": name, "input": input })
            }
            ContentBlock::ToolResult { tool_use_id, content } => {
                json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": content })
            }
        })
        .collect()
}

#[async_trait]
impl Planner for AnthropicPlanner {
    async fn create(&self, req: &PlanRequest) -> Result<PlanResponse> {
        // One cached system block caches tools+system.
        let system = json!([{
            "type": "text",
            "text": req.system,
            "cache_control": { "type": "ephemeral" },
        }]);
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.input_schema }))
            .collect();
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| json!({ "role": m.role, "content": to_anthropic_blocks(&m.content) }))
            .collect();

        let mut body = json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "system": system,
            "tools": tools,
            "messages": messages,
        });
        let obj = body.as_object_mut().unwrap();
        if !req.thinking_disabled {
            obj.insert("thinking".into(), json!({ "type": "disabled" }));
        }
        if let Some(effort) = &req.effort {
            obj.insert("output_config".into(), json!({ "effort": effort }));
        }

        let resp = self
            .http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow!("Anthropic API {}: {}", status, text));
        }
        let v: Value = serde_json::from_str(&text)?;

        let mut content = Vec::new();
        for item in v.get("content").and_then(Value::as_array).into_iter().flatten() {
            match item.get("type").and_then(Value::as_str) {
                Some("text") => content.push(ContentBlock::Text {
                    text: item.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                }),
                Some("tool_use") => content.push(ContentBlock::ToolUse {
                    id: item.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                    name: item.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    input: item.get("input").cloned().unwrap_or_else(|| json!({})),
                }),
                _ => {}
            }
        }
        let u = v.get("usage");
        let usage = Usage {
            input_tokens: u.and_then(|u| u.get("input_tokens")).and_then(Value::as_u64).unwrap_or(0),
            output_tokens: u.and_then(|u| u.get("output_tokens")).and_then(Value::as_u64).unwrap_or(0),
            cache_read_input_tokens: u
                .and_then(|u| u.get("cache_read_input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        };
        crate::stats::record_llm(usage.input_tokens + usage.output_tokens);
        Ok(PlanResponse { content, usage })
    }
}

// --- OpenAI (Responses API) ---

struct OpenAiPlanner {
    api_key: String,
    http: reqwest::Client,
}

impl OpenAiPlanner {
    fn new(api_key: &str) -> Self {
        Self { api_key: api_key.to_string(), http: reqwest::Client::new() }
    }
}

/// Message thread → Responses `input` items.
fn to_responses_input(messages: &[Message]) -> Vec<Value> {
    let mut input = Vec::new();
    for msg in messages {
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => input.push(json!({ "role": msg.role, "content": text })),
                ContentBlock::ToolUse { id, name, input: args } => input.push(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": serde_json::to_string(args).unwrap_or_else(|_| "{}".into()),
                })),
                ContentBlock::ToolResult { tool_use_id, content } => input.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": content,
                })),
            }
        }
    }
    input
}

fn to_responses_tools(tools: &[ToolDef]) -> Option<Vec<Value>> {
    if tools.is_empty() {
        return None;
    }
    Some(
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                    "strict": false,
                })
            })
            .collect(),
    )
}

#[async_trait]
impl Planner for OpenAiPlanner {
    async fn create(&self, req: &PlanRequest) -> Result<PlanResponse> {
        let tools = to_responses_tools(&req.tools);
        let mut body = json!({
            "model": req.model,
            "input": to_responses_input(&req.messages),
            "reasoning": { "effort": "none" }, // gpt-5.6-luna dropped "minimal"; "none" is its near-zero setting
            "max_output_tokens": req.max_tokens + 512, // room for preamble alongside the tool call
        });
        let obj = body.as_object_mut().unwrap();
        if !req.system.is_empty() {
            obj.insert("instructions".into(), json!(req.system));
        }
        if let Some(tools) = tools {
            obj.insert("tools".into(), json!(tools));
            obj.insert("tool_choice".into(), json!("auto"));
        }

        let resp = self
            .http
            .post("https://api.openai.com/v1/responses")
            .header("authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow!("OpenAI API {}: {}", status, text));
        }
        let v: Value = serde_json::from_str(&text)?;

        let mut content = Vec::new();
        for item in v.get("output").and_then(Value::as_array).into_iter().flatten() {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    let text: String = item
                        .get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|c| c.get("type").and_then(Value::as_str) == Some("output_text"))
                        .filter_map(|c| c.get("text").and_then(Value::as_str))
                        .collect();
                    let text = text.trim();
                    if !text.is_empty() {
                        content.push(ContentBlock::Text { text: text.to_string() });
                    }
                }
                Some("function_call") => content.push(ContentBlock::ToolUse {
                    id: item.get("call_id").and_then(Value::as_str).unwrap_or("").to_string(),
                    name: item.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    input: safe_json_parse(item.get("arguments").and_then(Value::as_str).unwrap_or("")),
                }),
                _ => {}
            }
        }

        // OpenAI folds cached tokens into input_tokens; subtract to match Anthropic semantics.
        let u = v.get("usage");
        let cached = u
            .and_then(|u| u.get("input_tokens_details"))
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let input = u.and_then(|u| u.get("input_tokens")).and_then(Value::as_u64).unwrap_or(0);
        let usage = Usage {
            input_tokens: input.saturating_sub(cached),
            output_tokens: u.and_then(|u| u.get("output_tokens")).and_then(Value::as_u64).unwrap_or(0),
            cache_read_input_tokens: cached,
        };
        crate::stats::record_llm(usage.input_tokens + usage.output_tokens);
        Ok(PlanResponse { content, usage })
    }
}
