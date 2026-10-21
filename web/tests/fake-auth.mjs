// Fixture for Rust's HTTP/auth-process boundary. Real Pi's extension API and
// credential writer are exercised separately in pi-auth.test.mjs.
import { createInterface } from "node:readline";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
const dir = process.env.PI_CODING_AGENT_DIR;
const provider = process.env.KUGGIS_AUTH_PROVIDER;
const method = process.env.KUGGIS_AUTH_METHOD;
const send = event => process.stdout.write(`${JSON.stringify(event)}\n`);
const notice = value => send({ type: "extension_ui_request", id: "notice", method: "notify", message: JSON.stringify(value) });
const authFile = join(dir, "auth.json");
async function auth() { try { return JSON.parse(await readFile(authFile, "utf8")); } catch (e) { if (e.code === "ENOENT") return {}; throw e; } }
async function save(value) { await mkdir(dir, { recursive: true, mode: 0o700 }); await writeFile(authFile, JSON.stringify(value), { mode: 0o600 }); }
if (method === "logout") {
  const credentials = await auth();
  delete credentials[provider];
  if (provider === "openai") delete credentials["openai-codex"];
  await save(credentials);
  notice({ state: "complete" });
} else if (method === "oauth") {
  send({ type: "extension_ui_request", id: "auth-method", method: "input", title: JSON.stringify({ kind: "select", message: "Select login method", options: [{ value: "browser", label: "Browser login" }, { value: provider === "anthropic" ? "copy_code" : "device_code", label: "Code login" }] }) });
} else {
  send({ type: "extension_ui_request", id: "auth-prompt", method: "input", title: JSON.stringify({ kind: "input", message: method === "api_key" ? "Enter API key" : "Paste authorization code", secret: method === "api_key" }) });
}
createInterface({ input: process.stdin }).on("line", async line => {
  const reply = JSON.parse(line);
  if (reply.type === "extension_ui_response" && reply.id === "auth-method") {
    if (reply.value === "device_code") {
      notice({ state: "notice", notice: { kind: "device_code", url: "https://auth.openai.com/codex/device", code: "ABCD-EFGH" } });
    } else {
      notice({ state: "notice", notice: { kind: "link", url: provider === "anthropic" ? "https://claude.ai/oauth/authorize?fixture=true" : "https://auth.openai.com/oauth/authorize?fixture=true", instructions: "Paste the final redirect URL here." } });
      send({ type: "extension_ui_request", id: "auth-prompt", method: "input", title: JSON.stringify({ kind: "input", message: "Paste authorization code / redirect URL", secret: false }) });
    }
    return;
  }
  if (reply.type !== "extension_ui_response" || reply.id !== "auth-prompt") return;
  if (method === "oauth" && reply.value !== "fixture#state") { notice({ state: "failed", message: "Pi could not complete sign-in. Try again." }); return; }
  const credentials = await auth();
  credentials[provider] = method === "api_key" ? { type: "api_key", key: reply.value } : { type: "oauth", access: "fixture-access", refresh: "fixture-refresh", expires: Date.now() + 3600000 };
  await save(credentials);
  notice({ state: "complete" });
});
