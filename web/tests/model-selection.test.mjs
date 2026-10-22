import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

// Opt-in, like the installed-Pi tests. Point this at an installed Playwright
// module; no browser download or real provider credentials are needed.
const playwright = process.env.KUGGIS_TEST_PLAYWRIGHT;
const root = fileURLToPath(new URL("../../", import.meta.url));
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));

test("new-chat model selection refreshes the header before the first prompt", { skip: !playwright, timeout: 45000 }, async t => {
  const { chromium } = await import(playwright);
  const state = await mkdtemp(join(tmpdir(), "kuggis-model-browser-"));
  const server = spawn(join(root, "cell/target/debug/cell"), ["chat", "--local", root, "--state", state, "--listen", "127.0.0.1:0", "--assets", join(root, "web/dist"), "--pi", join(root, "web/tests/fake-pi.mjs")]);
  let output = "";
  server.stderr.on("data", bytes => { output += bytes; });
  server.on("error", error => { output += error.message; });
  let browser;
  t.after(async () => {
    await browser?.close();
    if (server.exitCode === null && server.signalCode === null) {
      const stopped = new Promise(resolve => server.on("close", resolve));
      server.kill("SIGINT");
      const timeout = setTimeout(() => server.kill("SIGKILL"), 5000);
      try { await stopped; } finally { clearTimeout(timeout); }
    }
    await rm(state, { recursive: true, force: true });
  });
  for (let attempt = 0; !output.includes("Kuggis chat:") && server.exitCode === null && attempt < 200; attempt++) await pause(50);
  const url = output.match(/Kuggis chat: (http:\/\/[^\s]+)/)?.[1];
  assert.ok(url, output);
  browser = await chromium.launch({ headless: true, ...(process.env.KUGGIS_TEST_BROWSER ? { executablePath: process.env.KUGGIS_TEST_BROWSER } : {}) });
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  const requestModel = name => page.getByRole("button", { name: /^Model:/ }).click()
    .then(() => page.getByRole("menuitem", { name: "fixture" }).click())
    .then(() => page.getByRole("menuitem", { name, exact: true }).click());

  // Hold the write until the new thread's initial/default-model snapshot has
  // loaded. This deterministically reproduces the stale idle-controller bug.
  let releaseModel;
  const modelGate = new Promise(resolve => { releaseModel = resolve; });
  let modelRequested;
  const modelRequest = new Promise(resolve => { modelRequested = resolve; });
  t.after(() => releaseModel());
  await page.route("**/api/pi/threads/*/model", async route => {
    modelRequested();
    await modelGate;
    await route.continue();
  });
  await page.goto(url);
  // The first available cell now opens automatically in the sidebar layout.
  await page.getByRole("textbox", { name: "Message", exact: true }).fill("Keep this draft");
  await requestModel("Second model");
  await modelRequest;
  await page.getByRole("button", { name: "Model: Test model", exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "Send message", exact: true }).isDisabled(), true);
  assert.equal(await page.getByRole("textbox", { name: "Message", exact: true }).inputValue(), "Keep this draft");
  releaseModel();
  await page.getByRole("button", { name: "Model: Second model", exact: true }).waitFor();
  const id = new URL(page.url()).searchParams.get("chat");
  const snapshot = async () => (await (await fetch(`${url}/api/pi/threads/${id}`)).json());
  assert.equal((await snapshot()).metadata.config.modelId, "second-model");
  assert.equal((await (await fetch(`${url}/api/pi/threads`)).json()).length, 1);
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await page.getByText("This chat is saved and can be resumed.", { exact: false }).waitFor();
  for (let attempt = 0; (await snapshot()).metadata.status === "running" && attempt < 200; attempt++) await pause(25);
  const messages = (await snapshot()).messages;
  assert.ok(messages.some(message => message.role === "assistant"));
  assert.ok(messages.filter(message => message.role === "assistant").every(message => message.model === "second-model"));
  await page.reload();
  await page.getByRole("button", { name: "Model: Second model", exact: true }).waitFor();

  // Existing chats still use the normal controller path. Failed writes keep
  // the actual selection and release the send/model controls.
  await requestModel("Test model");
  await page.getByRole("button", { name: "Model: Test model", exact: true }).waitFor();
  await page.unroute("**/api/pi/threads/*/model");
  await page.route("**/api/pi/threads/*/model", route => route.fulfill({ status: 502, contentType: "application/json", body: JSON.stringify({ error: "Model unavailable" }) }));
  await page.getByRole("textbox", { name: "Message", exact: true }).fill("Retry draft");
  await requestModel("Second model");
  await page.getByRole("alert").filter({ hasText: "Model unavailable" }).first().waitFor();
  assert.equal((await snapshot()).metadata.config.modelId, "test-model");
  assert.equal(await page.getByRole("button", { name: "Model: Test model", exact: true }).isDisabled(), false);
  assert.equal(await page.getByRole("button", { name: "Send message", exact: true }).isDisabled(), false);
  assert.deepEqual(errors, []);
});
