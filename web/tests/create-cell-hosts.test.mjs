import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";

const playwright = process.env.KUGGIS_TEST_PLAYWRIGHT;

test("creation reviews editable browser hosts and restricts subcell defaults", { skip: !playwright, timeout: 30000 }, async t => {
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
  const previews = [];
  await page.route("**/api/**", route => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/cells") return route.fulfill({ json: { cells: [parent], mode: "cells", canApply: true, canCreate: true, canDelete: false } });
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
  assert.equal(previews[0].grants.filter(g => g.host === "custom.example").length, 1);
  assert.ok(!previews[0].grants.some(g => g.host === "storage.googleapis.com"));
  await page.getByRole("dialog").getByText("custom.example", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Back", exact: true }).click();
  assert.ok(!(await hosts.inputValue()).includes("storage.googleapis.com"));
  assert.ok((await hosts.inputValue()).includes("custom.example"));

  await dialog.getByRole("combobox", { name: "Parent cell", exact: true }).selectOption("parent");
  assert.equal(await hosts.inputValue(), "cdn.playwright.dev");
  await dialog.getByRole("status").filter({ hasText: "parent does not grant" }).waitFor();
  await dialog.getByRole("button", { name: "Continue", exact: true }).click();
  await page.getByRole("button", { name: "Create cell", exact: true }).waitFor();
  assert.equal(previews[1].name, "parent-browser");
  assert.deepEqual(previews[1].grants, [{ host: "cdn.playwright.dev" }]);
  assert.deepEqual(errors, []);
});
