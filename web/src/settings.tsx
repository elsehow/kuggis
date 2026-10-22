import { ProviderAuth } from "./provider-auth";
import { ProviderChoices } from "./provider-choices";
import { providerLabel } from "./providers";
import { useEffect, useRef, useState } from "react";
import { api, explain } from "./api";
import { connectionNames, type ConnectionConfig, type ConnectionUpdate, type SettingsView } from "./settings-types";

import type { Grant, CellInfo, ChangePreview } from "./api-types";
import { sameGrants as same, publishLabel, preservedGrants } from "./grants";
type ConnectionDraft = ConnectionConfig & Pick<ConnectionUpdate, "credential">;
type Preview = ChangePreview & { grants: Grant[] };
const grantLabel = (grant: Grant) => "provider" in grant ? providerLabel(grant.provider) : "code" in grant ? `${grant.code.write ? "Write" : "Read"} ${grant.code.repo}${grant.code.via ? ` via ${grant.code.via}` : ""}` : "host" in grant ? `Connect to ${grant.host}` : "publish" in grant ? publishLabel(grant.publish) : `Connect to ${grant.edge}`;

export function Settings({ cell, base, canApply, onClose, onApplied, onDelete }: { cell: CellInfo; base: string; canApply: boolean; onClose: () => void; onApplied: () => Promise<void>; onDelete?: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [detail, setDetail] = useState<SettingsView>();
  const [providers, setProviders] = useState(cell.grants.flatMap((g) => "provider" in g ? [g.provider] : []));
  const [repos, setRepos] = useState(cell.grants.flatMap((g) => "code" in g ? [g.code] : []));
  const [hosts, setHosts] = useState(cell.grants.flatMap((g) => "host" in g ? [g.host] : []).join("\n"));
  const [connections, setConnections] = useState<ConnectionDraft[]>([]);
  const [preview, setPreview] = useState<Preview>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const refresh = async () => {
    const d = await api<SettingsView>(`${base}/settings`); setDetail(d);
    setConnections(connectionNames.map((name) => ({ ...(d.connections.find((c) => c.name === name) ?? { name, url: "", auth: { method: "none" as const } }), credential: { operation: "keep" } })));
  };
  useEffect(() => { dialog.current?.showModal(); void refresh().catch((e) => setError(explain(e))); }, []);
  const work = async (fn: () => Promise<void>) => { setBusy(true); setError(""); setSaved(false); try { await fn(); } catch (e) { setError(explain(e)); } finally { setBusy(false); } };
  const storeConnections = async () => {
    const updates: ConnectionUpdate[] = connections.filter((c) => c.url.trim()).map(({ name, url, credential }) => ({ name, url: url.trim(), credential }));
    await api(`${base}/settings/connections`, { method: "PUT", body: JSON.stringify(updates) });
  };
  const save = () => work(async () => {
    const grantedHosts = new Set(hosts.split(/\s+/).filter(Boolean));
    for (const c of connections.filter((c) => c.url.trim())) { const url = new URL(c.url); if (url.protocol !== "https:") throw new Error("Connections require HTTPS"); grantedHosts.add(url.hostname); }
    const grants: Grant[] = [
      ...providers.map((provider) => ({ provider })),
      ...repos.filter((r) => r.repo.trim()).map((r) => ({ code: { repo: r.repo.trim(), write: r.write, ...(r.via?.trim() ? { via: r.via.trim() } : {}) } })),
      ...[...grantedHosts].map((host) => ({ host })),
      ...preservedGrants(cell.grants),
    ];
    if (same(grants, cell.grants)) { await storeConnections(); await refresh(); setSaved(true); }
    else { if (!canApply) throw new Error("Configuration changes require the UI server's Linux deployment binary."); setPreview({ ...await api<Preview>(`${base}/plan`, { method: "POST", body: JSON.stringify({ grants }) }), grants }); }
  });
  const apply = () => work(async () => {
    await api(`/api/changes/${preview!.id}/apply`, { method: "POST", body: JSON.stringify({ approval: preview!.approval }) });
    setPreview(undefined);
    await onApplied();
    await storeConnections();
    await refresh(); setSaved(true);
  });
  return <dialog ref={dialog} className="settings" onCancel={(e) => { e.preventDefault(); if (!busy) onClose(); }} aria-labelledby="settings-title">
    <header><h2 id="settings-title">{cell.name}</h2><button aria-label="Close settings" disabled={busy} onClick={onClose}>×</button></header>
    {error && <div className="inline-error" role="alert">{error}</div>}
    {preview ? <section className="review"><h3>Review changes</h3><ul>
      {preview.grants.filter((g) => !cell.grants.some((old) => same([g], [old]))).map((g) => <li key={`add-${JSON.stringify(g)}`}>Allow: {grantLabel(g)}</li>)}
      {cell.grants.filter((g) => !preview.grants.some((next) => same([g], [next]))).map((g) => <li key={`remove-${JSON.stringify(g)}`}>Remove: {grantLabel(g)}</li>)}
    </ul><p>The cell will restart. Active runs must finish first.</p><div className="settings-actions"><button disabled={busy} onClick={() => setPreview(undefined)}>Back</button><button className="primary" disabled={busy} onClick={() => void apply()}>{busy ? "Applying…" : "Apply changes"}</button></div></section> : <>
      <ProviderChoices selected={providers} onChange={setProviders} disabled={() => busy}>{id => {
        const provider = detail?.providers.find(p => p.id === id);
        return provider?.granted && <ProviderAuth provider={provider} base={base} refresh={refresh} />;
      }}</ProviderChoices>
      <section><h3>Git repositories</h3>{repos.map((repo, i) => <div className="repo-row" key={i}>
        <input aria-label={`Repository ${i + 1}`} placeholder="owner/repository" value={repo.repo} onChange={(e) => setRepos(repos.map((r, n) => n === i ? { ...r, repo: e.target.value } : r))} />
        <select aria-label={`Repository ${i + 1} access`} value={repo.write ? "write" : "read"} onChange={(e) => setRepos(repos.map((r, n) => n === i ? { ...r, write: e.target.value === "write" } : r))}><option value="read">Read</option><option value="write">Write</option></select>
        <button aria-label={`Remove repository ${i + 1}`} onClick={() => setRepos(repos.filter((_, n) => n !== i))}>×</button>
        <input className="fork" aria-label={`Repository ${i + 1} fork`} placeholder="Fork (optional): owner/repository" value={repo.via ?? ""} onChange={(e) => setRepos(repos.map((r, n) => n === i ? { ...r, via: e.target.value } : r))} />
      </div>)}<button className="text-button" onClick={() => setRepos([...repos, { repo: "", write: false }])}>＋ Repository</button></section>
      <section><h3>Connections</h3>{connections.map((connection, i) => <div className="connection" key={connection.name}><label>{connection.name === "bigbrain" ? "BigBrain" : "Portal"}<input type="url" aria-label={`${connection.name} URL`} placeholder="https://…/mcp" value={connection.url} onChange={(e) => setConnections(connections.map((c, n) => n === i ? { ...c, url: e.target.value } : c))} /></label><input type="password" autoComplete="off" aria-label={`${connection.name} token`} placeholder={connection.credential.operation === "clear" ? "Token will be removed" : connection.auth.method === "bearer" ? "Token saved — enter to replace" : "Bearer token (optional)"} value={connection.credential.operation === "replace" ? connection.credential.value : ""} onChange={(e) => setConnections(connections.map((c, n) => n === i ? { ...c, credential: e.target.value ? { operation: "replace", value: e.target.value } : { operation: "clear" } } : c))} />{connection.auth.method === "bearer" && connection.credential.operation !== "clear" && <button className="text-button" onClick={() => setConnections(connections.map((c, n) => n === i ? { ...c, credential: { operation: "clear" } } : c))}>Remove token</button>}</div>)}
        <details><summary>Allowed hosts</summary><textarea aria-label="Allowed hosts" rows={3} value={hosts} onChange={(e) => setHosts(e.target.value)} /></details>
      </section>
      <footer className="settings-actions">{onDelete && <button className="delete-link" disabled={busy} onClick={onDelete}>Delete cell…</button>}{saved && <span role="status">Saved</span>}<button className="primary" disabled={busy || !detail} onClick={() => void save()}>{busy ? "Saving…" : "Save"}</button></footer>
    </>}
  </dialog>;
}
