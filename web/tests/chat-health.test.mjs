import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const playwright = process.env.KUGGIS_TEST_PLAYWRIGHT;
const root = fileURLToPath(new URL("../../", import.meta.url));
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));

test("opening a cell starts stopped chat, diagnoses crashes, and requires explicit repair", { skip: !playwright, timeout: 60000 }, async t => {
  const { chromium } = await import(playwright);
  const state = await mkdtemp(join(tmpdir(), "kuggis-health-browser-"));
  const server = spawn(join(root, "cell/target/debug/cell"), ["chat", "--local", root, "--state", state, "--listen", "127.0.0.1:0", "--assets", join(root, "web/dist"), "--pi", join(root, "web/tests/fake-pi.mjs")]);
  let output = "";
  server.stderr.on("data", bytes => { output += bytes; });
  server.on("error", error => { output += error.message; });
  let browser, page;
  t.after(async () => {
    await page?.unrouteAll({ behavior: "wait" });
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
  page = await browser.newPage();
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  const ready = { state: "ready", message: "Chat is ready.", recovery: null };
  let health = { state: "stopped", message: "Chat is stopped.", recovery: "start" };
  const recovery = [];
  const writes = [];
  let requestsBeforeReady = 0;
  let holdHealth;
  let healthRequested;
  let holdList;
  let listRequested;
  let listFailure = false;
  await page.route("**/api/cells", route => route.fulfill({ json: {
    mode: "cells", canApply: false, canCreate: false, canDelete: false,
    cells: [{ name: "demo", parent: null, grants: [], vm: "demo", lifecycle: { state: "available" }, creation: null, deletion: null }],
  } }));
  await page.route("**/api/cells/demo/**", async route => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("/health")) {
      healthRequested?.();
      if (holdHealth) await holdHealth;
      return route.fulfill({ json: health });
    }
    if (path.endsWith("/recovery")) {
      recovery.push(route.request().postDataJSON().action);
      health = ready;
      return route.fulfill({ json: health });
    }
    if (route.request().method() === "POST") writes.push(path);
    if (health.state !== "ready") {
      requestsBeforeReady++;
      return route.fulfill({ status: 502, json: { error: "Chat service unavailable. Check the cell's chat health and recovery options." } });
    }
    if (path.endsWith("/threads") && route.request().method() === "GET") {
      listRequested?.();
      if (holdList) await holdList;
      if (listFailure) return route.fulfill({ status: 400, json: { error: "List temporarily unavailable" } });
    }
    return route.continue({ url: route.request().url().replace("/api/cells/demo/", "/api/") });
  });
  await page.goto(`${url}/?cell=demo`);
  await page.getByRole("button", { name: "＋ New chat", exact: true, disabled: false }).waitFor();
  assert.deepEqual(recovery, ["start"]);
  assert.equal(requestsBeforeReady, 0, "chat requests wait for service readiness");

  // Each return checks fresh health, with the same sidebar skeleton spanning
  // both a delayed health response and a delayed chat-list response.
  await page.getByRole("button", { name: "Resources", exact: true }).click();
  let releaseHealth, releaseList;
  holdHealth = new Promise(resolve => { releaseHealth = resolve; });
  holdList = new Promise(resolve => { releaseList = resolve; });
  const probing = new Promise(resolve => { healthRequested = resolve; });
  const listing = new Promise(resolve => { listRequested = resolve; });
  await page.getByRole("button", { name: "demo", exact: true }).click();
  await probing;
  const loading = page.getByRole("complementary", { name: "Chats" }).getByRole("status", { name: "Loading chats" });
  try {
    await loading.waitFor();
    await page.emulateMedia({ reducedMotion: "reduce" });
    assert.equal(await page.locator(".chat-skeleton").first().evaluate(node => getComputedStyle(node).animationName), "none");
    assert.equal(await page.getByRole("heading", { name: "Checking chat…" }).count(), 0);
    assert.equal(await page.getByText("No chats yet.", { exact: true }).count(), 0);
    assert.equal(await page.getByRole("button", { name: "＋ New chat", exact: true }).isDisabled(), true);
    releaseHealth(); holdHealth = null; healthRequested = null;
    await listing;
    await loading.waitFor();
    assert.equal(await page.getByText("No chats yet.", { exact: true }).count(), 0);
    // Exercise list-error handling separately from service recovery.
    listFailure = true;
    releaseList(); holdList = null; listRequested = null;
    await page.getByRole("button", { name: "Retry loading chats" }).waitFor();
    listFailure = false;
    await page.getByRole("button", { name: "Retry loading chats" }).click();
    await page.getByText("No chats yet.", { exact: true }).waitFor();
    assert.equal(await loading.count(), 0);
  } finally {
    releaseHealth(); releaseList(); holdHealth = null; holdList = null;
    healthRequested = null; listRequested = null;
  }

  health = { state: "failed", message: "Chat cannot start because its configured workspace differs from the saved chats. Repair restores the saved workspace and preserves conversations.", recovery: "repair" };
  await page.reload();
  await page.getByRole("heading", { name: "Chat service unavailable" }).waitFor();
  await page.getByText("Repair restores", { exact: false }).waitFor();
  assert.deepEqual(recovery, ["start"], "opening a crashed cell does not restart it");
  assert.equal(requestsBeforeReady, 0);
  if (process.env.KUGGIS_HEALTH_SCREENSHOT) await page.screenshot({ path: process.env.KUGGIS_HEALTH_SCREENSHOT });
  await page.getByRole("button", { name: "Repair chat", exact: true }).click();
  await page.getByRole("button", { name: "＋ New chat", exact: true, disabled: false }).waitFor();
  assert.deepEqual(recovery, ["start", "repair"]);

  health = { state: "failed", message: "Chat failed to start (result: exit-code, exit: 2).", recovery: "restart" };
  // A failed create diagnoses the service without replaying the user's prompt.
  await page.getByRole("textbox", { name: "Message", exact: true }).fill("Do not replay this message");
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await page.getByRole("button", { name: "Retry starting chat" }).waitFor();
  assert.deepEqual(recovery, ["start", "repair"]);
  await page.getByRole("button", { name: "Check again" }).click();
  await page.getByRole("button", { name: "Retry starting chat", disabled: false }).waitFor();
  assert.deepEqual(recovery, ["start", "repair"]);
  const attemptedWrites = [...writes];
  await page.getByRole("button", { name: "Retry starting chat" }).click();
  await page.getByRole("button", { name: "＋ New chat", exact: true, disabled: false }).waitFor();
  assert.deepEqual(recovery, ["start", "repair", "restart"]);
  assert.deepEqual(writes, attemptedWrites, "recovering does not replay a prompt or session creation");
  assert.deepEqual(errors, []);
});
