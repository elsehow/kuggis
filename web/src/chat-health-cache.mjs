// A tab-local UI hint, never an authorization or lifecycle decision. Avoid
// persistent storage so a reload/account change starts with a fresh check.
export function createChatHealthCache(ttl = 60_000, now = Date.now) {
  const ready = new Map();
  return {
    get(key) {
      const entry = ready.get(key);
      if (!entry) return null;
      if (now() >= entry.expires) {
        ready.delete(key);
        return null;
      }
      return entry.health;
    },
    set(key, health) {
      ready.delete(key);
      if (health.state === "ready") {
        // Bound memory even when navigating a large fleet of cells.
        for (const [id, entry] of ready) if (now() >= entry.expires) ready.delete(id);
        if (ready.size >= 100) ready.delete(ready.keys().next().value);
        ready.set(key, { health, expires: now() + ttl });
      }
    },
    delete(key) { ready.delete(key); },
  };
}
export const chatHealthCache = createChatHealthCache();
