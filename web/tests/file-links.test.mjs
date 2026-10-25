import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import ts from "typescript";

const source = await readFile(new URL("../src/file-links.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } });
const { cellFilePath, fileViewUrl, fileDownloadUrl } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);

test("chat file links distinguish cell paths from remote URLs and unsafe schemes", () => {
  for (const href of ["https://example.com/results.csv", "//example.com/file", "javascript:alert(1)", "data:text/html,hello", "file://remote/home/agent", "relative.txt", "/%2fexample.com", "/bad%00name", "/bad%zzname"]) assert.equal(cellFilePath(href), null, href);
  assert.equal(cellFilePath("/home/agent/round%2025/r%C3%A9sults.csv"), "/home/agent/round 25/résults.csv");
  assert.equal(cellFilePath("file:///home/agent/results.csv"), "/home/agent/results.csv");
  assert.equal(cellFilePath("/home/agent/main.py:23"), "/home/agent/main.py");
  assert.equal(cellFilePath("/home/agent/main.py#L23-L27"), "/home/agent/main.py");
});

test("file links retain the selected cell and chat without accepting URL parameters from paths", () => {
  globalThis.location = { href: "https://supervisor.example/?cell=project&chat=saved" };
  const path = "/home/agent/a &cell=other #?.csv";
  const url = new URL(fileViewUrl(path), location.href);
  assert.equal(url.searchParams.get("cell"), "project");
  assert.equal(url.searchParams.get("chat"), "saved");
  assert.equal(url.searchParams.get("path"), path);
  assert.equal(url.searchParams.get("view"), "files");
  const download = new URL(fileDownloadUrl("/api/cells/project", path), location.href);
  assert.equal(download.pathname, "/api/cells/project/files/download");
  assert.equal(download.searchParams.get("path"), path);
  delete globalThis.location;
});
