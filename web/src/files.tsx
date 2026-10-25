import { useEffect, useState, type FormEvent } from "react";
import { api, explain } from "./api";
import type { FileListing } from "./generated/FileListing";
import { fileDownloadUrl } from "./file-links";

function size(bytes: number | null) {
  if (bytes === null) return "—";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024, unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit++; }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
}

export function Files({ base, path, navigate }: { base: string; path: string; navigate: (path: string) => void }) {
  const [listing, setListing] = useState<FileListing | null>(null);
  const [input, setInput] = useState(path);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [refresh, setRefresh] = useState(0);
  const [filter, setFilter] = useState("");
  useEffect(() => {
    const controller = new AbortController();
    setInput(path); setError(""); setLoading(true); setListing(null); setFilter("");
    void api<FileListing>(`${base}/files${path ? `?${new URLSearchParams({ path })}` : ""}`, { signal: controller.signal }).then(result => {
      if (!controller.signal.aborted) { setListing(result); setInput(result.path); }
    }).catch(e => { if (!controller.signal.aborted) setError(explain(e)); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [base, path, refresh]);
  const submit = (event: FormEvent) => { event.preventDefault(); if (input === path) setRefresh(v => v + 1); else navigate(input); };
  const download = (value: string, label: string) => <a className="file-download" href={fileDownloadUrl(base, value)} download>{label}</a>;
  const entries = listing?.entries.filter(entry => entry.name.toLowerCase().includes(filter.toLowerCase())) ?? [];
  return <main className="files-main" aria-label="Cell files">
    <header className="files-heading"><div><h1>Files</h1><p>Browse and download files from this cell.</p></div><button onClick={() => setRefresh(v => v + 1)} disabled={loading}>Refresh</button></header>
    <form className="file-location" onSubmit={submit}><label htmlFor="file-path">Path</label><input id="file-path" value={input} placeholder="Workspace" onChange={e => setInput(e.target.value)} spellCheck={false} /><button type="submit">Go</button></form>
    <nav className="file-shortcuts" aria-label="File locations"><button onClick={() => navigate("")}>Workspace</button>{listing && <><button onClick={() => navigate(listing.root)}>{listing.root === "/" ? "Filesystem" : "Workspace root"}</button><button disabled={!listing.parent} onClick={() => listing.parent && navigate(listing.parent)}>↑ Parent folder</button></>}</nav>
    {loading && <p role="status">Loading files…</p>}
    {error && <div role="alert" className="inline-error">{error}<button onClick={() => setRefresh(v => v + 1)}>Retry</button></div>}
    {listing && <>
      <div className="file-summary"><span className="file-current-path">{listing.path}</span>{listing.kind === "directory" ? download(listing.path, "Download folder (.tar.gz)") : listing.kind === "file" ? download(listing.path, "Download file") : null}</div>
      {listing.kind === "file" ? <p>{size(listing.size)} · Download to open this file on your computer.</p> : listing.kind !== "directory" ? <p>This is not a regular file or folder and cannot be downloaded.</p> : <>
        <input className="file-filter" aria-label="Filter files" placeholder="Filter this folder…" value={filter} onChange={e => setFilter(e.target.value)} />
        <div className="file-table-scroll"><table className="file-table"><thead><tr><th>Name</th><th>Size</th><th>Modified</th><th><span className="sr-only">Download</span></th></tr></thead><tbody>
          {entries.map(entry => <tr key={entry.name}><td><button className="file-name" disabled={entry.kind === "unavailable" || entry.kind === "other"} onClick={() => navigate(entry.path)}><span aria-hidden="true">{entry.kind === "directory" ? "▱" : "·"}</span><span>{entry.name}{entry.kind === "directory" ? "/" : ""}</span></button>{entry.symlink && <small>Link</small>}{entry.kind === "unavailable" && <small>Unavailable</small>}{entry.kind === "other" && <small>Special file</small>}</td><td>{size(entry.size)}</td><td>{entry.modified === null ? "—" : new Date(entry.modified * 1000).toLocaleString()}</td><td>{(entry.kind === "file" || entry.kind === "directory") && <a href={fileDownloadUrl(base, entry.path)} download aria-label={`Download ${entry.name}`}>{entry.kind === "directory" ? ".tar.gz ↓" : "Download ↓"}</a>}</td></tr>)}
        </tbody></table></div>
        {!entries.length && <p className="files-empty">{listing.entries.length ? "No matching files." : "This folder is empty."}</p>}
        {listing.truncated && <p role="status">Showing the first 10,000 entries. Enter a specific path to browse further.</p>}
        <p className="files-note">Folder downloads include regular files and folders. Symbolic links, special files, and other mounted filesystems are skipped.</p>
      </>}
    </>}
  </main>;
}
