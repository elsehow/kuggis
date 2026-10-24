import test from "node:test";
import assert from "node:assert/strict";
import { createChatHealthCache } from "../src/chat-health-cache.mjs";

const ready = { state: "ready", message: "Ready", recovery: null };
test("recent readiness is scoped to a cell and expires without sliding on reads", () => {
  let now = 0;
  const cache = createChatHealthCache(60_000, () => now);
  assert.equal(cache.get("a"), null);
  cache.set("a", ready);
  now = 59_999;
  assert.equal(cache.get("a"), ready);
  assert.equal(cache.get("b"), null);
  now = 60_000;
  assert.equal(cache.get("a"), null);
});
test("only readiness is cached; failures and transport errors invalidate it", () => {
  const cache = createChatHealthCache();
  for (const state of ["stopped", "starting", "failed", "unreachable"]) {
    cache.set("a", ready);
    cache.set("a", { state, message: state, recovery: null });
    assert.equal(cache.get("a"), null);
  }
  cache.set("a", ready);
  cache.delete("a");
  assert.equal(cache.get("a"), null);
  cache.set("a", ready);
  assert.equal(createChatHealthCache().get("a"), null, "fresh tab/reload checks again");
});
