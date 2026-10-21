import { StrictMode, useEffect, useMemo, useState, type FormEvent } from "react";
import { createRoot } from "react-dom/client";
import {
  AssistantRuntimeProvider, ComposerPrimitive, MessagePrimitive,
  ThreadPrimitive, ThreadListPrimitive, ThreadListItemPrimitive,
  useAui, useAuiState, type ToolCallMessagePartProps, type TextMessagePartProps,
  type ReasoningMessagePartProps,
} from "@assistant-ui/react";
import {
  createPiHttpClient, usePiRuntime, usePiRuntimeExtras, usePiSession,
  usePiHostUiRequests, type PiHostUiRequest, type PiHostUiResponse,
  type PiModelInfo,
} from "@assistant-ui/react-pi";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Settings } from "./settings";
import type { CellInfo, CellsView } from "./api-types";
import { CreateCell, creationLabel } from "./create-cell";
import { DeleteCell, deletionLabel } from "./delete-cell";
import { ModelPicker } from "./model-picker";
import "./style.css";

import { api, explain } from "./api";

function label(cell: CellInfo) { return cell.parent && cell.name.startsWith(cell.parent + "-") ? cell.name.slice(cell.parent.length + 1) : cell.name; }

function App() {
  const [cells, setCells] = useState<CellInfo[]>([]);
  const [mode, setMode] = useState("cells");
  const [canApply, setCanApply] = useState(false);
  const [canCreate, setCanCreate] = useState(false);
  const [canDelete, setCanDelete] = useState(false);
  const [deleting, setDeleting] = useState(new URLSearchParams(location.search).has("deletion"));
  const [deletionId, setDeletionId] = useState(new URLSearchParams(location.search).get("deletion"));
  const [deleteName, setDeleteName] = useState<string | null>(null);
  const [creating, setCreating] = useState(new URLSearchParams(location.search).has("creation"));
  const [creationId, setCreationId] = useState(new URLSearchParams(location.search).get("creation"));
  const [newParent, setNewParent] = useState<string | null>(null);
  const [selected, setSelected] = useState(new URLSearchParams(location.search).get("cell"));
  const [error, setError] = useState("");
  const [loaded, setLoaded] = useState(false);
  const load = async () => {
    try {
      const result = await api<CellsView>("/api/cells");
      setCells(result.cells); setMode(result.mode); setCanApply(result.canApply); setCanCreate(result.canCreate); setCanDelete(result.canDelete); setError("");
    } catch {
      try {
        const info = await api<{cell: string; workspace: string}>("/api/info");
        setCells([{ name: info.cell, parent: null, grants: [], vm: null, lifecycle: { state: "available" }, creation: null, deletion: null }]); setMode("standalone");
      } catch (e) { setError(explain(e)); }
    } finally { setLoaded(true); }
  };
  useEffect(() => { void load(); const pop = () => setSelected(new URLSearchParams(location.search).get("cell")); window.addEventListener("popstate", pop); return () => window.removeEventListener("popstate", pop); }, []);
  const provisioning = cells.some((c) => c.lifecycle.state === "provisioning" || c.lifecycle.state === "deleting");
  useEffect(() => { if (!provisioning) return; const timer = setInterval(() => void load(), 2500); return () => clearInterval(timer); }, [provisioning]);
  const setCreation = (id: string | null) => {
    const url = new URL(location.href); if (id) url.searchParams.set("creation", id); else url.searchParams.delete("creation"); history.replaceState(null, "", url); setCreationId(id);
  };
  const openCreation = (parent: string | null, id: string | null = null) => { setNewParent(parent); setCreation(id); setCreating(true); };
  const setDeletion = (id: string | null) => {
    const url = new URL(location.href); if (id) url.searchParams.set("deletion", id); else url.searchParams.delete("deletion"); history.replaceState(null, "", url); setDeletionId(id);
  };
  const openDeletion = (name: string, id: string | null = null) => { setCreating(false); setCreation(null); setDeleteName(name); setDeletion(id); setDeleting(true); };
  const choose = (name: string | null, chat?: string, setup = false) => {
    const target = cells.find((c) => c.name === name);
    if (name && target?.lifecycle.state === "deleting" && target.deletion) { openDeletion(name, target.deletion.id); return; }
    if (name && target?.lifecycle.state === "provisioning" && target.creation) { openCreation(target.creation.parent, target.creation.id); return; }
    if (name && target?.lifecycle.state === "blocked") {
      if (target.deletion) { openDeletion(name, target.deletion.id); return; }
      if (target.creation) { openCreation(target.creation.parent, target.creation.id); return; }
    }
    if (name && target?.lifecycle.state !== "available") return;
    const url = new URL(location.href); url.search = ""; if (name) url.searchParams.set("cell", name);
    if (chat) url.searchParams.set("chat", chat); if (setup) url.searchParams.set("setup", "1");
    history.pushState(null, "", url); setSelected(name);
  };
  const cell = cells.find((c) => c.name === selected);
  const branch = (parent: string | null): React.ReactNode => <ul>{cells.filter((c) => c.parent === parent).map((c) => <li key={c.name}><div className="cell-row"><button onClick={() => choose(c.name)}>{label(c)}{c.lifecycle.state !== "available" && <small>{c.lifecycle.state === "blocked" ? "Needs attention" : c.lifecycle.state === "provisioning" && c.creation ? creationLabel(c.creation) : c.lifecycle.state === "deleting" && c.deletion ? deletionLabel(c.deletion) : "Not running"}</small>}</button>{canCreate && c.lifecycle.state === "available" && <button className="add-subcell" aria-label={`Add subcell to ${c.name}`} title="Add subcell" onClick={() => openCreation(c.name)}>＋</button>}</div>{branch(c.name)}</li>)}</ul>;
  const creation = creating && <CreateCell cells={cells} parent={newParent} id={creationId} onId={setCreation} onClose={() => { setCreating(false); setCreation(null); void load(); }} onStarted={load} onDelete={canDelete ? openDeletion : undefined} onReady={async (job) => { await load(); setCreating(false); setCreation(null); choose(job.cell.name, job.phase.session.id, true); }} />;
  const deletion = deleting && <DeleteCell name={deleteName} id={deletionId} onId={setDeletion} onClose={() => { setDeleting(false); setDeletion(null); void load(); }} onChanged={load} onDeleted={async () => { await load(); setDeleting(false); setDeletion(null); choose(null); }} />;
  if (!cell) return <><main className="cell-index"><header><h1>Cells</h1>{canCreate && <button className="new-cell-button" onClick={() => openCreation(null)}>＋ Cell</button>}</header>{error && <div role="alert" className="inline-error">{error}<button onClick={() => void load()}>Retry</button></div>}{!loaded ? <p>Loading…</p> : branch(null)}</main>{creation}{deletion}</>;
  const ancestors: CellInfo[] = [];
  let current: CellInfo | undefined = cell;
  while (current) { ancestors.unshift(current); current = cells.find((c) => c.name === current?.parent); }
  return <><CellWorkspace key={cell.name} cell={cell} ancestors={ancestors} choose={choose} standalone={mode === "standalone"} local={mode !== "cells"} canApply={canApply} reload={load} onDelete={canDelete ? () => openDeletion(cell.name, cell.deletion?.id) : undefined} />{creation}{deletion}</>;
}

