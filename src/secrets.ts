/**
 * Resolve the Anthropic API key: an ANTHROPIC_API_KEY env var (local dev) if set,
 * otherwise the OpenHost secrets service via the router.
 */
export async function resolveApiKey(): Promise<string> {
  const direct = process.env.ANTHROPIC_API_KEY;
  if (direct) return direct;

  const router = process.env.OPENHOST_ROUTER_URL;
  const token = process.env.OPENHOST_APP_TOKEN;
  if (!router || !token) {
    throw new Error("ANTHROPIC_API_KEY unset and OpenHost secrets service unavailable (no OPENHOST_ROUTER_URL / OPENHOST_APP_TOKEN)");
  }

  const res = await fetch(`${router}/api/services/v2/call/secrets/get`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ keys: ["ANTHROPIC_API_KEY"] }),
  });
  if (!res.ok) {
    throw new Error(`secrets service returned ${res.status} — is the ANTHROPIC_API_KEY grant approved for this app?`);
  }
  const data = (await res.json()) as { secrets?: Record<string, string> };
  const key = data.secrets?.ANTHROPIC_API_KEY;
  if (!key) throw new Error("ANTHROPIC_API_KEY not present in the OpenHost secrets store");
  return key;
}
