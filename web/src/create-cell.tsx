import { useEffect, useRef, useState } from "react";
import { cellId, cellBinding } from "./api-types";
import { api, explain } from "./api";
import type { CellInfo, Grant, Creation, CreationStep as Step, CreationPhase as Phase } from "./api-types";
import type { ProviderId } from "./settings-types";
import { ProviderChoices } from "./provider-choices";
import { providerLabel } from "./providers";
import { publishLabel } from "./grants";
import { browserTestingHosts, defaultHosts } from "./default-hosts";

const steps: Record<Step, string> = { preparing: "Preparing…", creating_vm: "Creating cell…", configuring_access: "Configuring access…", installing_runtime: "Setting up workspace…", connecting_parent: "Connecting parent…", opening_chat: "Opening chat…" };
export const creationLabel = (creation: Creation) => creation.phase.state === "provisioning" ? steps[creation.phase.step] : creation.phase.state === "failed" ? "Setup failed" : "";
const describe = (g: Grant) => "provider" in g ? providerLabel(g.provider) : "code" in g ? `${g.code.write ? "Write" : "Read"} ${g.code.repo}${g.code.via ? ` via ${g.code.via}` : ""}` : "host" in g ? g.host : "publish" in g ? publishLabel(g.publish) : "network" in g ? "Public internet (HTTPS)" : g.edge;

