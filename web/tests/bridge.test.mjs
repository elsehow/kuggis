import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, rm, chmod, readFile, writeFile, mkdir } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { get } from "node:http";
import { createPiHttpClient, PiThreadController } from "@assistant-ui/react-pi";

const root = fileURLToPath(new URL("../../", import.meta.url));
const fixture = fileURLToPath(new URL("fake-pi.mjs", import.meta.url));
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, label) {
  const deadline = Date.now() + 12000;
  while (Date.now() < deadline) { if (await fn()) return; await wait(20); }
  throw new Error(`Timed out: ${label}`);
}
async function start(state, pi = fixture, env = process.env) {
  const process = spawn(join(root, "cell/target/debug/cell"), ["chat", "--local", root, "--state", state, "--listen", "127.0.0.1:0", "--assets", join(root, "web/dist"), "--pi", pi], { env });
  let output = "";
  process.stderr.on("data", (data) => { output += data; });
  process.on("error", (error) => { output += error.message; });
  await until(() => /Kuggis chat: (http:\/\/[^\s]+)/.test(output) || process.exitCode !== null, "server startup");
  assert.equal(process.exitCode, null, output);
  const url = output.match(/Kuggis chat: (http:\/\/[^\s]+)/)[1];
  const client = createPiHttpClient({ baseUrl: `${url}/api/pi`, streamCloseDelayMs: 0, reconnectDelay: () => wait(25) });
  return { client, url, async stop() {
    if (process.exitCode !== null || process.signalCode !== null) return;
    process.kill("SIGINT");
    try { await until(() => process.exitCode !== null || process.signalCode !== null, "server shutdown"); }
    catch (error) { process.kill("SIGKILL"); throw error; }
  } };
}
const text = (s) => s.messages.filter((m) => m.role === "assistant").flatMap((m) => m.content).filter((c) => c.type === "text").map((c) => c.text).join("\n");
const opaqueId = () => randomUUID().replaceAll("-", "");
const post = (url, body, headers = {}) => fetch(url, { method: "POST", headers: { "Content-Type": "application/json", ...headers }, body: JSON.stringify(body) });
const readJson = async (path) => JSON.parse(await readFile(path, "utf8"));

