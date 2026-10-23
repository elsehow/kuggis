import { useEffect, useRef, useState } from "react";
import { api, explain } from "./api";
import type { Deletion } from "./api-types";
import type { ResourcesView } from "./generated/ResourcesView";
import type { ResourceOwner } from "./generated/ResourceOwner";
import { effectLabel, ownerLabel } from "./resource-labels";

export const deletionLabel = (job: Deletion) => job.phase.state === "failed" ? "Deletion failed" : "Deleting…";

export function DeleteCell({ cellId, id, onId, onClose, onChanged, onDeleted }: {
  cellId: string | null; id: string | null; onId: (id: string) => void;
  onClose: () => void; onChanged: () => Promise<void>; onDeleted: () => Promise<void>;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [job, setJob] = useState<Deletion>();
  const [confirmation, setConfirmation] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [targets, setTargets] = useState<ResourcesView["cells"]>([]);
  const [selections, setSelections] = useState<Record<string, string>>({});
  const [choicesDirty, setChoicesDirty] = useState(false);
  const [manualAck, setManualAck] = useState(false);
  const manual = job?.resources.some(b => b.plan.changes.some(p => p.effects.includes("remote_revocation_requires_manual_action"))) ?? false;
  useEffect(() => { void api<ResourcesView>("/api/resources").then(v => setTargets(v.cells)).catch(() => {}); }, []);
  const done = useRef(false);
  const previewRequest = useRef<{ cellId: string; promise: Promise<Deletion> }>(null);
  useEffect(() => { dialog.current?.showModal(); }, []);
  useEffect(() => {
    if (id || !cellId) return;
    let stopped = false;
    setBusy(true);
    if (previewRequest.current?.cellId !== cellId) previewRequest.current = { cellId, promise: api<Deletion>(`/api/cells/${cellId}/delete-plan`, { method: "POST", body: "{}" }) };
    void previewRequest.current.promise.then((next) => {
      if (!stopped) { setJob(next); onId(next.id); }
    }).catch((e) => { if (!stopped) setError(explain(e)); }).finally(() => { if (!stopped) setBusy(false); });
    return () => { stopped = true; };
  }, [cellId, id]);
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
    if (job?.phase.state !== "deleted" || done.current || manual) return;
    done.current = true;
    void onDeleted().catch((e) => { done.current = false; setError(explain(e)); });
  }, [job?.phase.state, manual]);
  const remove = async () => {
    if (!job || confirmation !== job.cell.name || choicesDirty || (manual && !manualAck)) return;
    setBusy(true); setError("");
    try {
      const next = await api<Deletion>(`/api/deletions/${job.id}/apply`, { method: "POST", body: JSON.stringify({ approval: job.approval, name: confirmation, acknowledge_manual: manualAck }) });
      setJob(next); await onChanged();
    } catch (e) { setError(explain(e)); } finally { setBusy(false); }
  };
  const reviewAgain = async () => {
    if (!job) return;
    setBusy(true); setError("");
    try {
      const choices: Record<string, ResourceOwner> = {};
      for (const backend of job.resources) for (const change of backend.plan.changes) {
        const intent = change.intent;
        const existing = typeof intent === "object" && "transfer" in intent && intent.transfer.visibility === "keep_current" ? (intent.transfer.to === "operator" ? "operator" : intent.transfer.to.cell) : "default";
        const selected = selections[change.resource.id] ?? existing;
        if (selected !== "default") choices[change.resource.id] = selected === "operator" ? "operator" : {cell: selected};
      }
      const next = await api<Deletion>(`/api/cells/${job.cell.id}/delete-plan`, { method: "POST", body: JSON.stringify({ choices }) });
      setConfirmation(""); setSelections({}); setChoicesDirty(false); setManualAck(false); setJob(next); onId(next.id);
    } catch (e) { setError(explain(e)); } finally { setBusy(false); }
  };
  const deleting = job?.phase.state === "deleting";
  return <dialog ref={dialog} className="settings delete-cell" aria-labelledby="delete-title" onCancel={(e) => { e.preventDefault(); if (!busy) onClose(); }}>
    <header><h2 id="delete-title">Delete {job?.cell.name ?? cellId ?? "cell"}?</h2><button aria-label="Close delete cell" disabled={busy} onClick={onClose}>×</button></header>
    {error && <div role="alert" className="inline-error">{error}</div>}
    {!job ? <>{busy && <p role="status">Checking cell…</p>}<footer className="settings-actions"><button disabled={busy} onClick={onClose}>Cancel</button></footer></> : deleting ? <p role="status">{job.phase.state === "deleting" && job.phase.step === "cleaning_up_resources" ? "Freezing resource access and completing approved cleanup…" : job.phase.state === "deleting" && job.phase.step === "disconnecting" ? "Disconnecting…" : job.phase.state === "deleting" && job.phase.step === "removing_declaration" ? "Removing from tree…" : "Deleting VM…"}</p>
      : job.phase.state === "deleted" ? <><p role="status">Cell deleted.</p>{manual && <><p>Provider cleanup is still manual; resources or charges may remain. Retained resources are available in Resources.</p><button onClick={() => void onDeleted()}>Done — I’ll handle provider cleanup</button></>}</> : <form onSubmit={(e) => { e.preventDefault(); void remove(); }}>
        <p>Deletes this cell’s VM, chats, workspace files, and saved credentials. This cannot be undone.</p>
        {job.phase.state === "failed" && <><div role="alert" className="inline-error">{job.phase.message}</div>{job.phase.step && <p>Deletion stopped while {job.phase.step.replaceAll("_", " ")}.</p>}</>}
        <section aria-label="Resource cleanup review"><h3>Resource cleanup</h3><p>Cleanup completes before VM removal. Interrupted requests may already have taken effect; retry reconciles the same operations.</p>
          {!job.resources_reviewed && <p role="alert">This old approval has no resource coverage. Review deletion again before continuing.</p>}
          {job.resources.length === 0 && <p>No resource backends are configured for this review. Resources created outside Kuggis are not discovered or cleaned up automatically.</p>}
          {job.resources.map(backend => <section key={backend.operation}><h4>{backend.backend} — {backend.receipt ? "cleanup completed" : "pending / not yet reconciled"}</h4>
            {backend.plan.changes.length === 0 && <p>No owned/attached resources found. This identity will still be fenced against new resource creation.</p>}
            {backend.plan.changes.map(change => {
              const owned = change.resource.owner !== "operator" && change.resource.owner.cell === backend.plan.cell;
              const intent = change.intent;
              const selected = selections[change.resource.id] ?? (typeof intent === "object" && "transfer" in intent && intent.transfer.visibility === "keep_current" ? (intent.transfer.to === "operator" ? "operator" : intent.transfer.to.cell) : "default");
              return <article className="resource-card" key={change.resource.id}><h5>{change.resource.external_id}</h5><p>{owned ? "Owned by this cell at review" : "Attached access only; the underlying resource is not this cell’s"}. {change.resource.management} {change.resource.kind}.</p>
                {typeof intent === "object" && "transfer" in intent && <p>New custody: {ownerLabel(intent.transfer.to, targets)}</p>}
                <ul>{change.effects.map(effect => <li key={effect}>{effectLabel[effect]}</li>)}</ul>
                {owned && <label>Cleanup choice <select aria-label={`Cleanup ${change.resource.external_id}`} disabled={busy || !!backend.receipt} value={selected} onChange={e => { setSelections({...selections, [change.resource.id]: e.target.value}); setChoicesDirty(true); setConfirmation(""); }}>
                  <option value="default">{change.resource.management === "managed" ? "Unpublish and retain for operator (default)" : "Retain for operator; provider cleanup is manual"}</option>
                  <option value="operator">Keep current visibility/effects under operator custody</option>
                  {targets.filter(c => c.identity !== backend.plan.cell).map(c => <option key={c.identity} value={c.identity}>Keep current visibility/effects and transfer to {c.name}</option>)}
                </select></label>}
              </article>;
            })}
          </section>)}
          {choicesDirty && <button type="button" disabled={busy} onClick={() => void reviewAgain()}>Review updated cleanup choices</button>}
          {manual && <label><input type="checkbox" checked={manualAck} onChange={e => setManualAck(e.target.checked)} /> I accept responsibility for manual provider cleanup/revocation and any continuing charges.</label>}
        </section>
        <label className="delete-confirm">Type <strong>{job.cell.name}</strong> to confirm.<input aria-label="Cell name to delete" autoComplete="off" value={confirmation} onChange={(e) => setConfirmation(e.target.value)} /></label>
        <footer className="settings-actions"><button type="button" disabled={busy} onClick={onClose}>Cancel</button>{!job.resources_reviewed || (job.phase.state === "failed" && job.phase.recovery === "review") ? <button type="button" className="primary" disabled={busy} onClick={() => void reviewAgain()}>{busy ? "Checking…" : "Review deletion"}</button> : <button type="submit" className="danger-button" disabled={busy || choicesDirty || (manual && !manualAck) || confirmation !== job.cell.name}>{busy ? "Starting…" : job.phase.state === "failed" ? "Retry deletion" : "Delete cell"}</button>}</footer>
      </form>}
  </dialog>;
}