function CellWorkspace({ cell, ancestors, choose, standalone, local, canApply, reload, onDelete }: { cell: CellInfo; ancestors: CellInfo[]; choose: (n: string | null) => void; standalone: boolean; local: boolean; canApply: boolean; reload: () => Promise<void>; onDelete?: () => void }) {
  const [error, setError] = useState("");
  const [models, setModels] = useState<PiModelInfo[]>([]);
  const [archived, setArchived] = useState(false);
  const [settings, setSettings] = useState(new URLSearchParams(location.search).get("setup") === "1");
  const base = standalone ? "/api" : `/api/cells/${cell.name}`;
  const client = useMemo(() => createPiHttpClient({ baseUrl: `${base}/pi`, fetchImpl(input, init) {
    const headers = new Headers(init?.headers);
    if (init?.method === "POST" && !headers.has("X-Request-ID")) headers.set("X-Request-ID", crypto.randomUUID().replaceAll("-", ""));
    return fetch(input, { ...init, headers });
  } }), [base]);
  const runtime = usePiRuntime({
    client, initialThreadId: new URLSearchParams(location.search).get("chat") ?? undefined, includeArchived: true,
    onError: (e) => setError(explain(e)),
    onThreadIdChange(id) { const url = new URL(location.href); if (id) url.searchParams.set("chat", id); else url.searchParams.delete("chat"); history.replaceState(null, "", url); setError(""); },
  });
  const loadModels = () => client.getAvailableModels().then(setModels).catch((e) => setError(explain(e)));
  useEffect(() => { void loadModels(); }, [client]);
  return <AssistantRuntimeProvider runtime={runtime}>
    <div className="workspace-shell">
      <header className="cell-header"><nav aria-label="Cell"><button onClick={() => choose(null)}>Cells</button>{ancestors.map((c) => <span key={c.name}><span className="slash">/</span><button onClick={() => choose(c.name)} aria-current={c.name === cell.name ? "page" : undefined} disabled={c.lifecycle.state !== "available"}>{label(c)}</button></span>)}</nav>
        <div className="cell-actions">{local && <span className="local-badge">Local</span>}<button className="gear" aria-label="Cell settings" title="Cell settings" disabled={cell.lifecycle.state !== "available"} onClick={() => setSettings(true)}>⚙</button></div>
      </header>
      <div className="app">
        <aside className="sidebar"><ThreadListPrimitive.Root className="thread-list">
          <ThreadListPrimitive.New className="new-chat">＋ New chat</ThreadListPrimitive.New>
          <ThreadListPrimitive.Items components={{ ThreadListItem: ChatItem }} />
          {archived && <><div className="section-label">Archived</div><ThreadListPrimitive.Items archived components={{ ThreadListItem: ChatItem }} /></>}
        </ThreadListPrimitive.Root><label className="archive-toggle"><input type="checkbox" checked={archived} onChange={(e) => setArchived(e.target.checked)} />Archived</label></aside>
        <main className="chat-main"><ChatHeader client={client} models={models} onError={setError} />
          {error && <div className="error-banner" role="alert"><span>{error}</span><button aria-label="Dismiss error" onClick={() => setError("")}>×</button></div>}
          <Chat onError={setError} />
        </main>
      </div>
    </div>
    {settings && <Settings cell={cell} base={base} canApply={canApply} onClose={() => { setSettings(false); const url = new URL(location.href); url.searchParams.delete("setup"); history.replaceState(null, "", url); void loadModels(); }} onApplied={reload} onDelete={onDelete ? () => { setSettings(false); onDelete(); } : undefined} />}
  </AssistantRuntimeProvider>;
}

