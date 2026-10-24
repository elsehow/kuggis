// Presentation only: never alter the prompt sent to the agent or stored history.
// Recognize the host envelope conservatively; ordinary user text stays intact.
export function splitUserMessage(text) {
  const start = text.trimStart();
  const prefix = "Cell context (host-published discovery, not an access grant): ";
  const separator = "\n\nUser request:\n";
  const boundary = text.indexOf(separator);
  if (boundary < 0) return { request: text, context: null };
  if (start.startsWith(prefix)) {
    const line = start.slice(prefix.length).split("\n", 1)[0];
    try {
      const context = JSON.parse(line);
      if (typeof context.cell?.id !== "string" || !start.includes("\nYour cell identity is the UUID above, not the checkout directory.")) {
        return { request: text, context: null };
      }
    } catch { return { request: text, context: null }; }
  } else if (!/^You are in cell UUID [\da-f-]+ \(deployed label [^\n]+\n\nUser request:\n/.test(start)) {
    return { request: text, context: null };
  }
  return { request: text.slice(boundary + separator.length), context: text.slice(0, boundary).trim() };
}
