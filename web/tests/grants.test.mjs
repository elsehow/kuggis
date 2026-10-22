import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import ts from "typescript";

const source = await readFile(new URL("../src/grants.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } });
const { sameGrants, preservedGrants } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);

test("grant review compares permissions rather than JSON property order", () => {
  const saved = { code: { repo: "example/kuggis", via: "example-workers/kuggis", write: true } };
  const draft = { code: { repo: "Example/Kuggis", write: true, via: "example-workers/kuggis" } };
  assert.equal(sameGrants([saved], [draft]), true);
  assert.equal(sameGrants([saved, { provider: "anthropic" }], [{ provider: "anthropic" }, draft, draft]), true);
  assert.equal(sameGrants([saved], [{ code: { ...draft.code, write: false } }]), false);
  assert.equal(sameGrants([saved], [{ code: { ...draft.code, via: "another/fork" } }]), false);
  assert.equal(sameGrants([saved], [draft, { provider: "openai" }]), false);
});

test("publication grant identity includes namespace, site, and scope", () => {
  const site = { publish: { site: { namespace: "owner", name: "news" } } };
  assert.equal(sameGrants([site], [{ publish: { site: { name: "news", namespace: "owner" } } }]), true);
  assert.equal(sameGrants([site], [{ publish: { site: { namespace: "other", name: "news" } } }]), false);
  assert.equal(sameGrants([site], [{ publish: { site: { namespace: "owner", name: "report" } } }]), false);
  assert.equal(sameGrants([site], [{ publish: { namespace: "owner" } }]), false);
  assert.equal(sameGrants([{ publish: { namespace: "owner" } }], [{ publish: { namespace: "other" } }]), false);
});

test("ordinary settings changes preserve operator-managed publishing grants", () => {
  const publishing = { publish: { namespace: "owner" } };
  const edge = { edge: "search" };
  assert.deepEqual(preservedGrants([
    { provider: "anthropic" }, { host: "example.com" },
    { code: { repo: "example/tool", write: true } }, publishing, edge,
  ]), [publishing, edge]);
});