function ChatItem() {
  const archived = useAuiState((s) => s.threadListItem.status === "archived");
  const remoteId = useAuiState((s) => s.threadListItem.remoteId);
  const session = usePiSession();
  return <ThreadListItemPrimitive.Root className="chat-item">
    <ThreadListItemPrimitive.Trigger className="chat-link"><span className="chat-glyph">◌</span><span>{session && session.id === remoteId ? session.title ?? "New chat" : <ThreadListItemPrimitive.Title fallback="New chat" />}</span></ThreadListItemPrimitive.Trigger>
    {archived ? <ThreadListItemPrimitive.Unarchive className="item-action" title="Restore chat" aria-label="Restore chat">↥</ThreadListItemPrimitive.Unarchive>
      : <ThreadListItemPrimitive.Archive className="item-action" title="Archive chat" aria-label="Archive chat">↧</ThreadListItemPrimitive.Archive>}
  </ThreadListItemPrimitive.Root>;
}

function ChatHeader({ client, models, onError }: { client: ReturnType<typeof createPiHttpClient>; models: PiModelInfo[]; onError: (e: string) => void }) {
  const aui = useAui();
  const session = usePiSession();
  const extras = usePiRuntimeExtras();
  const attached = session && !session.id.startsWith("__");
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState("");
  const [changingModel, setChangingModel] = useState(false);
  useEffect(() => {
    if (attached && session.title !== aui.threadListItem().getState().title) void aui.threads().reload().catch((e) => onError(explain(e)));
  }, [aui, attached, session?.title]);
  const selection = session?.config?.provider && session.config.modelId && session.config.provider !== "unknown" && session.config.modelId !== "unknown" ? `${session.config.provider}/${session.config.modelId}` : "";
  const rename = async () => {
    setEditing(false);
    if (attached && title.trim() && title !== session.title) {
      try { await aui.threadListItem().rename(title.trim()); await extras.refresh(); } catch (e) { onError(explain(e)); }
    }
  };
  return <header className="chat-header">
    {editing ? <input autoFocus aria-label="Chat title" value={title} onChange={(e) => setTitle(e.target.value)} onBlur={() => void rename()} onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); if (e.key === "Escape") { setEditing(false); } }} />
      : <button className="chat-title" disabled={!attached} title="Rename chat" onClick={() => { setTitle(session?.title ?? ""); setEditing(true); }}>{attached ? session.title : "New chat"}</button>}
    <div className="header-controls">
      {extras.status === "running" && <span className="run-status" role="status">Working…</span>}
      <ModelPicker models={models} selection={selection} disabled={changingModel || extras.status === "running"} onSelect={(model) => {
        void (async () => {
          setChangingModel(true);
          try {
            if (attached) await extras.setModel(model);
            else { const { remoteId } = await aui.threadListItem().initialize(); await client.setModel(remoteId, model); }
          } catch (e) { onError(explain(e)); }
          finally { setChangingModel(false); }
        })();
      }} />
    </div>
  </header>;
}

