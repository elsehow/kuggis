#!/usr/bin/env node
if (process.env.KUGGIS_AUTH_METHOD) {
  await import("./fake-auth.mjs");
  await new Promise(() => {});
}
// Deterministic Pi RPC fixture. No network, credentials, or model calls.
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
if (process.argv[2] === "auth") {
  let credential;
  try { credential = JSON.parse(readFileSync(join(process.env.PI_CODING_AGENT_DIR, "auth.json"), "utf8")).anthropic; } catch {}
  process.stdout.write(JSON.stringify(credential ? { status: "ready", authType: credential.type } : { status: "missing" }) + "\n");
  process.exit(credential ? 0 : 1);
}
const option = (name) => process.argv[process.argv.indexOf(name) + 1];
const id = option("--session-id");
const path = join(option("--session-dir"), `${id}.fixture.json`);
let messages = [];
try { messages = JSON.parse(readFileSync(path, "utf8")); } catch {}
let running = false;
let stopped = false;
let steering = [], followUp = [];
const defaultModel = { provider: process.env.KUGGIS_FIXTURE_PROVIDER ?? "fixture", id: "test-model", name: "Test model", reasoning: true };
let model = process.env.KUGGIS_FIXTURE_NO_MODEL ? null : defaultModel;
let thinkingLevel = "off";
let timestamp = Date.now();
const dialogs = new Map();
const emit = (event) => process.stdout.write(JSON.stringify(event) + "\n");
const reply = (command, data = {}) => emit({ type: "response", id: command.id, command: command.type, success: true, data });
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const usage = { input: 10, output: 10, cacheRead: 0, cacheWrite: 0, totalTokens: 20, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } };
const assistant = (content, stopReason = "stop") => ({ role: "assistant", content, api: "fixture", provider: model.provider, model: model.id, usage, stopReason, timestamp: ++timestamp });
const start = (message) => emit({ type: "message_start", message });
const end = (message) => { messages.push(message); writeFileSync(path, JSON.stringify(messages)); emit({ type: "message_end", message }); };
const queue = () => emit({ type: "queue_update", steering, followUp });

async function run(content) {
  running = true; stopped = false;
  emit({ type: "agent_start" }); emit({ type: "turn_start" });
  const system = { role: "system", content: "Internal runtime instructions", timestamp: ++timestamp };
  start(system); end(system);
  const user = { role: "user", content, timestamp: ++timestamp };
  start(user); end(user);
  if (content.includes("[error]") || content.includes("[extra-usage]")) {
    const failure = { ...assistant([], "error"), errorMessage: content.includes("[extra-usage]") ? "400 You\'re out of extra usage" : "Provider usage exhausted" };
    start(failure); end(failure); emit({ type: "agent_end", messages: [failure] });
    running = false; emit({ type: "agent_settled" }); return;
  }
  if (content.includes("[crash]")) { process.exit(7); }
  if (content.includes("[confirm]")) {
    const confirmed = await new Promise((resolve) => {
      dialogs.set("approval", resolve);
      emit({ type: "extension_ui_request", id: "approval", method: "confirm", title: "Fixture permission", message: "Continue the test run?" });
    });
    content += confirmed ? " (confirmed)" : " (declined)";
  }
  if (!stopped) {
    const tool = assistant([{ type: "toolCall", id: `tool-${timestamp}`, name: "read", arguments: { path: "README.md" } }], "toolUse");
    const call = tool.content[0]; start(tool); end(tool);
    emit({ type: "tool_execution_start", toolCallId: call.id, toolName: "read", args: call.arguments });
    const result = { content: [{ type: "text", text: "# Test workspace\nA persistent agent session." }] };
    emit({ type: "tool_execution_update", toolCallId: call.id, toolName: "read", partialResult: result });
    await pause(30);
    emit({ type: "tool_execution_end", toolCallId: call.id, toolName: "read", result, isError: false });
    const output = { role: "toolResult", toolCallId: call.id, toolName: "read", ...result, isError: false, timestamp: ++timestamp };
    start(output); end(output);
    emit({ type: "turn_end" }); emit({ type: "turn_start" });
  }
  const final = assistant([{ type: "text", text: "" }]); start(final);
  emit({ type: "message_update", usage, assistantMessageEvent: { type: "text_start", contentIndex: 0 } });
  const response = `I read the workspace. You said: ${content}. This chat is saved and can be resumed.`;
  for (const part of response.match(/.{1,8}/gs)) {
    if (stopped) break;
    final.content[0].text += part;
    emit({ type: "message_update", usage, assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: part } });
    await pause(content.includes("slow") ? 150 : 25);
  }
  final.stopReason = stopped ? "aborted" : "stop";
  end(final); emit({ type: "turn_end" }); emit({ type: "agent_end", messages: [final], willRetry: false });
  running = false;
  const next = steering.shift() ?? followUp.shift(); queue();
  if (next) { await run(next); } else { emit({ type: "agent_settled", aborted: stopped }); }
}

async function command(c) {
  switch (c.type) {
    case "get_state": return reply(c, { model, thinkingLevel, isStreaming: running, sessionId: id, sessionFile: path, messageCount: messages.length });
    case "get_messages": return reply(c, { messages });
    case "get_entries": return reply(c, { entries: messages.map((message, i) => ({ type: "message", id: String(i), parentId: i ? String(i - 1) : null, message })), leafId: messages.length ? String(messages.length - 1) : null });
    case "get_available_models": return reply(c, { models: [defaultModel, { ...defaultModel, id: "second-model", name: "Second model" }] });
    case "set_model": model = { ...model, provider: c.provider, id: c.modelId }; return reply(c, model);
    case "set_thinking_level": thinkingLevel = c.level; emit({ type: "thinking_level_changed", level: thinkingLevel }); return reply(c);
    case "clear_queue": { const cleared = { steering, followUp }; steering = []; followUp = []; queue(); return reply(c, cleared); }
    case "abort": stopped = true; for (const resolve of dialogs.values()) resolve(false); dialogs.clear(); while (running) await pause(5); return reply(c);
    case "extension_ui_response": dialogs.get(c.id)?.(c.confirmed ?? c.value ?? false); dialogs.delete(c.id); return;
    case "prompt":
      if (c.message.includes("[reject]")) {
        emit({ type: "response", id: c.id, command: c.type, success: false, error: "Fixture refused this prompt" }); return;
      }
      if (c.message.includes("[no-ack]")) {
        // Work reached the runtime, but the process died before acknowledging it.
        messages.push({ role: "user", content: c.message, timestamp: ++timestamp });
        writeFileSync(path, JSON.stringify(messages)); process.exit(8);
      }
      if (running) {
        if (!c.streamingBehavior) break;
        (c.streamingBehavior === "steer" ? steering : followUp).push(c.message); queue(); return reply(c, { disposition: "queued" });
      }
      reply(c, { disposition: "started" }); void run(c.message); return;
  }
  emit({ type: "response", id: c.id, command: c.type, success: false, error: "Unsupported fixture command or missing streamingBehavior" });
}
// Strict LF framing: U+2028 is legal inside a JSON string.
let buffer = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (data) => {
  buffer += data;
  let newline;
  while ((newline = buffer.indexOf("\n")) >= 0) {
    const line = buffer.slice(0, newline); buffer = buffer.slice(newline + 1);
    if (line.trim()) void command(JSON.parse(line));
  }
});
