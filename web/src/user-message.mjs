// Presentation only: never alter the prompt sent to the agent or stored history.
// Recognize the host envelope conservatively; ordinary user text stays intact.

// The host appends a scaling caption for attached images; users never type it.
const imageCaption = /\n*\[Image: original \d+x\d+, displayed at \d+x\d+\. Multiply coordinates by [\d.]+ to map to original image\.\]\s*$/;

export function splitUserMessage(text) {
  const start = text.trimStart();
  const prefix = "Cell context (host-published discovery, not an access grant): ";
  const separator = "\n\nUser request:\n";
  const boundary = text.indexOf(separator);
  if (boundary < 0) return { request: stripImageCaption(text), context: null };
  if (start.startsWith(prefix)) {
    const line = start.slice(prefix.length).split("\n", 1)[0];
    try {
      const context = JSON.parse(line);
      if (typeof context.cell?.id !== "string" || !start.includes("\nYour cell identity is the UUID above, not the checkout directory.")) {
        return { request: stripImageCaption(text), context: null };
      }
    } catch { return { request: stripImageCaption(text), context: null }; }
  } else if (!/^You are in cell UUID [\da-f-]+ \(deployed label [^\n]+\n\nUser request:\n/.test(start)) {
    return { request: stripImageCaption(text), context: null };
  }
  const request = text.slice(boundary + separator.length);
  return { request: stripImageCaption(request), context: text.slice(0, boundary).trim() };
}

// Strip the trailing caption so the transcript echo's text matches what the
// user typed; the runtime adapter reconciles its optimistic bubble by text,
// and an unstripped caption leaves that duplicate stuck at the bottom.
function stripImageCaption(request) {
  return request.replace(imageCaption, "");
}