function Chat({ onError }: { onError: (e: string) => void }) {
  const extras = usePiRuntimeExtras();
  const isRunning = useAuiState((s) => s.thread.isRunning);
  return <ThreadPrimitive.Root className="thread">
    <ThreadPrimitive.Viewport className="viewport">

      <div className="messages"><ThreadPrimitive.Messages components={{ UserMessage, AssistantMessage }} /></div>
      <div className="scroll-anchor" />
    </ThreadPrimitive.Viewport>
    <div className="composer-area">
      <HostRequests onError={onError} />
      {extras.lastError && <div role="alert" className="inline-error">{explain(extras.lastError)}</div>}
      {extras.readiness && extras.readiness.state !== "ready" && <div role="status" className="inline-error">{extras.readiness.message}</div>}
      {extras.queue.steering.length + extras.queue.followUp.length > 0 && <div className="queue">
        {[...extras.queue.steering, ...extras.queue.followUp].map((text, i) => <div key={i}><span>Queued</span> {text}</div>)}
        <button onClick={() => void extras.clearQueue().catch((e) => onError(explain(e)))}>Clear queue</button>
      </div>}
      <ComposerPrimitive.Root className="composer">
        <ComposerPrimitive.Input aria-label="Message" placeholder={isRunning ? "Message…" : "Message…"} className="composer-input" autoFocus />
        <div className="composer-footer"><div className="composer-actions">
          {isRunning && <ComposerPrimitive.Cancel className="stop-button">Stop</ComposerPrimitive.Cancel>}
          <ComposerPrimitive.Send className="send-button" aria-label={isRunning ? "Steer agent" : "Send message"}>{isRunning ? "Steer ↑" : "Send ↑"}</ComposerPrimitive.Send>
        </div></div>
      </ComposerPrimitive.Root>
    </div>
  </ThreadPrimitive.Root>;
}

