import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import type { PiModelInfo } from "@assistant-ui/react-pi";
import { latestModels } from "./model-catalog.mjs";
import { providerLabel } from "./providers";

const modelLabel = (model: PiModelInfo) => (model.name ?? model.modelId)
  .replace(/\s*\((?:latest|current)\)\s*/gi, " ")
  .replace(/\s+\b(?:latest|current)\b/gi, "")
  .replace(/\s+/g, " ").trim();
const keyOf = (model: PiModelInfo) => `${model.provider}/${model.modelId}`;

export function ModelPicker({ models, loading, error, onRetry, onConfigure, selection, disabled, onSelect }: {
  models: PiModelInfo[];
  loading: boolean;
  error: string;
  onRetry: () => void;
  onConfigure: () => void;
  selection: string;
  disabled: boolean;
  onSelect: (model: PiModelInfo) => void;
}) {
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const providerButtons = useRef(new Map<string, HTMLButtonElement>());
  const lastProvider = useRef<string | null>(null);
  const [open, setOpen] = useState(false);
  const [provider, setProvider] = useState<string | null>(null);
  const choices = useMemo(() => latestModels(models), [models]);
  const groups = useMemo(() => {
    const grouped = new Map<string, PiModelInfo[]>();
    for (const model of choices) grouped.set(model.provider, [...(grouped.get(model.provider) ?? []), model]);
    return [...grouped].sort(([a], [b]) => a.localeCompare(b));
  }, [choices]);
  const selected = models.find((model) => keyOf(model) === selection);

  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => { if (!root.current?.contains(event.target as Node)) { setOpen(false); setProvider(null); } };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [open]);
  useEffect(() => {
    if (!open) return;
    const target = provider
      ? root.current?.querySelector<HTMLElement>(`[data-model-provider="${CSS.escape(provider)}"] .model-submenu [role="menuitem"]`)
      : lastProvider.current
        ? providerButtons.current.get(lastProvider.current)
        : root.current?.querySelector<HTMLElement>("[role=menuitem]:not(:disabled)");
    if (provider) lastProvider.current = provider;
    else lastProvider.current = null;
    target?.focus();
  }, [open, provider, choices]);

  const keyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      if (provider) setProvider(null);
      else { setOpen(false); trigger.current?.focus(); }
      return;
    }
    if ((event.key === "ArrowLeft" || event.key === "Escape") && provider) {
      event.preventDefault(); lastProvider.current = provider; setProvider(null); return;
    }
    if (event.key === "ArrowRight") {
      const item = (event.target as HTMLElement).closest<HTMLElement>("[data-model-provider]");
      const name = item?.dataset.modelProvider;
      if (name) { event.preventDefault(); setProvider(name); }
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const activeMenu = (document.activeElement as HTMLElement).closest<HTMLElement>(".model-submenu")
      ?? root.current?.querySelector<HTMLElement>(".model-menu");
    const items = [...(activeMenu?.querySelectorAll<HTMLElement>("[role=menuitem]:not(:disabled)") ?? [])]
      .filter((item) => activeMenu?.classList.contains("model-submenu") || !item.closest(".model-submenu"));
    const index = items.indexOf(document.activeElement as HTMLElement);
    const direction = event.key === "ArrowDown" ? 1 : -1;
    items[(index + direction + items.length) % items.length]?.focus();
    event.preventDefault();
  };

  const choose = (model: PiModelInfo) => { onSelect(model); setOpen(false); setProvider(null); trigger.current?.focus(); };
  return <div className="model-picker" ref={root} onKeyDown={keyDown}>
    <button ref={trigger} className="model-picker-trigger" type="button" aria-label={`Model: ${selected ? modelLabel(selected) : "Choose model"}`} aria-haspopup="menu" aria-expanded={open} disabled={disabled} onClick={() => { setOpen((value) => !value); setProvider(null); }}>
      {selected ? modelLabel(selected) : selection || "Choose model"}<span aria-hidden="true">⌄</span>
    </button>
    {open && <div role="menu" aria-label="Choose model" className="model-menu">
      {loading ? <p role="status" className="model-notice">Loading models…</p>
        : error ? <p role="alert" className="model-notice">Could not load models: {error}</p>
        : models.length === 0 && <p role="status" className="model-notice">No models available. Enable a provider and sign in in Cell settings, then choose a model.</p>}
      {(error || models.length === 0) && <>
        <button type="button" role="menuitem" onClick={() => { setOpen(false); setProvider(null); onConfigure(); }}>Provider settings</button>
        <button type="button" role="menuitem" disabled={loading} onClick={onRetry}>Retry loading models</button>
      </>}
      {groups.map(([name, items]) => {
        const current = selected?.provider === name && !choices.some((item) => keyOf(item) === keyOf(selected));
        return <div role="none" className="model-group" data-model-group key={name} data-model-provider={name}>
          <button ref={(element) => { if (element) providerButtons.current.set(name, element); else providerButtons.current.delete(name); }} type="button" role="menuitem" aria-haspopup="menu" aria-expanded={provider === name} onClick={() => setProvider((value) => value === name ? null : name)}>{providerLabel(name)}<span aria-hidden="true">›</span></button>
          {provider === name && <div role="menu" aria-label={`${providerLabel(name)} models`} className="model-submenu">
            {current && selected && <button type="button" role="menuitem" onClick={() => choose(selected)}>{modelLabel(selected)} <small>Current</small></button>}
            {items.map((model) => <button key={keyOf(model)} type="button" role="menuitem" aria-current={keyOf(model) === selection ? "true" : undefined} onClick={() => choose(model)}>{modelLabel(model)}</button>)}
          </div>}
        </div>;
      })}
    </div>}
  </div>;
}
