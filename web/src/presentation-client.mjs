import { splitUserMessage } from "./user-message.mjs";

const request = (text) => splitUserMessage(text).request;
const message = (value) => value.role !== "user" ? value : {
  ...value,
  content: typeof value.content === "string" ? request(value.content) :
    value.content.map((part) => part.type === "text" ? { ...part, text: request(part.text) } : part),
};
const metadata = (value) => ({ ...value, ...(value.queuedMessages ? {
  queuedMessages: value.queuedMessages.map((item) => ({ ...item, content: request(item.content) })),
} : {}) });
const snapshot = (value) => ({ ...value, metadata: metadata(value.metadata), messages: value.messages.map(message) });
const queue = (value) => ({ ...value, steering: value.steering.map(request), followUp: value.followUp.map(request) });
function event(value) {
  if (value.type === "snapshot") return { ...value, snapshot: snapshot(value.snapshot) };
  if (value.type === "queue_update") return queue(value);
  if (value.type === "message_start" || value.type === "message_end") return { ...value, message: message(value.message) };
  return value;
}

// Normalize incoming UI data before react-pi reconciles optimistic sends.
// Outgoing prompts and the agent's durable transcript are never changed.
export function presentationClient(client) {
  return {
    ...client,
    listThreads: async (...args) => (await client.listThreads(...args)).map(metadata),
    getThread: async (...args) => snapshot(await client.getThread(...args)),
    createThread: async (...args) => snapshot(await client.createThread(...args)),
    clearQueue: async (...args) => queue(await client.clearQueue(...args)),
    subscribe: (id, listener, options) => client.subscribe(id, (value) => listener(event(value)), options),
  };
}
