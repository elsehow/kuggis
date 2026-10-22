import { useEffect, useState } from "react";
import { api, explain } from "./api";
import type { ResourcesView } from "./generated/ResourcesView";
import type { ActionView } from "./generated/ActionView";
import type { ResourceRecord } from "./generated/ResourceRecord";
import type { ResourceIntent } from "./generated/ResourceIntent";
import { effectLabel, ownerLabel } from "./resource-labels";

export function Resources() {
  const [view, setView] = useState<ResourcesView>();
  const [action, setAction] = useState<ActionView>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const refresh = async () => setView(await api<ResourcesView>("/api/resources"));
  useEffect(() => {
    void refresh().catch(e => setError(explain(e)));
    const pending = new URLSearchParams(location.search).get("resource_action");
    if (pending) void api<ActionView>(`/api/resources/actions/${encodeURIComponent(pending)}`).then(setAction).catch(e => setError(explain(e)));
  }, []);
  const remember = (next?: ActionView) => {
    const url = new URL(location.href);
    if (next) url.searchParams.set("resource_action", next.id); else url.searchParams.delete("resource_action");
    history.replaceState(null, "", url); setAction(next);
  };
  const work = async (fn: () => Promise<void>) => { setBusy(true); setError(""); setMessage(""); try { await fn(); } catch (e) { setError(explain(e)); } finally { setBusy(false); } };
  const plan = (backend: string, resource: ResourceRecord, intent: ResourceIntent) => work(async () => {
    remember(await api<ActionView>("/api/resources/plan", { method: "POST", body: JSON.stringify({ backend, resource: resource.id, intent }) }));
  });
  const apply = () => work(async () => {
    if (!action) return;
    const result = await api<ActionView>(`/api/resources/actions/${action.id}/apply`, { method: "POST", body: JSON.stringify({ approval: action.approval }) });
    if (result.receipt?.phase.state !== "succeeded") throw new Error("Resource operation is not complete.");
    remember(); setMessage("Resource operation completed."); await refresh();
  });
  return <main className="resources-view" aria-label="Resources">
    <header><h1>Resources</h1><button disabled={busy} onClick={() => void work(refresh)}>Refresh resources</button></header>
    <p>Resources can outlive their cells. Ownership, access, and provider management are separate.</p>
    {error && <div role="alert" className="inline-error">{error}{action && <p>If state changed, cancel and review again. If a reply was lost, retry this same operation to reconcile its outcome.</p>}</div>}
    {message && <p role="status">{message}</p>}
    {action ? <section className="resource-review" aria-label="Review resource operation">
      <h2>Review resource operation</h2><p>{action.plan.resource.external_id}</p>
      <p>Current owner: {ownerLabel(action.plan.resource.owner, view?.cells ?? [])}</p>
      {typeof action.plan.intent === "object" && "transfer" in action.plan.intent && <p>New owner: {ownerLabel(action.plan.intent.transfer.to, view?.cells ?? [])}</p>}
      <ul>{action.plan.effects.map(effect => <li key={effect}>{effectLabel[effect]}</li>)}</ul>
      <p>Approval binds this resource revision and backend. Changed state requires another review.</p>
      <button disabled={busy} onClick={() => remember()}>Cancel review</button>
      <button className="primary" disabled={busy || !!action.receipt} onClick={() => void apply()}>{action.receipt ? "Operation completed" : busy ? "Applying…" : "Apply resource operation"}</button>
    </section> : !view ? <p role="status">Loading resource inventory…</p> : <>
      {view.backends.length === 0 && <p>No resource backends configured. Configure the operator’s private <code>.resources.json</code> file; an empty configuration is not evidence that remote resources do not exist.</p>}
      {view.backends.map(backend => <section key={backend.name} aria-label={`Backend ${backend.name}`}>
        <h2>{backend.name}</h2>
        {backend.error && <div role="alert" className="inline-error">Inventory unavailable: {backend.error}. Cleanup cannot be assumed complete.</div>}
        {backend.inventory?.resources.length === 0 && <p>No registered resources.</p>}
        {backend.inventory?.resources.map(resource => <ResourceCard key={resource.id} resource={resource} cells={view.cells} busy={busy} onPlan={intent => void plan(backend.name, resource, intent)} />)}
      </section>)}
    </>}
  </main>;
}
function ResourceCard({ resource, cells, busy, onPlan }: { resource: ResourceRecord; cells: ResourcesView["cells"]; busy: boolean; onPlan: (intent: ResourceIntent) => void }) {
  const [target, setTarget] = useState("operator");
  const [unpublish, setUnpublish] = useState(true);
  const managed = resource.management === "managed";
  const published = typeof resource.state === "object" && "published" in resource.state;
  const currentOwner = resource.owner === "operator" ? "operator" : resource.owner.cell;
  return <article className="resource-card" aria-label={resource.external_id}>
    <h3>{resource.external_id}</h3>
    <dl><dt>Owner</dt><dd>{ownerLabel(resource.owner, cells)}</dd><dt>Management</dt><dd>{resource.management} · {resource.kind}</dd><dt>Visibility/state</dt><dd>{published ? "Published" : resource.state === "unpublished" ? "Unpublished; data retained" : "Provider state unknown"}</dd></dl>
    <p>Resource ID: <code>{resource.id}</code></p>
    <details><summary>Access ({resource.attachments.length})</summary><ul>{resource.attachments.map(a => <li key={a.cell}>{ownerLabel({cell: a.cell}, cells)} — {a.scope}{a.credential ? " (credential reference configured)" : ""}</li>)}</ul></details>
    {!managed && <p>Externally managed: local changes do not stop provider activity, billing, or credential access.</p>}
    {managed && published && <button disabled={busy} onClick={() => onPlan("unpublish")}>Unpublish site…</button>}
    <label>Transfer custody to <select aria-label={`Transfer ${resource.external_id} to`} value={target} disabled={busy} onChange={e => setTarget(e.target.value)}><option value="operator">Operator</option>{cells.map(c => <option value={c.identity} key={c.identity}>{c.name}</option>)}</select></label>
    {managed && <label><input type="checkbox" checked={unpublish} disabled={busy} onChange={e => setUnpublish(e.target.checked)} /> Unpublish during transfer</label>}
    <button disabled={busy || target === currentOwner} onClick={() => onPlan({transfer: {to: target === "operator" ? "operator" : {cell: target}, visibility: managed && unpublish ? "unpublish" : "keep_current"}})}>Review custody transfer…</button>
  </article>;
}