function UserMessage() {
  return <MessagePrimitive.Root className="message user-message"><div className="message-label">You</div><MessagePrimitive.Parts /></MessagePrimitive.Root>;
}
function AssistantMessage() {
  return <MessagePrimitive.Root className="message assistant-message"><div className="message-label">Assistant</div>
    <MessagePrimitive.Parts components={{ Text: MarkdownText, Reasoning, tools: { Fallback: Tool } }} />
    <MessagePrimitive.Error><div className="inline-error">Run failed.</div></MessagePrimitive.Error>
  </MessagePrimitive.Root>;
}
function MarkdownText({ text }: TextMessagePartProps) {
  return <div className="markdown"><Markdown remarkPlugins={[remarkGfm]}>{text}</Markdown></div>;
}
function Reasoning({ text }: ReasoningMessagePartProps) {
  return <details className="reasoning"><summary>Reasoning</summary><Markdown>{text}</Markdown></details>;
}
function Tool({ toolName, args, result, isError, status }: ToolCallMessagePartProps) {
  return <details className={`tool ${isError ? "tool-error" : ""}`} open={status.type === "running"}>
    <summary><span className="tool-symbol">⌘</span><strong>{toolName}</strong><span>{isError ? "Failed" : result !== undefined ? "Finished" : "Running"}</span></summary>
    <pre>{JSON.stringify(args, null, 2)}</pre>
    {result !== undefined && <pre className="tool-result">{typeof result === "string" ? result : JSON.stringify(result, null, 2)}</pre>}
  </details>;
}

function HostRequests({ onError }: { onError: (e: string) => void }) {
  const { requests, respond } = usePiHostUiRequests();
  return requests.map((request) => <HostRequest key={request.id} request={request} respond={respond} onError={onError} />);
}
function HostRequest({ request, respond, onError }: { request: PiHostUiRequest; respond: (r: PiHostUiResponse) => Promise<void>; onError: (e: string) => void }) {
  const [value, setValue] = useState(request.kind === "editor" ? request.prefill ?? "" : "");
  const [busy, setBusy] = useState(false);
  const answer = async (response: PiHostUiResponse) => {
    setBusy(true);
    try { await respond(response); } catch (e) { onError(explain(e)); setBusy(false); }
  };
  const submit = (e: FormEvent) => { e.preventDefault(); void answer({ requestId: request.id, value }); };
  return <form className="host-request" onSubmit={submit}>
    <strong>{request.title}</strong>
    {request.kind === "confirm" && <p>{request.message}</p>}
    {request.kind === "select" && <select aria-label={request.title} value={value} onChange={(e) => setValue(e.target.value)}><option value="" disabled>Choose…</option>{request.options.map((o) => <option key={o}>{o}</option>)}</select>}
    {(request.kind === "input" || request.kind === "editor") && <textarea aria-label={request.title} value={value} onChange={(e) => setValue(e.target.value)} placeholder={request.kind === "input" ? request.placeholder : ""} />}
    <div className="request-actions"><button type="button" disabled={busy} onClick={() => void answer({ requestId: request.id, dismissed: true })}>Dismiss</button>
      {request.kind === "confirm" ? <><button type="button" disabled={busy} onClick={() => void answer({ requestId: request.id, confirmed: false })}>Decline</button><button type="button" disabled={busy} onClick={() => void answer({ requestId: request.id, confirmed: true })}>Confirm</button></> : <button type="submit" disabled={busy || (request.kind === "select" && !value)}>Submit</button>}
    </div>
  </form>;
}

createRoot(document.getElementById("root")!).render(<StrictMode><App /></StrictMode>);
