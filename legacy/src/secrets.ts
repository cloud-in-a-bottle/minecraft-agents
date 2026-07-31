/**
 * Resolve a secret: its env var (local dev) if set, otherwise the OpenHost
 * secrets service via the router. Non-required secrets return "" when absent.
 */
async function resolveSecret(name: string, required: boolean): Promise<string> {
  const direct = process.env[name];
  if (direct) return direct;

  const router = process.env.OPENHOST_ROUTER_URL;
  const token = process.env.OPENHOST_APP_TOKEN;
  if (!router || !token) {
    if (!required) return "";
    throw new Error(`${name} unset and OpenHost secrets service unavailable (no OPENHOST_ROUTER_URL / OPENHOST_APP_TOKEN)`);
  }

  const res = await fetch(`${router}/api/services/v2/call/secrets/get`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ keys: [name] }),
  });
  if (!res.ok) {
    if (!required) return "";
    throw new Error(`secrets service returned ${res.status} — is the ${name} grant approved for this app?`);
  }
  const data = (await res.json()) as { secrets?: Record<string, string> };
  const key = data.secrets?.[name] ?? "";
  if (!key && required) throw new Error(`${name} not present in the OpenHost secrets store`);
  return key;
}

/** Anthropic planner key (required). */
export function resolveApiKey(): Promise<string> {
  return resolveSecret("ANTHROPIC_API_KEY", true);
}

/** OpenAI planner key (optional; only needed when an OpenAI model is used). */
export function resolveOpenAiKey(): Promise<string> {
  return resolveSecret("OPENAI_API_KEY", false);
}
