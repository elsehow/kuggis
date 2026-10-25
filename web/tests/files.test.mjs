import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { mkdtemp, mkdir, writeFile, readFile, rm, symlink, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));

async function fixture(t) {
  const dir = await realpath(await mkdtemp(join(tmpdir(), "kuggis-files-")));
  const workspace = join(dir, "workspace");
  await mkdir(workspace);
  await mkdir(join(workspace, "round 25"));
  await writeFile(join(workspace, "round 25", "résults #1.csv"), "question,forecast\n1,0.25\n");
  await writeFile(join(workspace, "empty.txt"), "");
  await writeFile(join(dir, "outside.txt"), "outside workspace");
  await symlink(join(dir, "outside.txt"), join(workspace, "outside-link"));
  await symlink(join(workspace, "round 25"), join(workspace, "round-link"));
  const child = spawn(join(root, "cell/target/debug/cell"), ["chat", "--local", workspace, "--state", join(dir, "state"), "--listen", "127.0.0.1:0", "--assets", join(root, "web/dist"), "--pi", join(root, "web/tests/fake-pi.mjs")]);
  let output = "";
  child.stderr.on("data", data => { output += data; });
  child.on("error", error => { output += error.message; });
  t.after(async () => {
    if (child.exitCode === null && child.signalCode === null) {
      const stopped = new Promise(resolve => child.once("close", resolve));
      child.kill("SIGTERM");
      const timer = setTimeout(() => child.kill("SIGKILL"), 5000);
      try { await stopped; } finally { clearTimeout(timer); }
    }
    await rm(dir, { recursive: true, force: true });
  });
  for (let i = 0; i < 200 && !output.includes("Kuggis chat:") && child.exitCode === null; i++) await pause(25);
  const url = output.match(/Kuggis chat: (http:\/\/\S+)/)?.[1];
  assert.ok(url, output);
  const files = (path = "", download = false) => `${url}/api/files${download ? "/download" : ""}${path ? `?${new URLSearchParams({ path })}` : ""}`;
  return { dir, workspace, url, files };
}

test("read-only files, local workspace boundary, and streamed archives", { timeout: 20000 }, async t => {
  const { dir, workspace, files } = await fixture(t);
  const listing = await (await fetch(files())).json();
  assert.equal(listing.path, workspace);
  assert.equal(listing.parent, null);
  assert.equal(listing.kind, "directory");
  assert.equal(listing.entries[0].kind, "directory");
  assert.equal(listing.entries.find(entry => entry.name === "outside-link").kind, "unavailable");
  assert.equal(listing.entries.find(entry => entry.name === "round-link").symlink, true);
  const path = join(workspace, "round 25", "résults #1.csv");
  const response = await fetch(files(path, true));
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "application/octet-stream");
  assert.match(response.headers.get("content-disposition"), /attachment;.*r%C3%A9sults%20%231\.csv/);
  assert.equal(response.headers.get("cache-control"), "no-store");
  assert.equal(response.headers.get("x-content-type-options"), "nosniff");
  assert.equal(await response.text(), await readFile(path, "utf8"));
  assert.equal(await (await fetch(files(join(workspace, "empty.txt"), true))).text(), "");
  assert.equal((await (await fetch(files(path))).json()).kind, "file");
  assert.equal((await (await fetch(files(join(workspace, "round-link")))).json()).path, join(workspace, "round 25"));

  for (const download of [false, true]) {
    for (const path of [join(dir, "outside.txt"), join(workspace, "outside-link"), `${workspace}/../outside.txt`]) {
      assert.equal((await fetch(files(path, download))).status, 403, path);
    }
    assert.equal((await fetch(files("relative.txt", download))).status, 400);
    assert.equal((await fetch(files(join(workspace, "missing"), download))).status, 404);
    assert.equal((await fetch(files(workspace, download), { headers: { Origin: "https://evil.example" } })).status, 403);
    assert.equal((await fetch(files(workspace, download), { headers: { "Sec-Fetch-Site": "cross-site" } })).status, 403);
    for (const method of ["POST", "PUT", "PATCH", "DELETE"]) assert.equal((await fetch(files(workspace, download), { method })).status, 405);
  }
  const archive = await fetch(files(workspace, true));
  assert.equal(archive.status, 200);
  assert.equal(archive.headers.get("content-type"), "application/gzip");
  assert.match(archive.headers.get("content-disposition"), /workspace\.tar\.gz/);
  const bytes = Buffer.from(await archive.arrayBuffer());
  const names = execFileSync("tar", ["-tzf", "-"], { input: bytes, encoding: "utf8" });
  assert.match(names, /round 25/);
  assert.doesNotMatch(names, /outside|round-link/);
  assert.equal(execFileSync("tar", ["-xOzf", "-", "workspace/round 25/résults #1.csv"], { input: bytes, encoding: "utf8" }), await readFile(path, "utf8"));
});

test("Files navigation preserves drafts and downloads from chat links", { skip: !process.env.KUGGIS_TEST_PLAYWRIGHT, timeout: 30000 }, async t => {
  const { workspace, url } = await fixture(t);
  const { chromium } = await import(process.env.KUGGIS_TEST_PLAYWRIGHT);
  const browser = await chromium.launch({ headless: true, ...(process.env.KUGGIS_TEST_BROWSER ? { executablePath: process.env.KUGGIS_TEST_BROWSER } : {}) });
  t.after(() => browser.close());
  const page = await browser.newPage({ acceptDownloads: true });
  const errors = [];
  page.on("pageerror", e => errors.push(e.message));
  await page.goto(url);
  const message = page.getByRole("textbox", { name: "Message", exact: true });
  await message.fill("Keep my draft");
  await page.getByRole("button", { name: "Files", exact: true }).click();
  await page.getByRole("button", { name: "round 25/", exact: true }).click();
  await page.getByRole("button", { name: "résults #1.csv", exact: true }).click();
  const [download] = await Promise.all([page.waitForEvent("download"), page.getByRole("link", { name: "Download file", exact: true }).click()]);
  assert.equal(download.suggestedFilename(), "résults #1.csv");
  assert.equal(await download.failure(), null);
  await page.goBack();
  await page.getByRole("link", { name: "Download folder (.tar.gz)", exact: true }).waitFor();
  await page.getByRole("button", { name: "Chat", exact: true }).click();
  assert.equal(await message.inputValue(), "Keep my draft");
  // The fixture echoes the prompt into its assistant Markdown response.
  await message.fill(`[Results](<${workspace}/round 25>)`);
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await page.locator(".assistant-message").getByRole("link", { name: "Results", exact: true }).click();
  await page.getByRole("link", { name: "Download folder (.tar.gz)", exact: true }).waitFor();
  assert.equal(new URL(page.url()).searchParams.get("path"), `${workspace}/round 25`);
  await page.reload();
  await page.getByRole("link", { name: "Download folder (.tar.gz)", exact: true }).waitFor();
  assert.deepEqual(errors, []);
});
