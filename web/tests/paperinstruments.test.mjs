import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import { createInterface } from "node:readline";
import { tmpdir } from "node:os";
import { join } from "node:path";

const pi = process.env.KUGGIS_TEST_PI;
test("Pi streams Ultramarine reasoning and tool turns with a compatible request", { skip: !pi, timeout: 25000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), "kuggis-paper-"));
  const requests = [];
  const server = createServer(async (req, res) => {
    let bytes = "";
    for await (const chunk of req) bytes += chunk;
    requests.push({ path: req.url, authorization: req.headers.authorization, body: JSON.parse(bytes) });
    res.writeHead(200, { "Content-Type": "text/event-stream" });
    const chunk = (delta, finish_reason = null) => res.write(`data: ${JSON.stringify({ id: "fixture", object: "chat.completion.chunk", created: 1, model: "ultramarine-1-flash", choices: [{ index: 0, delta, finish_reason }] })}\n\n`);
    if (requests.length === 1) {
      chunk({ role: "assistant", reasoning_content: "Read the local fixture." });
      chunk({ tool_calls: [{ index: 0, id: "read_fixture", type: "function", function: { name: "read", arguments: JSON.stringify({ path: join(dir, "fixture.txt") }) } }] });
      chunk({}, "tool_calls");
    } else {
      chunk({ role: "assistant", content: "Paper fixture complete." });
      chunk({}, "stop");
    }
    res.end("data: [DONE]\n\n");
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  t.after(async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); await rm(dir, { recursive: true, force: true }); });
  const paper = JSON.parse(await readFile(new URL("../../cell/src/pi/paperinstruments.json", import.meta.url), "utf8"));
  paper.baseUrl = `http://127.0.0.1:${server.address().port}/v1`;
  await writeFile(join(dir, "models.json"), JSON.stringify({ providers: { paperinstruments: paper } }));
  await writeFile(join(dir, "auth.json"), JSON.stringify({ paperinstruments: { type: "api_key", key: "paper-test-credential" } }), { mode: 0o600 });
  await writeFile(join(dir, "fixture.txt"), "local fixture contents");
  const child = spawn(pi, ["--mode", "rpc", "--no-session", "--no-extensions", "--no-mcp", "--no-skills", "--no-prompt-templates", "--no-context-files", "--offline", "--tools", "read", "--provider", "paperinstruments", "--model", "ultramarine-1-flash", "--thinking", "max"], {
    cwd: dir, env: { ...process.env, PI_CODING_AGENT_DIR: dir, PI_TELEMETRY: "0" },
  });
  let stderr = "", done = false, sawReasoning = false, sawTool = false;
  child.stderr.on("data", data => { stderr += data; });
  const timer = setTimeout(() => child.kill("SIGKILL"), 20000);
  t.after(() => { clearTimeout(timer); child.kill("SIGTERM"); });
  child.stdin.write(`${JSON.stringify({ id: "prompt", type: "prompt", message: "Read fixture.txt." })}\n`);
  for await (const line of createInterface({ input: child.stdout })) {
    const event = JSON.parse(line);
    if (event.type === "message_update" && event.assistantMessageEvent?.type === "thinking_delta") sawReasoning = true;
    if (event.type === "tool_execution_end") { sawTool = true; assert.equal(event.isError, false); }
    if (event.type === "agent_end") { done = true; break; }
  }
  assert.ok(done, stderr);
  assert.ok(sawReasoning);
  assert.ok(sawTool);
  assert.equal(requests.length, 2);
  for (const { path, authorization, body } of requests) {
    assert.equal(path, "/v1/chat/completions");
    assert.equal(authorization, "Bearer paper-test-credential");
    assert.equal(body.model, "ultramarine-1-flash");
    assert.equal(body.reasoning_effort, "max");
    assert.ok(body.messages.some(m => m.role === "system"));
    assert.ok(body.messages.every(m => m.role !== "developer"));
    for (const field of ["store", "metadata", "service_tier"]) assert.equal(body[field], undefined);
    assert.ok((body.max_completion_tokens ?? body.max_tokens) <= 65536);
  }
  const second = requests[1].body.messages;
  assert.equal(second.find(m => m.role === "assistant").reasoning_content, "Read the local fixture.");
  assert.ok(second.some(m => m.role === "tool" && m.content.includes("local fixture contents")));
});
