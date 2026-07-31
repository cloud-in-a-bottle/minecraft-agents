//! Secret resolution (port of secrets.ts). Env var for local dev, else the OpenHost secrets service.
use anyhow::{anyhow, Result};
use std::collections::HashMap;

#[derive(serde::Deserialize)]
struct SecretsResponse {
    secrets: Option<HashMap<String, String>>,
}

/// Resolve a secret: its env var if set, else the OpenHost secrets service via the router.
/// Non-required secrets return "" when absent.
async fn resolve_secret(name: &str, required: bool) -> Result<String> {
    if let Ok(direct) = std::env::var(name) {
        if !direct.is_empty() {
            return Ok(direct);
        }
    }

    let router = std::env::var("OPENHOST_ROUTER_URL").ok().filter(|v| !v.is_empty());
    let token = std::env::var("OPENHOST_APP_TOKEN").ok().filter(|v| !v.is_empty());
    let (router, token) = match (router, token) {
        (Some(r), Some(t)) => (r, t),
        _ => {
            if !required {
                return Ok(String::new());
            }
            return Err(anyhow!(
                "{name} unset and OpenHost secrets service unavailable (no OPENHOST_ROUTER_URL / OPENHOST_APP_TOKEN)"
            ));
        }
    };

    let res = reqwest::Client::new()
        .post(format!("{router}/api/services/v2/call/secrets/get"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "keys": [name] }))
        .send()
        .await?;
    if !res.status().is_success() {
        if !required {
            return Ok(String::new());
        }
        return Err(anyhow!(
            "secrets service returned {} — is the {name} grant approved for this app?",
            res.status().as_u16()
        ));
    }
    let data: SecretsResponse = res.json().await?;
    let key = data.secrets.and_then(|m| m.get(name).cloned()).unwrap_or_default();
    if key.is_empty() && required {
        return Err(anyhow!("{name} not present in the OpenHost secrets store"));
    }
    Ok(key)
}

/// Anthropic planner key (required).
pub async fn resolve_api_key() -> Result<String> {
    resolve_secret("ANTHROPIC_API_KEY", true).await
}

/// OpenAI planner key (optional; only needed when an OpenAI model is used).
pub async fn resolve_openai_key() -> Result<String> {
    resolve_secret("OPENAI_API_KEY", false).await
}
