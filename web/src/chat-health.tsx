import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { api, explain } from "./api";
import type { ChatHealth } from "./generated/ChatHealth";
import type { ChatRecovery } from "./generated/ChatRecovery";

const recoveryLabels: Record<ChatRecovery, string> = { start: "Start chat", repair: "Repair chat", restart: "Retry starting chat" };

export function ChatHealthBoundary({ name, standalone, children }: {
  name: string; standalone: boolean;
  children: (onUnavailable: () => void) => ReactNode;
}) {
  const [health, setHealth] = useState<ChatHealth | null>(null);
  const [busy, setBusy] = useState(false);
  const generation = useRef(0);
  const diagnosing = useRef(false);
  const base = `/api/cells/${encodeURIComponent(name)}`;
  const check = useCallback(async (autoStart = false, action?: ChatRecovery) => {
    const current = ++generation.current;
    setBusy(true);
    try {
      let result = action
        ? await api<ChatHealth>(`${base}/recovery`, { method: "POST", body: JSON.stringify({ action }) })
        : await api<ChatHealth>(`${base}/health`);
      if (current !== generation.current) return;
      if (autoStart && result.state === "stopped" && result.recovery === "start") {
        setHealth({ state: "starting", message: "Starting chat…", recovery: null });
        result = await api<ChatHealth>(`${base}/recovery`, { method: "POST", body: JSON.stringify({ action: "start" }) });
      }
      // Wait briefly for startup, then leave a retry control. Never turn a
      // persistent crash into an automatic restart loop or replay a message.
      for (let attempt = 0; result.state === "starting" && attempt < 5; attempt++) {
        if (current !== generation.current) return;
        setHealth(result);
        await new Promise(resolve => setTimeout(resolve, 1000));
        if (current !== generation.current) return;
        result = await api<ChatHealth>(`${base}/health`);
      }
      if (current === generation.current) setHealth(result);
    } catch (error) {
      if (current === generation.current) setHealth({ state: "unreachable", message: explain(error), recovery: null });
    } finally {
      if (current === generation.current) setBusy(false);
    }
  }, [base]);
  useEffect(() => {
    if (!standalone) void check(true);
    return () => { generation.current++; };
  }, [standalone, check]);
  const onUnavailable = useCallback(() => {
    if (standalone || diagnosing.current) return;
    diagnosing.current = true;
    void check().finally(() => { diagnosing.current = false; });
  }, [standalone, check]);
  if (standalone || health?.state === "ready") return children(onUnavailable);
  return <main className="chat-health">
    <h1>{name}</h1>
    <section role={health && health.state !== "starting" ? "alert" : "status"}>
      <h2>{!health ? "Checking chat…" : health.state === "starting" ? "Starting chat…" : "Chat service unavailable"}</h2>
      <p>{health?.message ?? "Checking the cell’s chat service."}</p>
      {health?.recovery && <button disabled={busy} onClick={() => void check(false, health.recovery ?? undefined)}>{recoveryLabels[health.recovery]}</button>}
      {health && <button disabled={busy} onClick={() => void check()}>{busy ? "Checking…" : "Check again"}</button>}
    </section>
  </main>;
}