test("real React Pi client against the Rust bridge", { timeout: 45000 }, async (t) => {
  await chmod(fixture, 0o755);
  const directory = await mkdtemp(join(tmpdir(), "kuggis-chat-"));
  let server = await start(directory);
  const controllers = [];
  t.after(async () => { for (const c of controllers) c.dispose(); await server.stop(); await rm(directory, { recursive: true, force: true }); });
  let { client, url } = server;
  assert.match(await (await fetch(url)).text(), /Kuggis/);
  assert.equal((await fetch(`${url}/api/pi/threads`, { headers: { Origin: "https://evil.example" } })).status, 403);
  const spoofedHostStatus = await new Promise((resolve, reject) => {
    get(`${url}/api/pi/threads`, { headers: { Host: "evil.example" } }, (response) => { response.resume(); resolve(response.statusCode); }).on("error", reject);
  });
  assert.equal(spoofedHostStatus, 403);
  assert.equal((await fetch(`${url}/api/pi/threads`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ workspacePath: "/" }) })).status, 400);
  assert.equal((await client.getAvailableModels()).length, 2);
  const created = await client.createThread();
  const id = created.metadata.id;
  assert.deepEqual(created.readiness.selection, { provider: "fixture", modelId: "test-model" });
  for (const selection of [{ provider: "fixture" }, { modelId: "test-model" }, { provider: "", modelId: "test-model" }]) {
    assert.equal((await post(`${url}/api/pi/threads/${id}/model`, selection)).status, 400);
  }
  assert.equal((await post(`${url}/api/pi/threads/${id}/thinking`, { level: "typo" })).status, 400);
  const controller = new PiThreadController(client, id); controllers.push(controller);
  await controller.load(); const disconnect = controller.connect();
  await client.sendMessage(id, { content: "slow hello \u2028 world" });
  await until(() => text(controller.getState()).includes("I read"), "partial text stream");
  assert.equal(controller.getState().runStatus, "running");
  assert.ok(controller.getState().messages.some((m) => m.role === "toolResult"));
  await controller.refresh();
  disconnect(); controller.dispose();
  // Reconnect in the middle of a partial response. The real adapter must not
  // append a duplicate assistant message after its snapshot reset.
  const recovered = new PiThreadController(client, id); controllers.push(recovered);
  await recovered.load(); recovered.connect();
  await until(() => recovered.getState().runStatus === "idle", "completion after reconnect");
  assert.equal(recovered.getState().messages.filter((m) => m.role === "assistant").length, 2);
  assert.match(text(recovered.getState()), /can be resumed/);

  await client.sendMessage(id, { content: "slow again" });
  await until(() => recovered.getState().runStatus === "running", "second run");
  await client.sendMessage(id, { content: "queued steering", streamingBehavior: "steer" });
  await until(() => recovered.getState().queue.steering.includes("queued steering"), "steering queue");
  assert.deepEqual(await client.clearQueue(id), { steering: ["queued steering"], followUp: [] });
  await client.cancelRun(id);
  await until(() => recovered.getState().runStatus === "idle", "cancelled run");
  assert.equal(recovered.getState().messages.at(-1).stopReason, "aborted");

  await client.sendMessage(id, { content: "[confirm]" });
  await until(() => recovered.getState().hostUiRequests.length === 1, "approval dialog");
  assert.equal((await client.getThread(id)).hostUiRequests.length, 1);
  for (const response of [{ requestId: "approval", value: "yes" }, { requestId: "approval", confirmed: true, value: "yes" }, { requestId: "approval", dismissed: false }]) {
    assert.equal((await post(`${url}/api/pi/threads/${id}/host-ui`, { response })).status, 400);
  }
  assert.equal((await client.getThread(id)).hostUiRequests.length, 1, "invalid replies leave the request pending");
  await client.respondToHostUiRequest(id, { requestId: "approval", confirmed: true });
  await until(() => recovered.getState().runStatus === "idle", "approval run completed");
  assert.match(text(recovered.getState()), /confirmed/);
  await client.setModel(id, { provider: "fixture", modelId: "second-model" });
  await client.setThinkingLevel(id, "low");
  await client.renameThread(id, "Durable chat");
  await client.archiveThread(id);
  assert.deepEqual(await readJson(join(directory, "threads", `${id}.json`)), {
    id, cell: "00000000-0000-4000-8000-000000000000", title: "Durable chat", archived: true,
  });
  assert.equal((await client.listThreads()).length, 0);
  assert.equal((await client.listThreads({ includeArchived: true })).length, 1);
  await client.unarchiveThread(id);
  const before = await client.getThread(id);
  // Shutdown must finish even with active SSE subscribers.
  await server.stop();
  for (const c of controllers) c.dispose();
  server = await start(directory); client = server.client;
  const after = await client.getThread(id);
  assert.equal(after.metadata.title, "Durable chat");
  assert.equal(after.metadata.cell, "00000000-0000-4000-8000-000000000000");
  assert.deepEqual(after.messages, before.messages);
  await client.sendMessage(id, { content: "[crash]" });
  await until(async () => (await client.listThreads())[0].status === "failed", "child exit reported");
  assert.equal((await client.getThread(id)).metadata.status, "idle");
});

