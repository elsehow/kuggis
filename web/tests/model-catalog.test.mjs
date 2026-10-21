import test from "node:test";
import assert from "node:assert/strict";
import { latestModels } from "../src/model-catalog.mjs";

const model = (provider, modelId, name) => ({ provider, modelId, name });

test("keeps only the newest explicit version in each model family", () => {
  const models = latestModels([
    model("anthropic", "claude-opus-4-5", "Claude Opus 4.5 (latest)"),
    model("anthropic", "claude-opus-4-6", "Claude Opus 4.6"),
    model("anthropic", "claude-sonnet-4-5", "Claude Sonnet 4.5"),
    model("anthropic", "claude-haiku-4-5", "Claude Haiku 4.5"),
  ]);
  assert.deepEqual(models.map((item) => item.modelId), ["claude-haiku-4-5", "claude-opus-4-6", "claude-sonnet-4-5"]);
});

test("deduplicates aliases without merging distinct families or providers", () => {
  const models = latestModels([
    model("anthropic", "opus-alias", "Claude Opus (latest)"),
    model("anthropic", "claude-opus-4-6", "Claude Opus 4.6"),
    model("anthropic", "claude-sonnet-4-6", "Claude Sonnet 4.6"),
    model("openai", "opus-4-6", "Claude Opus 4.6"),
  ]);
  assert.deepEqual(models.map((item) => `${item.provider}/${item.modelId}`), [
    "anthropic/claude-opus-4-6", "anthropic/claude-sonnet-4-6", "openai/opus-4-6",
  ]);
});

test("keeps distinct numeric family suffixes such as GPT 4o and GPT 4.1", () => {
  const models = latestModels([
    model("openai", "gpt-4o", "GPT-4o"),
    model("openai", "gpt-4o-20250501", "GPT-4o"),
    model("openai", "gpt-4.1", "GPT-4.1"),
    model("openai", "gpt-4.1-20250601", "GPT-4.1"),
  ]);
  assert.deepEqual(models.map((item) => item.modelId), ["gpt-4.1-20250601", "gpt-4o-20250501"]);
});
