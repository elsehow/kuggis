import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import ts from "typescript";

const source = await readFile(new URL("../src/grants.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } });
const { sameGrants } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);

test("grant review compares permissions rather than JSON property order", () => {
  const saved = { code: { repo: "example/kuggis", via: "example-workers/kuggis", write: true } };
  const draft = { code: { repo: "Example/Kuggis", write: true, via: "example-workers/kuggis" } };
  assert.equal(sameGrants([saved], [draft]), true);
  assert.equal(sameGrants([saved, { provider: "anthropic" }], [{ provider: "anthropic" }, draft, draft]), true);
  assert.equal(sameGrants([saved], [{ code: { ...draft.code, write: false } }]), false);
  assert.equal(sameGrants([saved], [{ code: { ...draft.code, via: "another/fork" } }]), false);
  assert.equal(sameGrants([saved], [draft, { provider: "openai" }]), false);
});
