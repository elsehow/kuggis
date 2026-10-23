import { useEffect, useRef, useState } from "react";
import { api, explain } from "./api";
import type { Migration } from "./generated/Migration";

export function IdentityMigration({ onClose, onApplied }: { onClose: () => void; onApplied: () => Promise<void> }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [plan, setPlan] = useState<Migration>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const initial = useRef<Promise<Migration> | null>(null);
  useEffect(() => {
    dialog.current?.showModal();
    initial.current ??= api<Migration>("/api/identity/plan", { method: "POST" });
    void initial.current.then(setPlan).catch(e => setError(explain(e)));
  }, []);
  const apply = async () => {
    if (!plan) return;
    setBusy(true); setError("");
    try {
      await api(`/api/identity/migrations/${plan.id}/apply`, { method: "POST", body: JSON.stringify({ approval: plan.approval }) });
      await onApplied(); onClose();
    } catch (e) { setError(explain(e)); } finally { setBusy(false); }
  };
  return <dialog ref={dialog} className="settings" aria-labelledby="identity-title" onCancel={e => { e.preventDefault(); if (!busy) onClose(); }}>
    <header><h2 id="identity-title">Enable cell renaming</h2><button aria-label="Close identity migration" disabled={busy} onClick={onClose}>×</button></header>
    <p>Assign permanent IDs to these cells. Chats keep running, and existing links continue to work. Names can then change without moving workspaces or changing access.</p>
    {error && <p role="alert">{error}</p>}
    {plan ? <ul>{plan.registry.cells.map(cell => <li key={cell.id}><strong>{cell.name}</strong> — {cell.vm?.name ?? "Local cell"}<br /><small>{cell.id}</small></li>)}</ul> : !error && <p role="status">Checking cell identities…</p>}
    <footer className="settings-actions"><button disabled={busy} onClick={onClose}>Cancel</button><button className="primary" disabled={busy || !plan} onClick={() => void apply()}>{busy ? "Applying…" : "Assign permanent IDs"}</button></footer>
  </dialog>;
}
