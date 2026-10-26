import test from "node:test";
import assert from "node:assert/strict";
import { PiThreadController } from "@assistant-ui/react-pi";
import { splitUserMessage } from "../src/user-message.mjs";
import { presentationClient } from "../src/presentation-client.mjs";

const envelope = (text) => '\nCell context (host-published discovery, not an access grant): {"cell":{"id":"cell-id"}}\nYour cell identity is the UUID above, not the checkout directory. Extra guidance.\n\nUser request:\n' + text;

test("only splits recognizable host envelopes, preserving the entire user request", () => {
  for (const text of ["hello", "User request:\nhello", "quoted\n" + envelope("hello"), "Cell context (host-published discovery, not an access grant): invalid\n\nUser request:\nhello"]) {
    assert.deepEqual(splitUserMessage(text), { request: text, context: null });
  }
  const request = "hello\n\nUser request:\nkeep this too";
  assert.equal(splitUserMessage(envelope(request)).request, request);
  assert.ok(splitUserMessage(envelope(request)).context);
  assert.equal(splitUserMessage("You are in cell UUID abc-def (deployed label dev, which may be stale). Context unavailable.\n\nUser request:\nhello").request, "hello");
});

function fixture() {
  let listener;
  const image = { type: "image", data: "AAAA", mimeType: "image/png" };
  const raw = { metadata: { id: "t1", status: "idle", queuedMessages: [{ id: "q1", mode: "steer", content: envelope("next") }] }, messages: [
    { role: "user", content: [{ type: "text", text: envelope("hello") }, image] },
    { role: "assistant", content: envelope("do not alter assistant text") },
  ], seq: 1 };
  const transport = {
    getThread: async () => raw, createThread: async () => raw,
    listThreads: async () => [raw.metadata],
    clearQueue: async () => ({ steering: [envelope("next")], followUp: [] }),
    sendMessage: async () => {},
    subscribe: (_id, fn) => { listener = fn; return () => {}; },
  };
  let seq = 1;
  return { client: presentationClient(transport), raw, image, emit: (event) => listener({ threadId: "t1", seq: ++seq, ...event }) };
}

test("normalizes snapshots, live echoes, and queues without mutating transport data or images", async () => {
  const { client, raw, image, emit } = fixture();
  for (const result of [await client.getThread("t1"), await client.createThread()]) {
    assert.equal(result.messages[0].content[0].text, "hello");
    assert.equal(result.messages[0].content[1], image);
    assert.equal(result.messages[1], raw.messages[1]);
    assert.equal(result.metadata.queuedMessages[0].content, "next");
  }
  assert.equal((await client.listThreads())[0].queuedMessages[0].content, "next");
  assert.deepEqual(await client.clearQueue("t1"), { steering: ["next"], followUp: [] });
  let received;
  client.subscribe("t1", (value) => { received = value; });
  emit({ type: "snapshot", snapshot: raw });
  assert.equal(received.snapshot.messages[0].content[0].text, "hello");
  for (const type of ["message_start", "message_end"]) {
    emit({ type, message: { role: "user", content: envelope("hello") } });
    assert.equal(received.message.content, "hello");
  }
  emit({ type: "queue_update", steering: [envelope("next")], followUp: [envelope("later")] });
  assert.deepEqual(received.steering, ["next"]);
  assert.deepEqual(received.followUp, ["later"]);
  assert.equal(raw.messages[0].content[0].text, envelope("hello"));
});

test("trailing host image captions are stripped so typed text and the echo reconcile", () => {
  const typed = "look at this plan";
  const caption = "\n\n[Image: original 968x2180, displayed at 888x2000. Multiply coordinates by 1.09 to map to original image.]";
  assert.equal(splitUserMessage(envelope(typed + caption)).request, typed);
  assert.equal(splitUserMessage("\n" + envelope(typed + caption)).request, typed);
  assert.equal(splitUserMessage(typed + caption).request, typed);
  assert.equal(splitUserMessage("real text [Image: not the host format]").request, "real text [Image: not the host format]");
});

test("host-enriched echoes reconcile optimistic messages instead of leaving a duplicate at the bottom", async () => {
  const { client, emit } = fixture();
  const controller = new PiThreadController(client, "t1");
  controller.connect();
  try {
    await controller.sendMessage({ role: "user", content: [{ type: "text", text: "hello" }], attachments: [], parentId: null, sourceId: null, runConfig: {} });
    assert.equal(controller.getProjectedMessages().length, 1);
    emit({ type: "message_start", message: { role: "user", content: [{ type: "text", text: envelope("hello") }], timestamp: 2 } });
    assert.equal(controller.getProjectedMessages().filter((m) => m.role === "user").length, 1);
  } finally { controller.dispose(); }
});

test("an echoed user message carrying an image caption reconciles against the optimistic bubble", async () => {
  const { client, emit } = fixture();
  const controller = new PiThreadController(client, "t1");
  controller.connect();
  try {
    const typed = "i could select a bigger one of these… OR an external box?";
    const caption = "\n\n[Image: original 968x2180, displayed at 888x2000. Multiply coordinates by 1.09 to map to original image.]";
    await controller.sendMessage({ role: "user", content: [{ type: "text", text: typed }, { type: "image", image: "data:image/png;base64,AAA" }], attachments: [], parentId: null, sourceId: null, runConfig: {} });
    assert.equal(controller.getProjectedMessages().filter((m) => m.role === "user").length, 1);
    emit({ type: "message_start", message: { role: "user", content: [{ type: "text", text: envelope(typed + caption) }, { type: "image", data: "AAAA", mimeType: "image/png" }], timestamp: 2 } });
    emit({ type: "message_end", message: { role: "user", content: [{ type: "text", text: envelope(typed + caption) }, { type: "image", data: "AAAA", mimeType: "image/png" }], timestamp: 2 } });
    emit({ type: "message_start", message: { role: "assistant", content: [{ type: "text", text: "Both work…" }], timestamp: 3 } });
    await new Promise((resolve) => setImmediate(resolve));
    const projected = controller.getProjectedMessages();
    assert.equal(projected.filter((m) => m.role === "user").length, 1);
    // The stale optimistic copy must not trail the reply it precedes.
    assert.notEqual(projected[projected.length - 1].role, "user");
  } finally { controller.dispose(); }
});
