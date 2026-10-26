import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";

const playwright = process.env.KUGGIS_TEST_PLAYWRIGHT;

test("creation and settings review public internet access while preserving narrower grants", { skip: !playwright, timeout: 30000 }, async t => {
  const { chromium } = await import(playwright);
  const server = createServer(async (request, response) => {
    const path = new URL(request.url, "http://localhost").pathname;
    try {
      const bytes = await readFile(new URL(`../dist${path === "/" ? "/index.html" : path}`, import.meta.url));
      response.setHeader("Content-Type", path.endsWith(".js") ? "text/javascript" : path.endsWith(".css") ? "text/css" : "text/html");
      response.end(bytes);
    } catch { response.writeHead(404).end(); }
  });
  let browser;
  t.after(async () => {
    await browser?.close();
    await new Promise(resolve => server.close(resolve));
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  browser = await chromium.launch({ headless: true, ...(process.env.KUGGIS_TEST_BROWSER ? { executablePath: process.env.KUGGIS_TEST_BROWSER } : {}) });
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  const parent = { name: "parent", parent: null, grants: [{ host: "cdn.playwright.dev" }], lifecycle: { state: "available" }, vm: null, creation: null, deletion: null };
  const publicParent = { ...parent, name: "research", grants: [{ network: "public" }] };
  const previews = [];
  const changes = [];
  let applied = 0;
  await page.route("**/api/**", route => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/cells") return route.fulfill({ json: { cells: [parent, publicParent], mode: "cells", canApply: true, canCreate: true, canDelete: false } });
    if (path.endsWith("/health")) return route.fulfill({ json: { state: "ready", message: "Chat is ready.", recovery: null } });
    if (path === "/api/publishing") return route.fulfill({ json: null });
    if (path === "/api/cells/parent/settings") return route.fulfill({ json: { providers: [], connections: [] } });
    if (path === "/api/cells/parent/settings/connections") return route.fulfill({ json: {} });
    if (path === "/api/cells/parent/plan") {
      changes.push(route.request().postDataJSON().grants);
      return route.fulfill({ json: { id: "network-change", approval: "fixture-approval" } });
    }
    if (path === "/api/changes/network-change/apply") {
      assert.deepEqual(route.request().postDataJSON(), { approval: "fixture-approval" });
      parent.grants = changes.at(-1);
      applied++;
      return route.fulfill({ json: {} });
    }
    if (path === "/api/cells/plan") {
      const input = route.request().postDataJSON();
      previews.push(input);
      return route.fulfill({ json: { id: "fixture", cell: { name: input.name, grants: input.grants }, parent: input.parent, phase: { state: "review" } } });
    }
    if (path.includes("/pi/")) return route.fulfill({ json: [] });
    return route.fulfill({ status: 404, json: { error: "Unexpected fixture request" } });
  });
  await page.goto(`http://127.0.0.1:${server.address().port}`);
  await page.getByRole("button", { name: "＋ Cell", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "New cell", exact: true });
  await dialog.getByRole("textbox", { name: "Cell name", exact: true }).fill("browser");
  const internet = dialog.getByRole("checkbox", { name: "Allow public internet", exact: true });
  assert.equal(await internet.isChecked(), false);
  await internet.check();
  const hosts = dialog.getByRole("textbox", { name: "Allowed hosts", exact: true });
  const defaults = (await hosts.inputValue()).split("\n");
  assert.ok(defaults.includes("cdn.playwright.dev"));
  assert.ok(defaults.includes("storage.googleapis.com"));
  assert.ok(defaults.includes("playwright.download.prss.microsoft.com"));

  // Defaults are a proposal: removed hosts stay removed through review/back,
  // and custom hosts survive without duplicate grants.
  await hosts.fill(defaults.filter(host => host !== "storage.googleapis.com").concat(["custom.example", "custom.example"]).join("\n"));
  await dialog.getByRole("button", { name: "Continue", exact: true }).click();
  await page.getByRole("button", { name: "Create cell", exact: true }).waitFor();
  assert.equal(previews.length, 1);
  assert.ok(previews[0].grants.some(g => g.network === "public"));
  await page.getByRole("dialog").getByText("Public internet (HTTPS)", { exact: true }).waitFor();
  assert.equal(previews[0].grants.filter(g => g.host === "custom.example").length, 1);
  assert.ok(!previews[0].grants.some(g => g.host === "storage.googleapis.com"));
  await page.getByRole("dialog").getByText("custom.example", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Back", exact: true }).click();
  assert.equal(await internet.isChecked(), true);
  assert.ok(!(await hosts.inputValue()).includes("storage.googleapis.com"));
  assert.ok((await hosts.inputValue()).includes("custom.example"));

  await dialog.getByRole("combobox", { name: "Parent cell", exact: true }).selectOption("parent");
  assert.equal(await hosts.inputValue(), "cdn.playwright.dev");
  assert.equal(await internet.isChecked(), false);
  assert.equal(await internet.isDisabled(), true);
  await dialog.getByRole("status").filter({ hasText: "parent does not grant" }).waitFor();
  await dialog.getByRole("button", { name: "Continue", exact: true }).click();
  await page.getByRole("button", { name: "Create cell", exact: true }).waitFor();
  assert.equal(previews[1].name, "parent-browser");
  assert.deepEqual(previews[1].grants, [{ host: "cdn.playwright.dev" }]);

  await page.getByRole("button", { name: "Back", exact: true }).click();
  await dialog.getByRole("combobox", { name: "Parent cell", exact: true }).selectOption("research");
  assert.equal(await internet.isDisabled(), false);
  assert.equal(await internet.isChecked(), false); // authority is never opted in automatically
  assert.deepEqual((await hosts.inputValue()).split("\n"), defaults);
  assert.equal(await dialog.getByRole("status").filter({ hasText: "parent does not grant" }).count(), 0);
  await internet.check();
  await dialog.getByRole("button", { name: "Continue", exact: true }).click();
  await page.getByRole("button", { name: "Create cell", exact: true }).waitFor();
  assert.ok(previews[2].grants.some(g => g.network === "public"));
  assert.deepEqual(previews[2].grants.filter(g => "host" in g), defaults.map(host => ({ host })));

  // Settings must review both directions and preserve unrelated authority.
  const retained = [
    { host: "cdn.playwright.dev" }, { publish: "private" },
    { publish: { namespace: "a".repeat(64) } },
    { publish: { site: { namespace: "b".repeat(64), name: "report" } } },
    { edge: "search" },
  ];
  parent.grants = retained;
  await page.getByRole("button", { name: "Close new cell", exact: true }).click();
  await page.reload();
  await page.getByRole("button", { name: "Cell settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "parent", exact: true });
  const access = settings.getByRole("checkbox", { name: "Allow public internet", exact: true });
  for (const enable of [true, false]) {
    assert.equal(await access.isChecked(), !enable);
    await access.setChecked(enable);
    await settings.getByRole("button", { name: "Save", exact: true }).click();
    await settings.getByText(`${enable ? "Allow" : "Remove"}: Public internet (HTTPS)`, { exact: true }).waitFor();
    assert.equal(applied, enable ? 0 : 1); // planning alone does not apply
    assert.deepEqual(changes.at(-1).filter(g => !("network" in g)), [retained[1], retained[0], ...retained.slice(2)]);
    assert.equal(changes.at(-1).some(g => g.network === "public"), enable);
    assert.equal(await settings.locator(".review li").count(), 1);
    await settings.getByRole("button", { name: "Apply changes", exact: true }).click();
    await settings.getByRole("status").filter({ hasText: "Saved" }).waitFor();
    await settings.getByRole("button", { name: "Close settings", exact: true }).click();
    await page.getByRole("button", { name: "Cell settings", exact: true }).click();
    assert.equal(await access.isChecked(), enable);
  }
  assert.equal(applied, 2);
  assert.deepEqual(errors, []);
});
