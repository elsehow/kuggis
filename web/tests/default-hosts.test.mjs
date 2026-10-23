import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import ts from "typescript";

const source = await readFile(new URL("../src/default-hosts.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } });
const { defaultHosts } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);

test("browser defaults cover package installation, download redirects, and guest apt mirrors", () => {
  const hosts = defaultHosts();
  for (const url of [
    "https://registry.npmjs.org/playwright",
    "https://cdn.playwright.dev/builds/cft/156.0.8078.4/linux64/chrome-linux64.zip",
    "https://storage.googleapis.com/chrome-for-testing-public/156.0.8078.4/linux64/chrome-linux64.zip",
    "https://playwright.download.prss.microsoft.com/dbazure/download/playwright/builds/ffmpeg/1011/ffmpeg-linux.zip",
    "https://archive.ubuntu.com/ubuntu",
    "https://us.archive.ubuntu.com/ubuntu",
    "https://security.ubuntu.com/ubuntu",
  ]) assert.ok(hosts.includes(new URL(url).hostname), url);
  assert.equal(hosts.length, new Set(hosts).size);
  assert.ok(hosts.every(host => !host.includes("*") && !host.includes("/")));
});

test("subcell defaults cannot broaden the parent's authority", () => {
  assert.deepEqual(defaultHosts([]), []);
  const parent = [
    { host: "cdn.playwright.dev" }, { host: "registry.npmjs.org" },
    { host: "unrelated.example" }, { provider: "openai" },
    { code: { repo: "example/project", write: true } },
  ];
  const snapshot = structuredClone(parent);
  assert.deepEqual(defaultHosts(parent).sort(), ["cdn.playwright.dev", "registry.npmjs.org"]);
  assert.deepEqual(parent, snapshot);
  assert.deepEqual(defaultHosts([{ host: "playwright.dev" }]), []);
});
