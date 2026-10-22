import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, writeFile, rm, stat } from "node:fs/promises";
import { createInterface } from "node:readline";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const pi = process.env.KUGGIS_TEST_PI;
const extension = fileURLToPath(new URL("../../cell/src/pi/auth.ts", import.meta.url));
async function run(dir, provider, method, respond) {
  const child = spawn(pi, ["--mode", "rpc", "--no-session", "--no-extensions", "--no-mcp", "--no-tools", "--no-skills", "--no-prompt-templates", "--no-context-files", "--offline", "--extension", extension], {
    cwd: dir, env: { ...process.env, PI_CODING_AGENT_DIR: dir, PI_TELEMETRY: "0", KUGGIS_AUTH_PROVIDER: provider, KUGGIS_AUTH_METHOD: method },
  });
  let stderr = "";
  child.stderr.on("data", bytes => { stderr += bytes; });
  const timer = setTimeout(() => child.kill("SIGKILL"), 15000);
  try {
    for await (const line of createInterface({ input: child.stdout })) {
      const event = JSON.parse(line);
      assert.notEqual(event.type, "agent_start", "sign-in must never start model inference");
      if (event.method === "input") {
        const prompt = JSON.parse(event.title);
        assert.equal(prompt.kind, "input");
        assert.equal(prompt.secret, true);
        child.stdin.write(`${JSON.stringify({ type: "extension_ui_response", id: event.id, value: respond })}\n`);
      }
      if (event.method === "notify") {
        const status = JSON.parse(event.message);
        if (status.state === "failed") assert.fail(status.message);
        if (status.state === "complete") return;
      }
    }
    assert.fail(`Pi auth exited before completion: ${stderr}`);
  } finally { clearTimeout(timer); child.kill("SIGTERM"); }
}

test("installed Pi owns per-cell API-key storage and sign-out for every provider", { skip: !pi, timeout: 40000 }, async t => {
  const cellA = await mkdtemp(join(tmpdir(), "kuggis-pi-auth-a-"));
  const cellB = await mkdtemp(join(tmpdir(), "kuggis-pi-auth-b-"));
  t.after(async () => { await rm(cellA, { recursive: true, force: true }); await rm(cellB, { recursive: true, force: true }); });
  const paper = JSON.parse(await readFile(new URL("../../cell/src/pi/paperinstruments.json", import.meta.url), "utf8"));
  await writeFile(join(cellA, "models.json"), JSON.stringify({ providers: { paperinstruments: paper } }));
  for (const provider of ["anthropic", "openai", "paperinstruments"]) {
    await run(cellA, provider, "api_key", `test-${provider}-credential`);
    const saved = JSON.parse(await readFile(join(cellA, "auth.json"), "utf8"));
    assert.deepEqual(saved[provider], { type: "api_key", key: `test-${provider}-credential` });
    assert.equal((await stat(join(cellA, "auth.json"))).mode & 0o777, 0o600);
    await assert.rejects(readFile(join(cellB, "auth.json")), { code: "ENOENT" });
  }
  await run(cellA, "anthropic", "logout");
  const remaining = JSON.parse(await readFile(join(cellA, "auth.json"), "utf8"));
  assert.equal(remaining.anthropic, undefined);
  assert.ok(remaining.openai, "one provider's sign-out leaves the other intact");
  await run(cellA, "openai", "logout");
  await run(cellA, "paperinstruments", "logout");
  assert.deepEqual(JSON.parse(await readFile(join(cellA, "auth.json"), "utf8")), {});
});

test("installed Pi exposes browser fallback for both subscriptions and Anthropic copy-code", { skip: !pi, timeout: 30000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), "kuggis-pi-login-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  for (const [provider, method, host] of [["openai-codex", "browser", "auth.openai.com"], ["anthropic", "browser", "claude.ai"], ["anthropic", "copy_code", "claude.ai"]]) {
    const child = spawn(pi, ["--mode", "rpc", "--no-session", "--no-extensions", "--no-mcp", "--no-tools", "--no-skills", "--no-prompt-templates", "--no-context-files", "--offline", "--extension", extension], {
      cwd: dir, env: { ...process.env, PI_CODING_AGENT_DIR: dir, PI_TELEMETRY: "0", KUGGIS_AUTH_PROVIDER: provider, KUGGIS_AUTH_METHOD: "oauth" },
    });
    const timer = setTimeout(() => child.kill("SIGKILL"), 9000);
    let link, sawInput = false;
    child.stderr.resume();
    try {
      for await (const line of createInterface({ input: child.stdout })) {
        const event = JSON.parse(line);
        assert.notEqual(event.type, "agent_start");
        if (event.method === "notify") {
          const status = JSON.parse(event.message);
          assert.notEqual(status.state, "failed");
          if (status.notice?.kind === "link") link = new URL(status.notice.url);
        }
        if (event.method !== "input") continue;
        const prompt = JSON.parse(event.title);
        if (prompt.kind === "select") {
          assert.ok(prompt.options.some(o => o.value === method));
          assert.ok(prompt.options.some(o => o.value === (provider === "anthropic" ? "copy_code" : "device_code")));
          child.stdin.write(`${JSON.stringify({ type: "extension_ui_response", id: event.id, value: method })}\n`);
        } else {
          assert.equal(link.hostname, host);
          assert.equal(prompt.secret, false);
          const redirect = new URL(link.searchParams.get("redirect_uri"));
          assert.equal(redirect.hostname, method === "copy_code" ? "platform.claude.com" : "localhost");
          assert.match(prompt.message, /code|redirect URL/i);
          sawInput = true;
          break;
        }
      }
      assert.ok(sawInput, `${provider} ${method} must expose the manual callback prompt`);
      const saved = await readFile(join(dir, "auth.json"), "utf8").catch(e => { if (e.code === "ENOENT") return "{}"; throw e; });
      assert.deepEqual(JSON.parse(saved), {}, "opening login must not save credentials");
    } finally {
      clearTimeout(timer);
      child.kill("SIGTERM");
      if (child.exitCode === null && child.signalCode === null) await new Promise(resolve => child.once("exit", resolve));
    }
  }
});
