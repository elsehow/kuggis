import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, rm, chmod, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { createPiHttpClient, PiThreadController } from "@assistant-ui/react-pi";
import { MAX_IMAGE_BYTES, MAX_MESSAGE_BODY_BYTES, utf8Length, mib } from "../src/attachment-limits.mjs";

const root = fileURLToPath(new URL("../../", import.meta.url));
const fixture = fileURLToPath(new URL("fake-pi.mjs", import.meta.url));
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, label) {
  const deadline = Date.now() + 12000;
  while (Date.now() < deadline) { if (await fn()) return; await wait(20); }
  throw new Error(`Timed out: ${label}`);
}
async function start(state) {
  const child = spawn(join(root, "cell/target/debug/cell"), ["chat", "--local", root, "--state", state, "--listen", "127.0.0.1:0", "--assets", join(root, "web/dist"), "--pi", fixture], {});
  let output = "";
  child.stderr.on("data", (data) => { output += data; });
  child.on("error", (error) => { output += error.message; });
  await until(() => /Kuggis chat: (http:\/\/[^\s]+)/.test(output) || child.exitCode !== null, "server startup");
  assert.equal(child.exitCode, null, output);
  const url = output.match(/Kuggis chat: (http:\/\/[^\s]+)/)[1];
  return { child, url, client: createPiHttpClient({ baseUrl: `${url}/api/pi`, streamCloseDelayMs: 0, reconnectDelay: () => wait(25) }) };
}
const readFixtureTranscript = async (state, id) => JSON.parse(await readFile(join(state, "pi", `${id}.fixture.json`), "utf8"));

test("image limits are real, not arbitrary", () => {
  // Provider hard limit: Anthropic rejects image blocks over 5 MiB.
  assert.equal(MAX_IMAGE_BYTES, 5 * 1024 * 1024);
  // Server caps: both the HTTP body limit and the Pi RPC frame limit are 16 MiB.
  assert.equal(16 * 1024 * 1024 - 64 * 1024, MAX_MESSAGE_BODY_BYTES);
  assert.ok(MAX_MESSAGE_BODY_BYTES < 16 * 1024 * 1024, "must stay below the 16 MiB ingress/RPC caps");
  assert.ok(MAX_MESSAGE_BODY_BYTES >= 8 * 1024 * 1024, "headroom must not shrink real capacity below base64 of the provider image limit");
});

test("utf8Length counts exact UTF-8 bytes", () => {
  assert.equal(utf8Length(""), 0);
  assert.equal(utf8Length("abc"), 3);
  assert.equal(utf8Length("ä"), 2);
  assert.equal(utf8Length("😀"), 4);
  const base64 = Buffer.alloc(1024).toString("base64");
  assert.equal(utf8Length(base64), base64.length, "base64 is ASCII");
  assert.equal(mib(5 * 1024 * 1024), "5.0");
});

test("oversized request bodies are rejected with 413 by the server cap the guard mirrors", { timeout: 45000 }, async (t) => {
  await chmod(fixture, 0o755);
  const directory = await mkdtemp(join(tmpdir(), "kuggis-attach-big-"));
  const server = await start(directory);
  t.after(async () => {
    if (server.child.exitCode !== null || server.child.signalCode !== null) return void await rm(directory, { recursive: true, force: true });
    server.child.kill("SIGINT");
    try { await until(() => server.child.exitCode !== null || server.child.signalCode !== null, "server shutdown"); }
    catch { server.child.kill("SIGKILL"); }
    await rm(directory, { recursive: true, force: true });
  });
  const created = await server.client.createThread();
  const id = created.metadata.id;
  // One byte past the 16 MiB ingress cap, matching what the client guard avoids.
  const oversized = JSON.stringify({ input: { content: "x".repeat(16 * 1024 * 1024 + 2 - JSON.stringify({ input: { content: "" } }).length) } });
  const response = await fetch(`${server.url}/api/pi/threads/${id}/messages`, { method: "POST", headers: { "Content-Type": "application/json" }, body: oversized });
  assert.equal(response.status, 413);
});

test("image attachments flow through the bridge to Pi", { timeout: 45000 }, async (t) => {
  await chmod(fixture, 0o755);
  const directory = await mkdtemp(join(tmpdir(), "kuggis-attach-"));
  const server = await start(directory);
  const controllers = [];
  t.after(async () => {
    for (const c of controllers) c.dispose();
    if (server.child.exitCode === null && server.child.signalCode === null) {
      server.child.kill("SIGINT");
      await until(() => server.child.exitCode !== null || server.child.signalCode !== null, "server shutdown");
    }
    await rm(directory, { recursive: true, force: true });
  });
  const { metadata: { id } } = await server.client.createThread();
  const controller = new PiThreadController(server.client, id);
  controllers.push(controller);
  await controller.load(); controller.connect();
  const png = Buffer.from("89504e470d0a1a0a0000000d49484452", "hex").toString("base64");
  await server.client.sendMessage(id, {
    content: "what is in this image?",
    attachments: [{ type: "image", mimeType: "image/png", data: png }],
  });
  await until(() => controller.getState().runStatus === "idle", "run completes");
  // The fixture records exactly what the bridge handed to Pi: text plus the image block.
  const transcript = await readFixtureTranscript(directory, id);
  const user = transcript.find((m) => m.role === "user");
  assert.ok(Array.isArray(user.content), "fixture received user content parts");
  assert.deepEqual(user.content[0], { type: "text", text: "what is in this image?" });
  assert.equal(user.content[1].type, "image");
  assert.equal(user.content[1].mimeType, "image/png");
  assert.equal(user.content[1].data, png);
  await until(() => /what is in this image/.test(controller.getState().messages.at(-1)?.content?.find?.((c) => c.type === "text")?.text ?? ""), "assistant reply visible");
  assert.match(controller.getState().messages.at(-1).content.find((c) => c.type === "text").text, /what is in this image/);
  void until;
  void readFixtureTranscript;
});