test("cell-scoped commands and retry receipts survive restart", { timeout: 30000 }, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "kuggis-commands-"));
  let server = await start(directory);
  t.after(async () => { await server.stop(); await rm(directory, { recursive: true, force: true }); });
  const { metadata: { id } } = await server.client.createThread();
  const command = { request: opaqueId(), session: { cell: "00000000-0000-4000-8000-000000000000", id }, action: { prompt: { content: "once" } } };
  const send = (body) => post(`${server.url}/api/sessions/${id}/commands`, body);
  assert.equal((await send({ ...command, session: { cell: "70ee78ad-5e3a-510a-af20-6f485ecb6f16", id } })).status, 403);
  assert.equal((await send({ ...command, session: { cell: "00000000-0000-4000-8000-000000000000", id: opaqueId() } })).status, 400);
  assert.equal((await send({ ...command, action: { prompt: { content: "   " } } })).status, 400);
  assert.equal((await fetch(`${server.url}/api/pi/threads/not-an-id`)).status, 400);
  assert.equal((await server.client.getThread(id)).messages.length, 0);
  assert.equal((await send(command)).status, 204);
  assert.equal((await send(command)).status, 204);
  await until(async () => (await server.client.getThread(id)).metadata.status === "idle", "first command completes");
  assert.equal((await send({ ...command, action: { prompt: { content: "changed" } } })).status, 409);
  await server.stop();
  server = await start(directory);
  assert.equal((await send(command)).status, 204);
  assert.equal((await server.client.getThread(id)).messages.filter((m) => m.role === "user").length, 1);

  // The assistant-ui route uses the same journal, via a stable request header.
  const messageUrl = `${server.url}/api/pi/threads/${id}/messages`;
  const request = opaqueId();
  const input = { input: { content: "slow second" } };
  assert.equal((await post(messageUrl, input, { "X-Request-ID": request })).status, 204);
  assert.equal((await post(messageUrl, input, { "X-Request-ID": request })).status, 204);
  await until(async () => (await server.client.getThread(id)).metadata.status === "running", "second command starts");
  const cancel = { request: opaqueId(), session: command.session, action: "cancel" };
  assert.equal((await send(cancel)).status, 204);
  await until(async () => (await server.client.getThread(id)).metadata.status === "idle", "cancel settles");
  await server.client.sendMessage(id, { content: "slow third" });
  await until(async () => (await server.client.getThread(id)).metadata.status === "running", "third command starts");
  assert.equal((await send(cancel)).status, 204);
  assert.equal((await server.client.getThread(id)).metadata.status, "running", "replaying an old cancel cannot stop a new run");
  await server.client.cancelRun(id);
  const messages = (await server.client.getThread(id)).messages;
  assert.equal(messages.filter((m) => m.role === "user").length, 3);
});

test("a lost runtime acknowledgement is never automatically resubmitted", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "kuggis-pending-"));
  let server = await start(directory);
  t.after(async () => { await server.stop(); await rm(directory, { recursive: true, force: true }); });
  const { metadata: { id } } = await server.client.createThread();
  const command = { request: opaqueId(), session: { cell: "00000000-0000-4000-8000-000000000000", id }, action: { prompt: { content: "[no-ack]" } } };
  assert.equal((await post(`${server.url}/api/sessions/${id}/commands`, command)).status, 502);
  await server.stop();
  server = await start(directory);
  const retry = await post(`${server.url}/api/sessions/${id}/commands`, command);
  assert.equal(retry.status, 409);
  const failure = await retry.json();
  assert.match(failure.error, /outcome is unknown/);
  assert.equal(failure.submission.kind, "outcome_unknown");
  assert.equal((await server.client.getThread(id)).messages.filter((m) => m.role === "user").length, 1);
});

test("a rejected command stays rejected on replay, independently of a model result", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "kuggis-rejected-"));
  let server = await start(directory);
  t.after(async () => { await server.stop(); await rm(directory, { recursive: true, force: true }); });
  const { metadata: { id } } = await server.client.createThread();
  const command = { request: opaqueId(), session: { cell: "00000000-0000-4000-8000-000000000000", id }, action: { prompt: { content: "[reject]" } } };
  const send = () => post(`${server.url}/api/sessions/${id}/commands`, command);
  let response = await send();
  assert.equal(response.status, 502);
  assert.equal((await response.json()).submission.kind, "rejected");
  await server.stop(); server = await start(directory);
  response = await send();
  assert.equal(response.status, 502);
  assert.equal((await response.json()).submission.kind, "rejected", "known rejection must not become uncertain delivery");
  assert.equal((await server.client.getThread(id)).messages.length, 0);
  const accepted = { ...command, request: opaqueId(), action: { prompt: { content: "[error]" } } };
  assert.equal((await post(`${server.url}/api/sessions/${id}/commands`, accepted)).status, 204);
  await until(async () => (await server.client.getThread(id)).metadata.status === "failed", "model failure");
  assert.equal((await post(`${server.url}/api/sessions/${id}/commands`, accepted)).status, 204, "model rejection cannot replay an accepted prompt");
  assert.equal((await server.client.getThread(id)).messages.filter((m) => m.role === "user").length, 1);
});

