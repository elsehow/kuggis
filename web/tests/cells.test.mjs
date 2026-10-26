import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, mkdir, writeFile, readFile, rm, readdir, stat } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { createPiHttpClient, PiThreadController } from "@assistant-ui/react-pi";
const root = fileURLToPath(new URL("../../", import.meta.url));
const binary = join(root, "cell/target/debug/cell");
const fixture = join(root, "web/tests/fake-pi.mjs");
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, label) { const deadline = Date.now() + 12000; while (Date.now() < deadline) { if (await fn()) return; await wait(20); } throw new Error(`Timed out: ${label}`); }
async function launch(args, ready) {
  const child = spawn(binary, args, { env: { ...process.env, KUGGIS_FIXTURE_PROVIDER: "anthropic" } });
  let log = "";
  child.stderr.on("data", (b) => { log += b; });
  child.on("error", (e) => { log += e.message; });
  await until(() => ready.test(log) || child.exitCode !== null, "process startup");
  assert.equal(child.exitCode, null, log);
  return { log: () => log, async stop() { if (child.exitCode !== null || child.signalCode !== null) return; child.kill("SIGTERM"); try { await until(() => child.exitCode !== null || child.signalCode !== null, "shutdown"); } catch (e) { child.kill("SIGKILL"); throw e; } } };
}
const request = (url, method, body) => fetch(url, { method, headers: { "Content-Type": "application/json" }, ...(body !== undefined ? { body: JSON.stringify(body) } : {}) });

async function beginLogin(base, provider, method) {
  const response = await request(`${base}/settings/providers/${provider}/login`, "POST", { method });
  assert.equal(response.status, 200, await response.clone().text());
  const { id } = await response.json();
  const path = `${base}/settings/providers/${provider}/login/${id}`;
  let flow;
  await until(async () => { flow = await (await fetch(path)).json(); return flow.state !== "starting"; }, "Pi auth prompt");
  return { path, flow };
}
async function saveApiKey(base, provider, value) {
  const { path, flow } = await beginLogin(base, provider, "api_key");
  assert.equal(flow.prompt.secret, true);
  assert.equal((await request(path, "POST", { id: flow.prompt.id, value })).status, 204);
  await until(async () => (await (await fetch(path)).json()).state === "complete", "Pi saves credential");
}
async function chooseLogin(path, flow, method) {
  assert.equal(flow.prompt.kind, "select");
  assert.ok(flow.prompt.options.some(o => o.value === method));
  assert.equal((await request(path, "POST", { id: flow.prompt.id, value: method })).status, 204);
  let next;
  await until(async () => { next = await (await fetch(path)).json(); return next.state === "waiting" && next.notice !== null; }, "selected Pi login method");
  return next;
}

test("a pre-dispatch permission failure remains known after restart", { timeout: 20000 }, async (t) => {
  const dir = await mkdtemp("/tmp/kuggis-denied-");
  for (const name of ["inbox", "outbox", "sockets", "tree"]) await mkdir(join(dir, name));
  const file = join(dir, "tree/denied.json");
  await writeFile(file, JSON.stringify({ name: "denied", grants: [] }));
  const args = ["chat", "--cell", file, "--workspace", dir, "--state", join(dir, "state"), "--socket", join(dir, "sockets/denied.sock"), "--agent-dir", join(dir, "auth"), "--pi", fixture, "--outbox", join(dir, "outbox"), "--inbox", join(dir, "inbox"), "--shared", join(dir, "shared")];
  let cell = await launch(args, /Kuggis cell/);
  const ui = await launch(["ui", join(dir, "tree"), "--state", join(dir, "ui"), "--socket-dir", join(dir, "sockets"), "--listen", "127.0.0.1:0"], /Kuggis UI/);
  t.after(async () => { await ui.stop(); await cell.stop(); await rm(dir, { recursive: true, force: true }); });
  const url = ui.log().match(/Kuggis UI: (http:\/\/\S+)/)[1];
  const base = `${url}/api/cells/denied`;
  assert.equal((await (await fetch(`${base}/health`)).json()).state, "ready");
  assert.equal((await fetch(`${url}/api/cells/missing/health`)).status, 404);
  assert.equal((await request(`${base}/recovery`, "POST", { action: "start" })).status, 409);
  const created = await (await request(`${base}/pi/threads`, "POST", {})).json();
  const id = created.metadata.id;
  const body = { request: "a".repeat(32), session: { cell: "denied", id }, action: { prompt: { content: "must not dispatch" } } };
  for (let attempt = 0; attempt < 2; attempt++) {
    const response = await request(`${base}/sessions/${id}/commands`, "POST", body);
    assert.equal(response.status, 403);
    assert.equal((await response.json()).submission.kind, "not_dispatched");
    if (!attempt) { await cell.stop(); cell = await launch(args, /Kuggis cell/); }
  }
  assert.equal((await (await fetch(`${base}/pi/threads/${id}`)).json()).messages.length, 0);
  await cell.stop();
  const health = await (await fetch(`${base}/health`)).json();
  assert.equal(health.state, "unreachable");
  assert.equal(health.recovery, null);
  assert.match(health.message, /Start this local chat service/);
});

