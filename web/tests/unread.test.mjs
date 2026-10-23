import { test } from "node:test";
import assert from "node:assert/strict";
import { activityTime, baseline, isUnread, loadSeen, merge, saveSeen } from "../src/unread.mjs";

const row = (id, updatedAt, status = "idle") => ({ id, status, updatedAt });
class Storage {
  values = new Map();
  getItem(key) { return this.values.get(key) ?? null; }
  setItem(key, value) { this.values.set(key, value); }
}

test("a run that ended after the chat was last open is unread until it is seen again", () => {
  const before = row("a", "2026-10-22T20:00:00.000Z");
  const seen = baseline([before]);
  assert.equal(isUnread(before, seen), false);
  assert.equal(isUnread(row("a", "2026-10-22T20:01:00.000Z", "running"), seen), false);
  const settled = row("a", "2026-10-22T20:02:00.000Z");
  assert.equal(isUnread(settled, seen), true);
  assert.equal(isUnread(settled, { ...seen, a: activityTime(settled) }), false);
  assert.equal(isUnread(row("b", "2026-10-22T20:02:00.000Z"), seen), true, "chats started elsewhere are news");
  assert.equal(isUnread({ id: "c", status: "idle" }, seen), false, "an unknown time is not news");
});

test("tabs merge their sightings instead of overwriting each other", () => {
  const storage = new Storage();
  assert.equal(loadSeen(storage, "k"), null);
  saveSeen(storage, "k", { a: 2, b: 1 });
  saveSeen(storage, "k", { a: 1, c: 3 });
  assert.deepEqual(loadSeen(storage, "k"), { a: 2, b: 1, c: 3 });
  assert.deepEqual(merge({ a: 1 }, { a: "2", b: 2 }), { a: 1, b: 2 });
  storage.setItem("k", "[1]");
  assert.equal(loadSeen(storage, "k"), null);
  storage.setItem("k", "{");
  assert.equal(loadSeen(storage, "k"), null);
});