test("legacy metadata is backed up and adopted without changing the transcript", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "kuggis-migrate-"));
  await mkdir(join(directory, "threads"));
  await mkdir(join(directory, "pi"));
  const id = opaqueId();
  const path = join(directory, "threads", `${id}.json`);
  const legacy = { id, title: "Previous chat", archived: true, workspacePath: root.replace(/\/$/, ""), status: "running", queuedMessages: [{ content: "stale" }] };
  await writeFile(path, JSON.stringify(legacy));
  const transcript = [{ role: "user", content: "saved", timestamp: 1 }];
  await writeFile(join(directory, "pi", `${id}.fixture.json`), JSON.stringify(transcript));
  const server = await start(directory);
  t.after(async () => { await server.stop(); await rm(directory, { recursive: true, force: true }); });
  assert.deepEqual(await readJson(`${path}.bak`), legacy);
  assert.deepEqual(await readJson(path), { id, cell: "00000000-0000-4000-8000-000000000000", title: legacy.title, archived: true });
  const restored = await server.client.getThread(id);
  assert.deepEqual(restored.messages, transcript);
  assert.equal(restored.metadata.status, "idle");
  assert.deepEqual(restored.metadata.queuedMessages, []);
  await server.stop();
  const foreign = { id, cell: "68db3576-c08c-57b9-8853-165445ea6508", title: "Foreign", archived: false };
  await writeFile(path, JSON.stringify(foreign));
  await assert.rejects(start(directory), /belongs to 68db3576-c08c-57b9-8853-165445ea6508, not 00000000-0000-4000-8000-000000000000/);
  assert.deepEqual(await readJson(path), foreign, "a foreign session is never reassigned");
});

test("missing Pi fails visibly", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "kuggis-chat-missing-"));
  const server = await start(directory, "/nonexistent/pi");
  t.after(async () => { await server.stop(); await rm(directory, { recursive: true, force: true }); });
  await assert.rejects(server.client.createThread(), /start Pi/);
});

test("provider failures survive restart and internal system records stay hidden", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "kuggis-failed-pi-"));
  let server = await start(directory);
  t.after(async () => { await server.stop(); await rm(directory, { recursive: true, force: true }); });
  const { metadata: { id } } = await server.client.createThread();
  const controller = new PiThreadController(server.client, id);
  t.after(() => controller.dispose());
  await controller.load(); controller.connect();
  await server.client.sendMessage(id, { content: "[error]" });
  await until(() => controller.getState().lastError === "Provider usage exhausted", "provider failure is visible");
  assert.deepEqual(controller.getState().messages.map((m) => m.role), ["user", "assistant"]);
  controller.dispose(); await server.stop();
  server = await start(directory);
  const resumed = await server.client.getThread(id);
  assert.equal(resumed.lastError, "Provider usage exhausted");
  assert.equal(resumed.metadata.status, "failed");
  assert.deepEqual(resumed.messages.map((m) => m.role), ["user", "assistant"]);
});

test("installed Pi RPC smoke (no prompt or model call)", { skip: !process.env.KUGGIS_TEST_PI }, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "kuggis-real-pi-"));
  let server;
  t.after(async () => { try { await server?.stop(); } finally { await rm(directory, { recursive: true, force: true }); } });
  // Local chat normally inherits the operator's Pi configuration. This smoke
  // test must not load their credentials, extensions, or MCP configuration.
  const agentDir = join(directory, "agent");
  await mkdir(agentDir, { mode: 0o700 });
  server = await start(directory, process.env.KUGGIS_TEST_PI, {
    ...process.env, PI_CODING_AGENT_DIR: agentDir, PI_TELEMETRY: "0",
  });
  const models = await server.client.getAvailableModels();
  assert.ok(Array.isArray(models));
  const snapshot = await server.client.createThread();
  assert.equal(snapshot.metadata.status, "idle");
  assert.equal(snapshot.messages.length, 0);
  assert.equal((await server.client.getThread(snapshot.metadata.id)).metadata.id, snapshot.metadata.id);
});