test("cell tree, isolated sessions, credentials, connections and gateway restart", { timeout: 45000 }, async (t) => {
  const previousMask = process.umask(0o077); // Match cell-agent.service.
  t.after(() => process.umask(previousMask));
  const dir = await mkdtemp("/tmp/kuggis-cells-");
  const tree = join(dir, "tree"), sockets = join(dir, "sockets");
  await mkdir(join(tree, "project"), { recursive: true }); await mkdir(sockets);
  const grants = [{ provider: "anthropic" }, { host: "tools.example.com" }];
  await writeFile(join(tree, "project.json"), JSON.stringify({ name: "project", grants }));
  await writeFile(join(tree, "project", "project-dev.json"), JSON.stringify({ name: "project-dev", grants }));
  const processes = [], controllers = [];
  t.after(async () => { for (const c of controllers) c.dispose(); for (const p of processes.reverse()) await p.stop(); await rm(dir, { recursive: true, force: true }); });
  for (const name of ["project", "project-dev"]) {
    const home = join(dir, name); await mkdir(home); await mkdir(join(home, "outbox")); await mkdir(join(home, "inbox"));
    const file = name === "project" ? join(tree, "project.json") : join(tree, "project", "project-dev.json");
    processes.push(await launch(["chat", "--cell", file, "--workspace", home, "--state", join(home, "state"), "--socket", join(sockets, `${name}.sock`), "--agent-dir", join(home, "auth"), "--pi", fixture, "--outbox", join(home, "outbox"), "--inbox", join(home, "inbox"), "--shared", join(home, "shared")], /Kuggis cell/));
  }
  // Exercise the exact stdio transport used by the remote SSH command.
  const relay = spawn(binary, ["relay", "--socket", join(sockets, "project-dev.sock")]);
  t.after(() => relay.kill());
  let relayed = "", relayError = "";
  relay.stdout.on("data", (b) => { relayed += b; }); relay.stderr.on("data", (b) => { relayError += b; });
  relay.stdin.write("GET /api/info HTTP/1.1\r\nHost: cell.internal\r\n\r\n");
  await until(() => relayed.includes('"cell":"project-dev"'), "relay flushes a keep-alive response");
  relay.stdin.write("GET /api/info HTTP/1.1\r\nHost: cell.internal\r\nConnection: close\r\n\r\n");
  await until(() => relay.exitCode !== null, "relay exits when the server closes with stdin still open");
  assert.equal(relay.exitCode, 0, relayError);
  assert.equal((relayed.match(/"cell":"project-dev"/g) ?? []).length, 2);
  const startUi = async () => {
    const p = await launch(["ui", tree, "--state", join(dir, "ui"), "--socket-dir", sockets, "--listen", "127.0.0.1:0", "--assets", join(root, "web/dist")], /Kuggis UI: http:/);
    processes.push(p); return { ...p, url: p.log().match(/Kuggis UI: (http:\/\/\S+)/)[1] };
  };
  let ui = await startUi();
  const inventory = await (await fetch(`${ui.url}/api/cells`)).json();
  assert.equal(inventory.mode, "local");
  assert.deepEqual(inventory.cells.map(({ name, parent }) => [name, parent]), [["project", null], ["project-dev", "project"]]);
  assert.equal((await fetch(`${ui.url}/api/cells`, { headers: { Origin: "https://evil.example" } })).status, 403);
  assert.equal((await fetch(`${ui.url}/api/cells/missing/pi/threads`)).status, 404);
  const base = () => `${ui.url}/api/cells/project-dev`;
  const client = () => createPiHttpClient({ baseUrl: `${base()}/pi`, streamCloseDelayMs: 0 });
  await saveApiKey(base(), "anthropic", "test-secret");
  let settings = await (await fetch(`${base()}/settings`)).json();
  assert.deepEqual(settings.providers[0].credential, { state: "stored", method: "api_key" });
  assert.equal(settings.providers[0].authentication.state, "unchecked");
  assert.equal(settings.providers[0].account.state, "unchecked");
  assert.equal((await request(`${base()}/settings/providers/anthropic/check`, "POST")).status, 204);
  settings = await (await fetch(`${base()}/settings`)).json();
  assert.equal(settings.providers[0].authentication.state, "resolved");
  assert.equal(settings.providers[0].authentication.method, "api_key");
  assert.equal(settings.providers[0].account.state, "unchecked", "resolving an API key does not verify an account");
  assert.ok(!JSON.stringify(settings).includes("test-secret"));
  assert.equal((await request(`${base()}/settings/providers/openai/login`, "POST", { method: "oauth" })).status, 403);
  assert.equal((await request(`${base()}/settings/connections`, "PUT", [{ name: "bigbrain", url: "https://ungranted.example/mcp" }])).status, 403);
  assert.equal((await request(`${base()}/settings/connections`, "PUT", [{ name: "bigbrain", url: "https://tools.example.com/mcp", credential: { operation: "replace", value: "secret-mcp" } }])).status, 204);
  settings = await (await fetch(`${base()}/settings`)).json();
  assert.deepEqual(settings.connections, [{ name: "bigbrain", url: "https://tools.example.com/mcp", auth: { method: "bearer", credential: "bigbrain" } }]);
  assert.ok(!JSON.stringify(settings).includes("secret-mcp"));
  const { path: loginPath, flow: choices } = await beginLogin(base(), "anthropic", "oauth");
  const login = await chooseLogin(loginPath, choices, "browser");
  assert.match(login.notice.url, /^https:\/\/claude.ai\/oauth\/authorize\?/);
  assert.equal((await request(loginPath, "POST", { id: "stale", value: "code" })).status, 400);
  assert.equal((await request(loginPath.replace("anthropic", "openai"), "GET")).status, 400, "login belongs to its provider");
  assert.equal((await request(loginPath, "DELETE")).status, 204);
  await until(async () => (await (await fetch(loginPath)).json()).state === "cancelled", "login cancelled");
  const created = await client().createThread(); const id = created.metadata.id;
  assert.equal(created.metadata.cell, "project-dev");
  assert.equal((await fetch(`${ui.url}/api/cells/project/pi/threads/${id}`)).status, 404);
  await assert.rejects(client().setModel(id, { provider: "fixture", modelId: "test-model" }), /not granted/);
  const c = new PiThreadController(client(), id); controllers.push(c); await c.load(); c.connect();
  await client().sendMessage(id, { content: "slow persistent conversation" });
  await until(() => c.getState().runStatus === "running", "run starts");
  assert.equal((await request(`${base()}/settings/providers/anthropic/login`, "POST", { method: "api_key" })).status, 409);
  // The operator gateway can stop with SSE connected; the cell keeps running.
  await ui.stop(); c.dispose();
  ui = await startUi();
  await until(async () => (await client().getThread(id)).metadata.status === "idle", "cell finishes while gateway restarts");
  const finished = await client().getThread(id);
  assert.equal(finished.messages.filter((m) => m.role === "user").length, 1);
  await client().renameThread(id, "Persistent"); await client().archiveThread(id);
  assert.equal((await client().listThreads()).length, 0);
  assert.equal((await client().listThreads({ includeArchived: true }))[0].title, "Persistent");
  await client().unarchiveThread(id);
  const statements = await readdir(join(dir, "project-dev/outbox"));
  assert.ok(statements.filter((n) => n.endsWith(".json")).length >= 2);
  const statement = JSON.parse(await readFile(join(dir, "project-dev/outbox", statements.find((n) => n.endsWith(".json"))), "utf8"));
  assert.equal(statement.session.session.cell, "project-dev");
  assert.equal(statement.session.session.id, id);
  const states = [];
  for (const name of statements.filter((n) => n.endsWith(".json")).sort()) {
    const path = join(dir, "project-dev/outbox", name);
    assert.equal((await stat(path)).mode & 0o777, 0o644, "separate signer can read outbox statements under umask 0077");
    states.push(JSON.parse(await readFile(path, "utf8")).session.state);
  }
  assert.deepEqual(states, ["working", "done"]);
  await client().sendMessage(id, { content: "[error]" });
  await until(async () => (await client().getThread(id)).metadata.status === "failed", "provider error settles");
  await client().sendMessage(id, { content: "retry after provider error" });
  await until(async () => (await client().getThread(id)).metadata.status === "idle", "new run clears old provider error");
  for (const name of (await readdir(join(dir, "project-dev/outbox"))).filter((n) => n.endsWith(".json"))) {
    const event = JSON.parse(await readFile(join(dir, "project-dev/outbox", name), "utf8")).session;
    if (event.state === "working") assert.equal(event.text, undefined, "new run must not report a stale failure");
  }
  await client().sendMessage(id, { content: "[extra-usage]" });
  await until(async () => (await client().getThread(id)).metadata.status === "failed", "extra usage rejection");
  settings = await (await fetch(`${base()}/settings`)).json();
  assert.equal(settings.providers[0].credential.state, "stored");
  assert.equal(settings.providers[0].last_request.outcome.state, "failed");
  assert.equal(settings.providers[0].last_request.outcome.failure.kind, "extra_usage");
  assert.equal(settings.providers[0].authentication.state, "resolved", "inference failure does not mean missing sign-in");
  assert.ok(!JSON.stringify(settings).includes("test-secret"));
  const forbidden = await request(`${base()}/plan`, "POST", { grants: [...grants, { host: "wider.example" }] });
  assert.equal(forbidden.status, 400);
  assert.deepEqual(await forbidden.json(), {
    error: "You must add these hosts to parent cell project. (Children cannot have more access than their parents).",
  });
  assert.equal((await request(`${base()}/settings/providers/anthropic`, "DELETE")).status, 204);
  settings = await (await fetch(`${base()}/settings`)).json();
  assert.equal(settings.providers[0].credential.state, "missing");
  assert.equal(settings.providers[0].authentication.state, "unchecked");
  assert.equal(settings.providers[0].last_request, null, "old observations are not attached to a removed credential");
});

