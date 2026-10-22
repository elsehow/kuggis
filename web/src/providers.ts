import type { ProviderId } from "./settings-types";

const labels: Record<ProviderId, string> = {
  anthropic: "Anthropic",
  openai: "OpenAI",
  paperinstruments: "Paper Instruments",
};
export const providerIds = Object.keys(labels) as ProviderId[];
export const providerLabel = (id: string) => id === "openai-codex" ? "OpenAI · ChatGPT" : labels[id as ProviderId] ?? id;
