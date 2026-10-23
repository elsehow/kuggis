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
  const dir = await mkdtemp("/tmp/kuggis-f19ecadf-0ca0-5024-a80a-ba8de0567677-");
  for (const name of ["inbox", "outbox", "sockets", "tree"]) await mkdir(join(dir, name));
  const file = join(dir, "tree/f19ecadf-0ca0-5024-a80a-ba8de0567677.json");
  await writeFile(file, JSON.stringify({ id: "f19ecadf-0ca0-5024-a80a-ba8de0567677", name: "f19ecadf-0ca0-5024-a80a-ba8de0567677", grants: [] }));
  const args = ["chat", "--cell", file, "--workspace", dir, "--state", join(dir, "state"), "--socket", join(dir, "sockets/f19ecadf-0ca0-5024-a80a-ba8de0567677.sock"), "--agent-dir", join(dir, "auth"), "--pi", fixture, "--outbox", join(dir, "outbox"), "--inbox", join(dir, "inbox")];
  let cell = await launch(args, /Kuggis cell/);
  const ui = await launch(["ui", join(dir, "tree"), "--state", join(dir, "ui"), "--socket-dir", join(dir, "sockets"), "--listen", "127.0.0.1:0"], /Kuggis UI/);
  t.after(async () => { await ui.stop(); await cell.stop(); await rm(dir, { recursive: true, force: true }); });
  const url = ui.log().match(/Kuggis UI: (http:\/\/\S+)/)[1];
  const base = `${url}/api/cells/f19ecadf-0ca0-5024-a80a-ba8de0567677`;
  const created = await (await request(`${base}/pi/threads`, "POST", {})).json();
  const id = created.metadata.id;
  const body = { request: "a".repeat(32), session: { cell: "f19ecadf-0ca0-5024-a80a-ba8de0567677", id }, action: { prompt: { content: "must not dispatch" } } };
  for (let attempt = 0; attempt < 2; attempt++) {
    const response = await request(`${base}/sessions/${id}/commands`, "POST", body);
    assert.equal(response.status, 403);
    assert.equal((await response.json()).submission.kind, "not_dispatched");
    if (!attempt) { await cell.stop(); cell = await launch(args, /Kuggis cell/); }
  }
  assert.equal((await (await fetch(`${base}/pi/threads/${id}`)).json()).messages.length, 0);
});