test("Pi offers browser and code login, plus API-key-only providers", { timeout: 25000 }, async t => {
  const dir = await mkdtemp("/tmp/kuggis-openai-");
  for (const name of ["inbox", "outbox", "sockets", "tree"]) await mkdir(join(dir, name));
  const file = join(dir, "tree/project.json");
  await writeFile(file, JSON.stringify({ name: "project", grants: [{ provider: "openai" }, { provider: "paperinstruments" }] }));
  const cell = await launch(["chat", "--cell", file, "--workspace", dir, "--state", join(dir, "state"), "--socket", join(dir, "sockets/project.sock"), "--agent-dir", join(dir, "auth"), "--pi", fixture, "--outbox", join(dir, "outbox"), "--inbox", join(dir, "inbox"), "--shared", join(dir, "shared")], /Kuggis cell/);
  const ui = await launch(["ui", join(dir, "tree"), "--state", join(dir, "ui"), "--socket-dir", join(dir, "sockets"), "--listen", "127.0.0.1:0"], /Kuggis UI/);
  t.after(async () => { await ui.stop(); await cell.stop(); await rm(dir, { recursive: true, force: true }); });
  const base = `${ui.log().match(/Kuggis UI: (http:\/\/\S+)/)[1]}/api/cells/project`;
  const { path, flow: choices } = await beginLogin(base, "openai", "oauth");
  assert.equal((await request(path, "POST", { id: choices.prompt.id, value: "invented-method" })).status, 400);
  const flow = await chooseLogin(path, choices, "device_code");
  assert.deepEqual(flow.notice, { kind: "device_code", url: "https://auth.openai.com/codex/device", code: "ABCD-EFGH" });
  assert.equal(flow.prompt, null);
  assert.equal((await request(path, "POST", { id: "invented", value: "code" })).status, 400);
  assert.equal((await request(`${base}/settings/providers/openai/login`, "POST", { method: "api_key" })).status, 400);
  assert.equal((await request(path, "DELETE")).status, 204);
  await until(async () => (await (await fetch(path)).json()).state === "cancelled", "device flow cancelled");
  await assert.rejects(readFile(join(dir, "auth/auth.json")), { code: "ENOENT" });
  const browser = await beginLogin(base, "openai", "oauth");
  let browserFlow = await chooseLogin(browser.path, browser.flow, "browser");
  assert.match(browserFlow.notice.url, /^https:\/\/auth.openai.com\/oauth\/authorize/);
  await until(async () => {
    browserFlow = await (await fetch(browser.path)).json();
    return browserFlow.prompt !== null;
  }, "browser login input prompt");
  assert.equal(browserFlow.prompt.kind, "input");
  assert.equal((await request(browser.path, "POST", { id: browserFlow.prompt.id, value: "fixture#state" })).status, 204);
  await until(async () => (await (await fetch(browser.path)).json()).state === "complete", "browser login completes");
  assert.equal((await request(`${base}/settings/providers/openai`, "DELETE")).status, 204);
  await saveApiKey(base, "openai", "openai-fixture-secret");
  let settings = await (await fetch(`${base}/settings`)).json();
  assert.deepEqual(settings.providers.find(p => p.id === "openai").credential, { state: "stored", method: "api_key" });
  assert.ok(!JSON.stringify(settings).includes("openai-fixture-secret"));
  assert.equal((await request(`${base}/settings/providers/openai`, "DELETE")).status, 204);
  settings = await (await fetch(`${base}/settings`)).json();
  assert.equal(settings.providers.find(p => p.id === "openai").credential.state, "missing");
  assert.deepEqual(settings.providers.find(p => p.id === "paperinstruments").login_methods, ["api_key"]);
  assert.deepEqual(settings.providers.find(p => p.id === "anthropic").login_methods, ["oauth", "api_key"]);
  assert.equal((await request(`${base}/settings/providers/paperinstruments/login`, "POST", { method: "oauth" })).status, 400);
  await saveApiKey(base, "paperinstruments", "paper-fixture-secret");
  settings = await (await fetch(`${base}/settings`)).json();
  assert.deepEqual(settings.providers.find(p => p.id === "paperinstruments").credential, { state: "stored", method: "api_key" });
  assert.ok(!JSON.stringify(settings).includes("paper-fixture-secret"));
  assert.equal((await request(`${base}/settings/providers/paperinstruments`, "DELETE")).status, 204);
  const models = JSON.parse(await readFile(join(dir, "auth/models.json"), "utf8"));
  assert.equal(models.providers.paperinstruments.models[0].id, "ultramarine-1-flash");
});
