import { useEffect, useRef, useState } from "react";
import { api, explain } from "./api";
import type { Deletion } from "./api-types";

export const deletionLabel = (job: Deletion) => job.phase.state === "failed" ? "Deletion failed" : "Deleting…";

export function DeleteCell({ name, id, onId, onClose, onChanged, onDeleted }: {
  name: string | null; id: string | null; onId: (id: string) => void;
  onClose: () => void; onChanged: () => Promise<void>; onDeleted: () => Promise<void>;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [job, setJob] = useState<Deletion>();
  const [confirmation, setConfirmation] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const done = useRef(false);
  const previewRequest = useRef<{ name: string; promise: Promise<Deletion> }>(null);
  useEffect(() => { dialog.current?.showModal(); }, []);
  useEffect(() => {
    if (id || !name) return;
    let stopped = false;
    setBusy(true);
    if (previewRequest.current?.name !== name) previewRequest.current = { name, promise: api<Deletion>(`/api/cells/${name}/delete-plan`, { method: "POST" }) };
    void previewRequest.current.promise.then((next) => {
      if (!stopped) { setJob(next); onId(next.id); }
    }).catch((e) => { if (!stopped) setError(explain(e)); }).finally(() => { if (!stopped) setBusy(false); });
    return () => { stopped = true; };
  }, [name, id]);
  useEffect(() => {
    if (!id) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const next = await api<Deletion>(`/api/deletions/${id}`);
        if (stopped) return;
        setJob(next); setError(""); setBusy(false);
        if (next.phase.state === "deleting") timer = setTimeout(poll, 1500);
      } catch (e) { if (!stopped) { setError(explain(e)); setBusy(false); timer = setTimeout(poll, 3000); } }
    };
    void poll();
    return () => { stopped = true; clearTimeout(timer); };
  }, [id, job?.phase.state === "deleting"]);
  useEffect(() => {
    if (job?.phase.state !== "deleted" || done.current) return;
    done.current = true;
    void onDeleted().catch((e) => { done.current = false; setError(explain(e)); });
  }, [job?.phase.state]);
  const remove = async () => {
    if (!job || confirmation !== job.cell.name) return;
    setBusy(true); setError("");
    try {
      const next = await api<Deletion>(`/api/deletions/${job.id}/apply`, { method: "POST", body: JSON.stringify({ approval: job.approval, name: confirmation }) });
      setJob(next); await onChanged();
    } catch (e) { setError(explain(e)); } finally { setBusy(false); }
  };
  const reviewAgain = async () => {
    if (!job) return;
    setBusy(true); setError("");
    try {
      const next = await api<Deletion>(`/api/cells/${job.cell.name}/delete-plan`, { method: "POST" });
      setConfirmation(""); setJob(next); onId(next.id);
    } catch (e) { setError(explain(e)); } finally { setBusy(false); }
  };
  const deleting = job?.phase.state === "deleting";
  return <dialog ref={dialog} className="settings delete-cell" aria-labelledby="delete-title" onCancel={(e) => { e.preventDefault(); if (!busy) onClose(); }}>
    <header><h2 id="delete-title">Delete {job?.cell.name ?? name ?? "cell"}?</h2><button aria-label="Close delete cell" disabled={busy} onClick={onClose}>×</button></header>
    {error && <div role="alert" className="inline-error">{error}</div>}
    {!job ? <>{busy && <p role="status">Checking cell…</p>}<footer className="settings-actions"><button disabled={busy} onClick={onClose}>Cancel</button></footer></> : deleting ? <p role="status">{job.phase.state === "deleting" && job.phase.step === "disconnecting" ? "Disconnecting…" : job.phase.state === "deleting" && job.phase.step === "removing_declaration" ? "Removing from tree…" : "Deleting VM…"}</p>
      : job.phase.state === "deleted" ? <p role="status">Deleted.</p> : <form onSubmit={(e) => { e.preventDefault(); void remove(); }}>
        <p>Deletes this cell’s VM, chats, workspace files, and saved credentials. This cannot be undone.</p>
        {job.phase.state === "failed" && <><div role="alert" className="inline-error">{job.phase.message}</div>{job.phase.step && <p>Deletion stopped while {job.phase.step.replaceAll("_", " ")}.</p>}</>}
        <label className="delete-confirm">Type <strong>{job.cell.name}</strong> to confirm.<input aria-label="Cell name to delete" autoComplete="off" value={confirmation} onChange={(e) => setConfirmation(e.target.value)} /></label>
        <footer className="settings-actions"><button type="button" disabled={busy} onClick={onClose}>Cancel</button>{job.phase.state === "failed" && job.phase.recovery === "review" ? <button type="button" className="primary" disabled={busy} onClick={() => void reviewAgain()}>{busy ? "Checking…" : "Review deletion"}</button> : <button type="submit" className="danger-button" disabled={busy || confirmation !== job.cell.name}>{busy ? "Starting…" : job.phase.state === "failed" ? "Retry deletion" : "Delete cell"}</button>}</footer>
      </form>}
  </dialog>;
}