test("cell tree, isolated sessions, credentials, connections and gateway restart", { timeout: 45000 }, async (t) => {
  const previousMask = process.umask(0o077); // Match cell-agent.service.
  t.after(() => process.umask(previousMask));
  const dir = await mkdtemp("/tmp/kuggis-cells-");
  const tree = join(dir, "tree"), sockets = join(dir, "sockets");
  await mkdir(join(tree, "44fc36e4-5adc-51aa-a6e3-3db15ade9819"), { recursive: true }); await mkdir(sockets);
  const grants = [{ provider: "anthropic" }, { host: "tools.example.com" }];
  await writeFile(join(tree, "44fc36e4-5adc-51aa-a6e3-3db15ade9819.json"), JSON.stringify({ id: "44fc36e4-5adc-51aa-a6e3-3db15ade9819", name: "44fc36e4-5adc-51aa-a6e3-3db15ade9819", grants }));
  await writeFile(join(tree, "44fc36e4-5adc-51aa-a6e3-3db15ade9819", "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21.json"), JSON.stringify({ id: "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21", name: "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21", grants }));
  const processes = [], controllers = [];
  t.after(async () => { for (const c of controllers) c.dispose(); for (const p of processes.reverse()) await p.stop(); await rm(dir, { recursive: true, force: true }); });
  for (const name of ["44fc36e4-5adc-51aa-a6e3-3db15ade9819", "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21"]) {
    const home = join(dir, name); await mkdir(home); await mkdir(join(home, "outbox")); await mkdir(join(home, "inbox"));
    const file = name === "44fc36e4-5adc-51aa-a6e3-3db15ade9819" ? join(tree, "44fc36e4-5adc-51aa-a6e3-3db15ade9819.json") : join(tree, "44fc36e4-5adc-51aa-a6e3-3db15ade9819", "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21.json");
    processes.push(await launch(["chat", "--cell", file, "--workspace", home, "--state", join(home, "state"), "--socket", join(sockets, `${name}.sock`), "--agent-dir", join(home, "auth"), "--pi", fixture, "--outbox", join(home, "outbox"), "--inbox", join(home, "inbox")], /Kuggis cell/));
  }
  // Exercise the exact stdio transport used by the remote SSH command.
  const relay = spawn(binary, ["relay", "--socket", join(sockets, "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21.sock")]);
  t.after(() => relay.kill());
  let relayed = "", relayError = "";
  relay.stdout.on("data", (b) => { relayed += b; }); relay.stderr.on("data", (b) => { relayError += b; });
  relay.stdin.write("GET /api/info HTTP/1.1\r\nHost: cell.internal\r\n\r\n");
  await until(() => relayed.includes('"cell":"e95cc7f0-21bb-5212-8fa9-abbfde3f6b21"'), "relay flushes a keep-alive response");
  relay.stdin.write("GET /api/info HTTP/1.1\r\nHost: cell.internal\r\nConnection: close\r\n\r\n");
  await until(() => relay.exitCode !== null, "relay exits when the server closes with stdin still open");
  assert.equal(relay.exitCode, 0, relayError);
  assert.equal((relayed.match(/"cell":"e95cc7f0-21bb-5212-8fa9-abbfde3f6b21"/g) ?? []).length, 2);
  const startUi = async () => {
    const p = await launch(["ui", tree, "--state", join(dir, "ui"), "--socket-dir", sockets, "--listen", "127.0.0.1:0", "--assets", join(root, "web/dist")], /Kuggis UI: http:/);
    processes.push(p); return { ...p, url: p.log().match(/Kuggis UI: (http:\/\/\S+)/)[1] };
  };
  let ui = await startUi();
  const inventory = await (await fetch(`${ui.url}/api/cells`)).json();
  assert.equal(inventory.mode, "local");
  assert.deepEqual(inventory.cells.map(({ name, parent }) => [name, parent]), [["44fc36e4-5adc-51aa-a6e3-3db15ade9819", null], ["e95cc7f0-21bb-5212-8fa9-abbfde3f6b21", "44fc36e4-5adc-51aa-a6e3-3db15ade9819"]]);
  assert.equal((await fetch(`${ui.url}/api/cells`, { headers: { Origin: "https://evil.example" } })).status, 403);
  assert.equal((await fetch(`${ui.url}/api/cells/9f1bc83a-82d5-59ba-9fb5-45088d81526b/pi/threads`)).status, 404);
  const base = () => `${ui.url}/api/cells/e95cc7f0-21bb-5212-8fa9-abbfde3f6b21`;
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
  assert.equal(created.metadata.cell, "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21");
  assert.equal((await fetch(`${ui.url}/api/cells/44fc36e4-5adc-51aa-a6e3-3db15ade9819/pi/threads/${id}`)).status, 404);
  await assert.rejects(client().setModel(id, { provider: "fixture", modelId: "test-model" }), /not granted/);
  const c = new PiThreadController(client(), id); controllers.push(c); await c.load(); c.connect();
  await client().sendMessage(id, { content: "slow persistent conversation" });
  await until(() => c.getState().runStatus === "running", "run starts");
  assert.equal((await request(`${base()}/settings/providers/anthropic/login`, "POST", { method: "api_key" })).status, 409);
  // Renaming a parent and its running child is metadata-only. Addresses, SSE,
  // grants, and credential storage remain attached to the original UUIDs.
  const parentId = "44fc36e4-5adc-51aa-a6e3-3db15ade9819";
  const childId = "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21";
  const credentials = await readFile(join(dir, childId, "auth/auth.json"));
  assert.equal((await request(`${ui.url}/api/cells/${parentId}/rename`, "POST", { name: "renamed-project" })).status, 204);
  assert.equal((await request(`${base()}/rename`, "POST", { name: "renamed-worker" })).status, 204);
  assert.equal((await request(`${base()}/rename`, "POST", { name: "renamed-project" })).status, 409);
  assert.equal((await request(`${base()}/rename`, "POST", { name: "bad", id: parentId })).status, 422);
  assert.equal((await request(`${base()}/rename`, "POST", { name: "../escape" })).status, 422);
  const renamed = (await (await fetch(`${ui.url}/api/cells`)).json()).cells;
  assert.deepEqual(renamed.map(({ id, name, parent }) => [id, name, parent]), [[parentId, "renamed-project", null], [childId, "renamed-worker", parentId]]);
  assert.deepEqual(await readFile(join(dir, childId, "auth/auth.json")), credentials);
  assert.equal((await client().getThread(id)).metadata.cell, childId);
  assert.deepEqual(JSON.parse(await readFile(join(tree, parentId, `${childId}.json`), "utf8")), { id: childId, name: "renamed-worker", grants });
  // The operator gateway can stop with SSE connected; the cell keeps running.
  await ui.stop(); c.dispose();
  ui = await startUi();
  assert.equal((await (await fetch(`${ui.url}/api/cells`)).json()).cells.find(c => c.id === childId).name, "renamed-worker");
  await until(async () => (await client().getThread(id)).metadata.status === "idle", "cell finishes while gateway restarts");
  const finished = await client().getThread(id);
  assert.equal(finished.messages.filter((m) => m.role === "user").length, 1);
  await client().renameThread(id, "Persistent"); await client().archiveThread(id);
  assert.equal((await client().listThreads()).length, 0);
  assert.equal((await client().listThreads({ includeArchived: true }))[0].title, "Persistent");
  await client().unarchiveThread(id);
  const statements = await readdir(join(dir, "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21/outbox"));
  assert.ok(statements.filter((n) => n.endsWith(".json")).length >= 2);
  const statement = JSON.parse(await readFile(join(dir, "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21/outbox", statements.find((n) => n.endsWith(".json"))), "utf8"));
  assert.equal(statement.session.session.cell, "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21");
  assert.equal(statement.session.session.id, id);
  const states = [];
  for (const name of statements.filter((n) => n.endsWith(".json")).sort()) {
    const path = join(dir, "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21/outbox", name);
    assert.equal((await stat(path)).mode & 0o777, 0o644, "separate signer can read outbox statements under umask 0077");
    states.push(JSON.parse(await readFile(path, "utf8")).session.state);
  }
  assert.deepEqual(states, ["working", "done"]);
  await client().sendMessage(id, { content: "[error]" });
  await until(async () => (await client().getThread(id)).metadata.status === "failed", "provider error settles");
  await client().sendMessage(id, { content: "retry after provider error" });
  await until(async () => (await client().getThread(id)).metadata.status === "idle", "new run clears old provider error");
  for (const name of (await readdir(join(dir, "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21/outbox"))).filter((n) => n.endsWith(".json"))) {
    const event = JSON.parse(await readFile(join(dir, "e95cc7f0-21bb-5212-8fa9-abbfde3f6b21/outbox", name), "utf8")).session;
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
  assert.match(await forbidden.text(), /not covered by its parent/);
  assert.equal((await request(`${base()}/settings/providers/anthropic`, "DELETE")).status, 204);
  settings = await (await fetch(`${base()}/settings`)).json();
  assert.equal(settings.providers[0].credential.state, "missing");
  assert.equal(settings.providers[0].authentication.state, "unchecked");
  assert.equal(settings.providers[0].last_request, null, "old observations are not attached to a removed credential");
});

test("Pi offers browser and code login, plus API-key-only providers", { timeout: 25000 }, async t => {
  const dir = await mkdtemp("/tmp/kuggis-openai-");
  for (const name of ["inbox", "outbox", "sockets", "tree"]) await mkdir(join(dir, name));
  const file = join(dir, "tree/44fc36e4-5adc-51aa-a6e3-3db15ade9819.json");
  await writeFile(file, JSON.stringify({ id: "44fc36e4-5adc-51aa-a6e3-3db15ade9819", name: "44fc36e4-5adc-51aa-a6e3-3db15ade9819", grants: [{ provider: "openai" }, { provider: "paperinstruments" }] }));
  const cell = await launch(["chat", "--cell", file, "--workspace", dir, "--state", join(dir, "state"), "--socket", join(dir, "sockets/44fc36e4-5adc-51aa-a6e3-3db15ade9819.sock"), "--agent-dir", join(dir, "auth"), "--pi", fixture, "--outbox", join(dir, "outbox"), "--inbox", join(dir, "inbox")], /Kuggis cell/);
  const ui = await launch(["ui", join(dir, "tree"), "--state", join(dir, "ui"), "--socket-dir", join(dir, "sockets"), "--listen", "127.0.0.1:0"], /Kuggis UI/);
  t.after(async () => { await ui.stop(); await cell.stop(); await rm(dir, { recursive: true, force: true }); });
  const base = `${ui.log().match(/Kuggis UI: (http:\/\/\S+)/)[1]}/api/cells/44fc36e4-5adc-51aa-a6e3-3db15ade9819`;
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
  const browserFlow = await chooseLogin(browser.path, browser.flow, "browser");
  assert.match(browserFlow.notice.url, /^https:\/\/auth.openai.com\/oauth\/authorize/);
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

test("renaming in settings preserves the selected chat and unsent draft", { timeout: 30000, skip: !process.env.PUBLISH_SMOKE_PLAYWRIGHT }, async (t) => {
  const { createRequire } = await import("node:module");
  const { chromium } = createRequire(import.meta.url)(process.env.PUBLISH_SMOKE_PLAYWRIGHT);
  const dir = await mkdtemp("/tmp/kuggis-rename-ui-");
  const cellId = "00000000-0000-4000-8000-000000000011";
  for (const name of ["tree", "sockets", "inbox", "outbox"]) await mkdir(join(dir, name));
  const declaration = join(dir, "tree", `${cellId}.json`);
  await writeFile(declaration, JSON.stringify({ id: cellId, name: "old-worker", grants: [] }));
  const processes = [];
  let browser;
  t.after(async () => {
    await browser?.close();
    for (const p of processes.reverse()) await p.stop();
    await rm(dir, { recursive: true, force: true });
  });
  processes.push(await launch(["chat", "--cell", declaration, "--workspace", dir, "--state", join(dir, "state"), "--socket", join(dir, "sockets", `${cellId}.sock`), "--agent-dir", join(dir, "auth"), "--pi", fixture, "--outbox", join(dir, "outbox"), "--inbox", join(dir, "inbox")], /Kuggis cell/));
  const ui = await launch(["ui", join(dir, "tree"), "--state", join(dir, "ui"), "--socket-dir", join(dir, "sockets"), "--listen", "127.0.0.1:0", "--assets", join(root, "web/dist")], /Kuggis UI/);
  processes.push(ui);
  const url = ui.log().match(/Kuggis UI: (http:\/\/\S+)/)[1];
  const base = `${url}/api/cells/${cellId}`;
  const thread = await (await request(`${base}/pi/threads`, "POST", { title: "Keep this chat" })).json();
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  await page.goto(`${url}/?cell=${cellId}&chat=${thread.metadata.id}`);
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await composer.fill("Keep this unsent draft");
  const beforeUrl = page.url();
  await page.getByRole("button", { name: "Cell settings" }).click();
  await page.getByRole("textbox", { name: "Cell name", exact: true }).fill("new-worker");
  await page.getByRole("button", { name: "Rename", exact: true }).click();
  await page.getByRole("heading", { name: "new-worker", exact: true }).waitFor();
  await page.getByRole("button", { name: "Close settings" }).click();
  assert.equal(await composer.inputValue(), "Keep this unsent draft");
  assert.equal(page.url(), beforeUrl);
  assert.equal((await (await fetch(`${base}/pi/threads/${thread.metadata.id}`)).json()).metadata.cell, cellId);
  assert.deepEqual(JSON.parse(await readFile(declaration, "utf8")), { id: cellId, name: "new-worker", grants: [] });
});
