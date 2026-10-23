import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { createPiHttpClient } from "@assistant-ui/react-pi";
import { fileURLToPath } from "node:url";
const root = fileURLToPath(new URL("../../", import.meta.url));
const binary = join(root, "cell/target/debug/cell");
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
async function launch(args, ready) {
  const child = spawn(binary, args, { env: { ...process.env, KUGGIS_FIXTURE_PROVIDER: "anthropic" } }); let log = "";
  child.stderr.on("data", b => { log += b; });
  for (let n = 0; !ready.test(log) && child.exitCode === null && n < 200; n++) await pause(25);
  assert.match(log, ready);
  return { log, async stop() { if (child.exitCode !== null || child.signalCode !== null) return; const end = new Promise(r => child.once("close", r)); child.kill("SIGTERM"); await end; } };
}
const post = (url, body = {}) => fetch(url, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
async function json(response) { assert.equal(response.status, 200, await response.clone().text()); return response.json(); }

async function fixture(t) {
  const dir = await mkdtemp("/tmp/kuggis-identity-");
  for (const sub of ["tree/parent", "tree/.pins", "sockets", "inbox", "outbox"]) await mkdir(join(dir, sub), { recursive: true });
  const parent = JSON.stringify({ name: "parent", grants: [{ provider: "anthropic" }] });
  const child = JSON.stringify({ name: "child", grants: [] });
  await writeFile(join(dir, "tree/parent.json"), parent);
  await writeFile(join(dir, "tree/parent/child.json"), child);
  for (const name of ["parent", "child"]) {
    const key = execFileSync(binary, ["key", "new", join(dir, `${name}.key`)], { encoding: "utf8" }).trim();
    await writeFile(join(dir, `tree/.pins/${name}.pub`), key);
  }
  const cell = await launch(["chat", "--cell", join(dir, "tree/parent.json"), "--workspace", dir, "--state", join(dir, "chat"), "--socket", join(dir, "sockets/parent.sock"), "--agent-dir", join(dir, "auth"), "--pi", join(root, "web/tests/fake-pi.mjs"), "--outbox", join(dir, "outbox"), "--inbox", join(dir, "inbox")], /Kuggis cell/);
  const args = ["ui", join(dir, "tree"), "--state", join(dir, "operator"), "--socket-dir", join(dir, "sockets"), "--assets", join(root, "web/dist"), "--listen", "127.0.0.1:0"];
  let ui = await launch(args, /Kuggis UI/);
  t.after(async () => { await ui.stop(); await cell.stop(); await rm(dir, { recursive: true, force: true }); });
  return { dir, parent, child, url: () => ui.log.match(/Kuggis UI: (http:\/\/\S+)/)[1], async restart() { await ui.stop(); ui = await launch(args, /Kuggis UI/); } };
}

test("legacy cells migrate atomically, retain aliases and chats, and reject changed pins", { timeout: 30000 }, async t => {
  const f = await fixture(t); const base = () => f.url() + "/api/cells/parent";
  const chat = await json(await post(base() + "/pi/threads", { title: "Keep this conversation" }));
  const client = createPiHttpClient({ baseUrl: base() + "/pi" });
  await client.sendMessage(chat.metadata.id, { content: "slow conversation survives migration" });
  const plan = await json(await post(f.url() + "/api/identity/plan"));
  const apply = () => post(f.url() + `/api/identity/migrations/${plan.id}/apply`, { approval: plan.approval });
  assert.equal((await post(f.url() + `/api/identity/migrations/${plan.id}/apply`, { approval: "0".repeat(64) })).status, 400);
  await json(await apply()); await json(await apply());
  const inventory = await json(await fetch(f.url() + "/api/cells"));
  const parent = inventory.cells.find(c => c.binding === "parent"); const child = inventory.cells.find(c => c.binding === "child");
  assert.match(parent.id, /^[a-f0-9-]{36}$/); assert.equal(child.parent, parent.id);
  assert.equal(inventory.canMigrateIdentity, false);
  for (let n = 0; n < 200; n++) {
    const current = await json(await fetch(base() + `/pi/threads/${chat.metadata.id}`));
    if (current.metadata.status !== "running" && current.messages.some(m => m.role === "assistant")) break;
    await pause(20);
  }
  const snapshot = await json(await fetch(base() + `/pi/threads/${chat.metadata.id}`));
  assert.equal(snapshot.messages.filter(m => m.role === "user").length, 1);
  assert.ok(snapshot.messages.some(m => m.role === "assistant"));
  assert.deepEqual((await json(await fetch(f.url() + `/api/cells/${parent.id}/pi/threads/${chat.metadata.id}`))).metadata, snapshot.metadata);
  await json(await post(f.url() + `/api/cells/${parent.id}/rename`, { name: "renamed" }));
  assert.equal((await post(f.url() + `/api/cells/${child.id}/rename`, { name: "parent" })).status, 400);
  assert.equal((await fetch(f.url() + "/api/cells/renamed/health")).status, 404, "display labels never become runtime routes");
  assert.equal(await readFile(join(f.dir, "tree/parent.json"), "utf8"), f.parent);
  assert.equal(await readFile(join(f.dir, "tree/parent/child.json"), "utf8"), f.child);
  await f.restart();
  const restored = await json(await fetch(f.url() + "/api/cells"));
  assert.equal(restored.cells.find(c => c.id === parent.id).name, "renamed");
  assert.equal(restored.cells.find(c => c.id === child.id).parent, parent.id);
  assert.deepEqual((await json(await fetch(base() + `/pi/threads/${chat.metadata.id}`))).metadata, snapshot.metadata);
  const after = await json(await fetch(base() + `/pi/threads/${chat.metadata.id}`));
  assert.deepEqual(after.messages, snapshot.messages, "migration and restart never replay the prompt");
  const replacement = execFileSync(binary, ["key", "new", join(f.dir, "replacement.key")], { encoding: "utf8" }).trim();
  await writeFile(join(f.dir, "tree/.pins/parent.pub"), replacement);
  assert.equal((await fetch(f.url() + `/api/cells/${parent.id}/pi/threads`)).status, 400);
  assert.equal((await fetch(base() + "/pi/threads")).status, 400);
});

test("browser migration and rename retain the selected chat and draft", { skip: !process.env.KUGGIS_TEST_PLAYWRIGHT, timeout: 45000 }, async t => {
  const f = await fixture(t);
  const { chromium } = await import(process.env.KUGGIS_TEST_PLAYWRIGHT);
  const browser = await chromium.launch({ headless: true, ...(process.env.KUGGIS_TEST_BROWSER ? { executablePath: process.env.KUGGIS_TEST_BROWSER } : {}) });
  t.after(() => browser.close());
  const page = await browser.newPage(); const errors = []; page.on("pageerror", e => errors.push(e.message));
  const chat = await json(await post(f.url() + "/api/cells/parent/pi/threads", { title: "Saved conversation" }));
  await page.goto(f.url() + `/?cell=parent&chat=${chat.metadata.id}`);
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await composer.fill("Keep my unsent draft");
  await page.getByRole("button", { name: "Enable cell renaming…", exact: true }).click();
  await page.getByRole("button", { name: "Assign permanent IDs", exact: true }).click();
  await page.getByRole("heading", { name: "Enable cell renaming", exact: true }).waitFor({ state: "hidden" });
  assert.equal(await composer.inputValue(), "Keep my unsent draft");
  const selected = new URL(page.url()).searchParams.get("cell"); assert.notEqual(selected, "parent");
  await page.getByRole("button", { name: "Cell settings", exact: true }).click();
  await page.getByRole("textbox", { name: "Cell name", exact: true }).fill("new-parent");
  await page.getByRole("button", { name: "Rename", exact: true }).click();
  await page.getByRole("heading", { name: "new-parent", exact: true }).waitFor();
  await page.getByRole("button", { name: "Close settings", exact: true }).click();
  assert.equal(await composer.inputValue(), "Keep my unsent draft");
  assert.equal(new URL(page.url()).searchParams.get("cell"), selected);
  assert.equal(new URL(page.url()).searchParams.get("chat"), chat.metadata.id);
  assert.deepEqual(errors, []);
});