export function CreateCell({ cells, parent: initialParent, id, onId, onClose, onStarted, onReady, onDelete }: {
  cells: CellInfo[]; parent: string | null; id: string | null;
  onId: (id: string | null) => void; onClose: () => void; onStarted: () => Promise<void>;
  onReady: (job: Creation & { phase: Extract<Phase, { state: "ready" }> }) => Promise<void>;
  onDelete?: (name: string) => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const nameInput = useRef<HTMLInputElement>(null);
  const [name, setName] = useState("");
  const [parent, setParent] = useState(initialParent ?? "");
  const [repo, setRepo] = useState("");
  const [write, setWrite] = useState(false);
  const [fork, setFork] = useState("");
  const [providers, setProviders] = useState<ProviderId[]>([]);
  const [privateSites, setPrivateSites] = useState(false);
  const [publisher, setPublisher] = useState<string | null>(null);
  useEffect(() => { void api<string | null>("/api/publishing").then(setPublisher).catch(() => {}); }, []);
  const [hosts, setHosts] = useState("");
  const [publicNetwork, setPublicNetwork] = useState(false);
  const [job, setJob] = useState<Creation>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(!!id);
  const delivered = useRef(false);
  const parentCell = cells.find((c) => cellId(c) === parent || cellBinding(c) === parent);
  const canUsePublicNetwork = !parentCell || parentCell.grants.some((g) => "network" in g && g.network === "public");
  const available = cells.filter((c) => c.lifecycle.state === "available");
  const parentName = parentCell?.name ?? parent;
  const fullName = parent && !name.startsWith(parentName + "-") ? `${parentName}-${name}` : name;
  useEffect(() => { dialog.current?.showModal(); nameInput.current?.focus(); }, []);
  const selectParent = (value: string) => {
    setParent(value);
    const grants = cells.find((c) => cellId(c) === value || cellBinding(c) === value)?.grants;
    setPublicNetwork(false);
    setProviders(grants ? grants.flatMap((g) => "provider" in g ? [g.provider] : []) : ["anthropic"]);
    setPrivateSites(!!grants?.some(g => "publish" in g && g.publish === "private"));
    setHosts(defaultHosts(grants).join("\n"));
    const code = grants?.find((g) => "code" in g);
    setRepo(code && "code" in code ? code.code.repo : "");
    setFork(code && "code" in code ? code.code.via ?? "" : ""); setWrite(false);
  };
  useEffect(() => { selectParent(initialParent ?? ""); }, []);
  const edit = () => {
    if (!job) return;
    setName(job.display_name ?? job.cell.name); setParent(job.parent ?? "");
    setProviders(job.cell.grants.flatMap((g) => "provider" in g ? [g.provider] : []));
    setPrivateSites(job.cell.grants.some(g => "publish" in g && g.publish === "private"));
    setHosts(job.cell.grants.flatMap((g) => "host" in g ? [g.host] : []).join("\n"));
    setPublicNetwork(job.cell.grants.some((g) => "network" in g && g.network === "public"));
    const code = job.cell.grants.find((g) => "code" in g);
    setRepo(code && "code" in code ? code.code.repo : "");
    setWrite(code && "code" in code ? code.code.write : false);
    setFork(code && "code" in code ? code.code.via ?? "" : "");
    setJob(undefined); onId(null);
  };
  useEffect(() => {
    if (!id) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const next = await api<Creation>(`/api/creations/${id}`);
        if (stopped) return;
        setJob(next); setLoading(false); setError("");
        if (next.phase.state === "provisioning") timer = setTimeout(poll, 1500);
      } catch (e) { if (!stopped) { setError(explain(e)); setLoading(false); timer = setTimeout(poll, 3000); } }
    };
    void poll();
    return () => { stopped = true; clearTimeout(timer); };
  }, [id, job?.phase.state === "provisioning"]);
  useEffect(() => {
    if (job?.phase.state !== "ready" || delivered.current) return;
    delivered.current = true;
    void onReady(job as Creation & { phase: Extract<Phase, { state: "ready" }> }).catch((e) => { delivered.current = false; setError(explain(e)); });
  }, [job?.phase.state]);
  const work = async (fn: () => Promise<void>) => { setBusy(true); setError(""); try { await fn(); } catch (e) { setError(explain(e)); } finally { setBusy(false); } };
  const preview = () => work(async () => {
    const grants: Grant[] = [
      ...(privateSites ? [{ publish: "private" as const }, ...(publisher ? [{ host: new URL(publisher).hostname }] : [])] : []),
      ...providers.map((provider) => ({ provider })),
      ...[...new Set(hosts.split(/\s+/).filter(Boolean))].map(host => ({ host })),
      ...(publicNetwork && canUsePublicNetwork ? [{ network: "public" as const }] : []),
      ...(repo.trim() ? [{ code: { repo: repo.trim(), write, ...(fork.trim() ? { via: fork.trim() } : {}) } }] : []),
    ];
    const next = await api<Creation>("/api/cells/plan", { method: "POST", body: JSON.stringify({ name: fullName, parent: parent || null, grants }) });
    setJob(next); onId(next.id);
  });
  const start = () => work(async () => {
    const next = await api<Creation>(`/api/creations/${job!.id}/apply`, { method: "POST", body: JSON.stringify({ approval: job!.approval }) });
    setJob(next); await onStarted();
  });
  return <dialog ref={dialog} className="settings create-cell" aria-labelledby="create-title" onCancel={(e) => { e.preventDefault(); if (!busy) onClose(); }}>
    <header><h2 id="create-title">{job ? job.display_name ?? job.cell.name : "New cell"}</h2><button aria-label="Close new cell" disabled={busy} onClick={onClose}>×</button></header>
    {error && <div role="alert" className="inline-error">{error}</div>}
    {loading ? <p role="status">Loading…</p> : job ? <section>
      <div className="creation-location">Cells / {job.display_name ?? job.cell.name}</div>
      {job.phase.state === "review" ? <>
        <ul className="creation-grants">{job.cell.grants.map((g, i) => <li key={i}>{describe(g)}</li>)}</ul>
        <footer className="settings-actions"><button disabled={busy} onClick={edit}>Back</button><button className="primary" disabled={busy} onClick={() => void start()}>{busy ? "Starting…" : "Create cell"}</button></footer>
      </> : job.phase.state === "failed" ? <>
        <div role="alert" className="inline-error">{job.phase.message}</div>
        {job.phase.step && <p>Setup stopped while {steps[job.phase.step].toLocaleLowerCase()}</p>}
        <footer className="settings-actions">{onDelete && <button className="delete-link" disabled={busy} onClick={() => onDelete(job.cell.name)}>Delete cell…</button>}{job.phase.recovery === "review" ? <button className="primary" disabled={busy} onClick={edit}>Review setup</button> : job.phase.recovery === "retry" && <button className="primary" disabled={busy} onClick={() => void start()}>{busy ? "Starting…" : "Retry setup"}</button>}</footer>
      </> : <p role="status">{job.phase.state === "ready" ? "Opening chat…" : job.phase.state === "deleted" ? "Cell deleted." : steps[job.phase.step]}</p>}
    </section> : <form onSubmit={(e) => { e.preventDefault(); void preview(); }}>
      <section className="cell-identity">
        <label>Name<input ref={nameInput} aria-label="Cell name" required pattern="[a-z0-9]+(-[a-z0-9]+)*" maxLength={63} placeholder="dev" value={name} onChange={(e) => setName(e.target.value)} /></label>
        <label>Parent<select aria-label="Parent cell" value={parent} onChange={(e) => selectParent(e.target.value)}><option value="">None</option>{available.map((c) => <option key={cellId(c)} value={cellId(c)}>{c.name}</option>)}</select></label>
      </section>
      <section><h3>Repository</h3><div className="new-cell-repo">
        <input aria-label="Repository" list="parent-repositories" placeholder="owner/repository (optional)" value={repo} onChange={(e) => setRepo(e.target.value)} />
        <datalist id="parent-repositories">{parentCell?.grants.flatMap((g) => "code" in g ? [<option key={`${g.code.repo}/${g.code.via ?? ""}`} value={g.code.repo} />] : [])}</datalist>
        <select aria-label="Repository access" value={write ? "write" : "read"} onChange={(e) => setWrite(e.target.value === "write")}><option value="read">Read</option><option value="write">Write</option></select>
      </div><details open={!!fork}><summary>Fork</summary><input aria-label="Repository fork" placeholder="owner/repository" value={fork} onChange={(e) => setFork(e.target.value)} /></details></section>
      <label><input type="checkbox" checked={privateSites} disabled={!!parentCell && !parentCell.grants.some(g => "publish" in g && g.publish === "private")} onChange={e => setPrivateSites(e.target.checked)} /> Private websites</label>
      {privateSites && !publisher && <p role="status">Connect a publisher to enable website hosting.</p>}
      <ProviderChoices selected={providers} onChange={setProviders} disabled={id => !!parentCell && !parentCell.grants.some(g => "provider" in g && g.provider === id)} />
      <section className="network-access"><h3>Internet access</h3>
        <label><input type="checkbox" checked={publicNetwork && canUsePublicNetwork} disabled={!canUsePublicNetwork} onChange={(e) => setPublicNetwork(e.target.checked)} /> Allow public internet</label>
        <p>{canUsePublicNetwork ? "Access public sites over HTTPS for research and downloads. Private network addresses remain blocked." : "Enable public internet access on the parent cell first."}</p>
      </section>
      <section><h3>Allowed hosts</h3>
        <p>Defaults allow npm packages, Playwright browsers, and Ubuntu browser dependencies. Remove hosts you do not need.</p>
        {parentCell && defaultHosts(parentCell.grants).length < browserTestingHosts.length && <p role="status">Some browser testing hosts are unavailable because the parent does not grant them. Add them to the parent first to enable full browser installation.</p>}
        <textarea aria-label="Allowed hosts" rows={7} value={hosts} onChange={(e) => setHosts(e.target.value)} />
      </section>
      <footer className="settings-actions"><button className="primary" disabled={busy || !name || fullName.length > 63}>{busy ? "Checking…" : "Continue"}</button></footer>
    </form>}
  </dialog>;
}
