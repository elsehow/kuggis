import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { createServer, request } from "node:http";
import { once } from "node:events";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

const binary = fileURLToPath(new URL("../../cell/target/debug/cell", import.meta.url));
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
// Node fetch replaces Host with the URL's authority. The ingress fixture must
// send the external Host while connecting to the private loopback listener.
const fetch = (url, options = {}) => new Promise((resolve, reject) => {
  const req = request(url, options, (res) => {
    const chunks = [];
    res.on("data", chunk => chunks.push(chunk));
    res.on("end", () => resolve(new Response(Buffer.concat(chunks), { status: res.statusCode, headers: res.headers })));
    res.on("error", reject);
  });
  req.on("error", reject); req.end();
});

test("hosted ingress protects every route and removes credentials before cell relay", { timeout: 15000 }, async (t) => {
  const dir = await mkdtemp("/tmp/kuggis-hosted-");
  let child;
  const cell = createServer((req, res) => {
    res.setHeader("Content-Type", "application/json");
    res.end(JSON.stringify({ path: req.url, headers: req.headers }));
  });
  t.after(async () => {
    if (child && child.exitCode === null && child.signalCode === null) {
      child.kill("SIGTERM"); await once(child, "exit");
    }
    cell.closeAllConnections();
    await new Promise((resolve) => cell.close(resolve));
    await rm(dir, { recursive: true, force: true });
  });
  for (const name of ["tree", "sockets", "web"]) await mkdir(join(dir, name));
  await writeFile(join(dir, "tree/44fc36e4-5adc-51aa-a6e3-3db15ade9819.json"), JSON.stringify({ id: "44fc36e4-5adc-51aa-a6e3-3db15ade9819", name: "44fc36e4-5adc-51aa-a6e3-3db15ade9819", grants: [] }));
  await writeFile(join(dir, "web/index.html"), "hosted fixture");
  await writeFile(join(dir, "access.json"), JSON.stringify({ origin: "https://supervisor.example", users: ["usr_fixture"] }));
  cell.listen(join(dir, "sockets/44fc36e4-5adc-51aa-a6e3-3db15ade9819.sock")); await once(cell, "listening");
  const reservation = createServer();
  reservation.listen(0, "127.0.0.1"); await once(reservation, "listening");
  const port = reservation.address().port;
  await new Promise((resolve) => reservation.close(resolve));
  child = spawn(binary, ["ui", join(dir, "tree"), "--state", join(dir, "state"), "--assets", join(dir, "web"), "--socket-dir", join(dir, "sockets"), "--listen", `127.0.0.1:${port}`, "--exe-proxy", join(dir, "access.json")]);
  let log = ""; child.stderr.on("data", (b) => { log += b; });
  const deadline = Date.now() + 5000;
  while (!log.includes("Kuggis UI:") && child.exitCode === null && Date.now() < deadline) await wait(20);
  assert.equal(child.exitCode, null, log);
  assert.match(log, /Kuggis UI: https:\/\/supervisor.example/);
  const base = `http://127.0.0.1:${port}`;
  const good = { Host: "supervisor.example", "X-Forwarded-Host": "supervisor.example", "X-Forwarded-Proto": "https", "X-ExeDev-UserID": "usr_fixture", Origin: "https://supervisor.example" };
  for (const [path, method] of [["/", "GET"], ["/api/cells", "GET"], ["/api/cells/plan", "POST"], ["/api/cells/44fc36e4-5adc-51aa-a6e3-3db15ade9819/events", "GET"]]) {
    for (const headers of [{}, { ...good, "X-ExeDev-UserID": "usr_other" }, { ...good, Origin: "https://evil.example" }]) {
      const r = await fetch(base + path, { method, headers });
      assert.equal(r.status, 403, `${method} ${path}`);
    }
  }
  const page = await fetch(base, { headers: good });
  assert.equal(page.status, 200);
  assert.equal(await page.text(), "hosted fixture");
  assert.equal(page.headers.get("cache-control"), "no-store");
  assert.match(page.headers.get("content-security-policy"), /frame-ancestors 'none'/);
  const navigation = { ...good, "Sec-Fetch-Site": "cross-site", "Sec-Fetch-Mode": "navigate", "Sec-Fetch-Dest": "document" };
  delete navigation.Origin;
  assert.equal((await fetch(base + "/", { headers: navigation })).status, 200, "links and login redirects can open the UI");
  assert.equal((await fetch(base + "/api/cells", { headers: navigation })).status, 403);
  assert.equal((await fetch(base + "/", { method: "POST", headers: navigation })).status, 403);
  const inventory = await (await fetch(base + "/api/cells", { headers: good })).json();
  assert.deepEqual(inventory.cells.map(c => c.name), ["44fc36e4-5adc-51aa-a6e3-3db15ade9819"]);
  const relayed = await (await fetch(base + "/api/cells/44fc36e4-5adc-51aa-a6e3-3db15ade9819/info", { headers: { ...good, Cookie: "private", Authorization: "Bearer private", "X-ExeDev-Email": "private@example.com", "X-Forwarded-For": "private", "Content-Type": "application/json" } })).json();
  assert.equal(relayed.path, "/api/info");
  assert.equal(relayed.headers.host, "cell.internal");
  assert.equal(relayed.headers["content-type"], "application/json");
  for (const key of Object.keys(relayed.headers)) {
    assert.ok(!/^(cookie|authorization|origin|x-exedev-|x-forwarded-|sec-fetch-)/.test(key), key);
  }
});

test("an incomplete hosted option cannot fall back to local access", async () => {
  const child = spawn(binary, ["ui", "/unused", "--exe-proxy"]);
  let stderr = ""; child.stderr.on("data", b => { stderr += b; });
  const [code] = await once(child, "exit");
  assert.equal(code, 2);
  assert.match(stderr, /--exe-proxy requires a config file/);
});
