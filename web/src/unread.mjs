// Which chats have news for this browser. `seen` maps a chat ID to the
// activity time it had when this browser last showed it open. The cell moves
// `updatedAt` when a run starts or ends, so a later time means a run finished
// while the chat was elsewhere.

export const activityTime = (row) => Date.parse(row.updatedAt ?? row.createdAt ?? "") || 0;

export const isUnread = (row, seen) => row.status !== "running" && activityTime(row) > (seen[row.id] ?? 0);

// A browser's first visit to a cell has seen everything already there.
export const baseline = (rows) => Object.fromEntries(rows.map((row) => [row.id, activityTime(row)]));

// Tabs share storage but not state; keep the latest sighting from either.
export function merge(a, b) {
  const result = { ...a };
  for (const [id, time] of Object.entries(b)) if (typeof time === "number" && time > (result[id] ?? 0)) result[id] = time;
  return result;
}

export function loadSeen(storage, key) {
  try {
    const value = JSON.parse(storage.getItem(key) ?? "null");
    return value && typeof value === "object" && !Array.isArray(value) ? merge({}, value) : null;
  } catch { return null; }
}

export function saveSeen(storage, key, seen) {
  try { storage.setItem(key, JSON.stringify(merge(loadSeen(storage, key) ?? {}, seen))); } catch { /* Storage can be refused; unread state then lasts one visit. */ }
}
