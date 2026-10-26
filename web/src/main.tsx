import { StrictMode, Fragment, createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { createRoot } from "react-dom/client";
import {
  AssistantRuntimeProvider, AttachmentPrimitive, ComposerPrimitive, MessagePrimitive,
  ThreadPrimitive, ThreadListPrimitive, ThreadListItemPrimitive,
  SimpleImageAttachmentAdapter,
  useAui, useAuiEvent, useAuiState, type ToolCallMessagePartProps, type TextMessagePartProps,
  type ReasoningMessagePartProps,
} from "@assistant-ui/react";
import {
  createPiHttpClient, usePiRuntime, usePiRuntimeExtras, usePiSession,
  usePiHostUiRequests, type PiHostUiRequest, type PiHostUiResponse,
  type PiModelInfo, type PiThreadMetadata, type PiThinkingLevel,
} from "@assistant-ui/react-pi";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { MAX_IMAGE_BYTES, MAX_MESSAGE_BODY_BYTES, mib, utf8Length } from "./attachment-limits.mjs";
import { activityTime, baseline, isUnread, loadSeen, saveSeen, type Seen } from "./unread.mjs";
import { cellId, cellBinding } from "./api-types";
import { IdentityMigration } from "./identity-migration";
import { Settings } from "./settings";
import { Resources } from "./resources";
import type { CellInfo, CellsView } from "./api-types";
import type { AttachmentAdapter } from "@assistant-ui/core";
import { CreateCell, creationLabel } from "./create-cell";
import { DeleteCell, deletionLabel } from "./delete-cell";
import { ModelPicker } from "./model-picker";
import { ChatHealthBoundary } from "./chat-health";
import { ChatListLoading } from "./chat-list-loading";
import "./style.css";

import { api, explain } from "./api";
import { presentationClient } from "./presentation-client.mjs";

function label(cell: CellInfo) { return cell.name; }

// Image attachments wrapped in a size guard: providers reject image blocks over
// 5 MiB (see attachment-limits.mjs for the derivation), and a clear client-side
// error beats a provider failure on the whole turn.
const imageAttachments = new SimpleImageAttachmentAdapter();
const guardedImageAttachments: AttachmentAdapter = {
  accept: imageAttachments.accept,
  add({ file }) {
    if (file.size > MAX_IMAGE_BYTES) {
      throw new Error(`${file.name} is too large (${mib(file.size)} MiB); the limit is ${mib(MAX_IMAGE_BYTES)} MiB per image. Resize the image or attach a smaller one.`);
    }
    return imageAttachments.add({ file });
  },
  send: (attachment, options) => imageAttachments.send(attachment, options),
  remove: () => imageAttachments.remove(),
};

const ChatMetadata = createContext<PiThreadMetadata[]>([]);
const SeenChats = createContext<{ seen: Seen; markSeen: (id: string, time: number) => void }>({ seen: {}, markSeen() {} });

function App() {
  const [resources, setResources] = useState(new URLSearchParams(location.search).get("view") === "resources");
  const [cellsHidden, setCellsHidden] = useState(false);
  const [cells, setCells] = useState<CellInfo[]>([]);
  const [mode, setMode] = useState("cells");
  const [canApply, setCanApply] = useState(false);
  const [canCreate, setCanCreate] = useState(false);
  const [canDelete, setCanDelete] = useState(false);
  const [canMigrateIdentity, setCanMigrateIdentity] = useState(false);
  const [migratingIdentity, setMigratingIdentity] = useState(false);
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
      setCells(result.cells); setMode(result.mode); setCanApply(result.canApply); setCanCreate(result.canCreate); setCanDelete(result.canDelete); setCanMigrateIdentity(result.canMigrateIdentity); setError("");
    } catch {
      try {
        const info = await api<{cell: string; workspace: string}>("/api/info");
        setCells([{ id: info.cell, binding: info.cell, can_rename: false, external_host: null, name: info.cell, parent: null, grants: [], vm: null, lifecycle: { state: "available" }, creation: null, deletion: null }]); setMode("standalone");
      } catch (e) { setError(explain(e)); }
    } finally { setLoaded(true); }
  };
  useEffect(() => { void load(); const pop = () => { setSelected(new URLSearchParams(location.search).get("cell")); setResources(new URLSearchParams(location.search).get("view") === "resources"); }; window.addEventListener("popstate", pop); return () => window.removeEventListener("popstate", pop); }, []);
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
    const target = cells.find((c) => cellId(c) === name || cellBinding(c) === name);
    if (name && target?.lifecycle.state === "deleting" && target.deletion) { openDeletion(name, target.deletion.id); return; }
    if (name && target?.lifecycle.state === "provisioning" && target.creation) { openCreation(target.creation.parent, target.creation.id); return; }
    if (name && target?.lifecycle.state === "blocked") {
      if (target.deletion) { openDeletion(name, target.deletion.id); return; }
      if (target.creation) { openCreation(target.creation.parent, target.creation.id); return; }
    }
    if (name && target?.lifecycle.state !== "available") return;
    const url = new URL(location.href); url.search = ""; if (name) url.searchParams.set("cell", name);
    if (chat) url.searchParams.set("chat", chat); if (setup) url.searchParams.set("setup", "1");
    history.pushState(null, "", url); setSelected(name); setResources(false);
  };
  useEffect(() => {
    const match = cells.find(c => cellBinding(c) === selected);
    if (!match || cellId(match) === selected) return;
    const url = new URL(location.href); url.searchParams.set("cell", cellId(match));
    history.replaceState(null, "", url); setSelected(cellId(match));
  }, [cells, selected]);
  const cell = cells.find((c) => cellId(c) === selected || cellBinding(c) === selected);
  // Without a homepage, a missing or stale selection falls through to the first
  // available cell in tree order. Other query parameters (creation/deletion) survive.
  const order = (parent: string | null): CellInfo[] => cells.filter((c) => c.parent === parent).flatMap((c) => [c, ...order(cellId(c))]);
  const fallback = order(null).find((c) => c.lifecycle.state === "available");
  useEffect(() => {
    if (!loaded || cell || !fallback) return;
    const url = new URL(location.href); url.searchParams.set("cell", cellId(fallback)); url.searchParams.delete("chat"); url.searchParams.delete("setup");
    history.replaceState(null, "", url); setSelected(cellId(fallback));
  }, [loaded, cell, fallback?.name]);
  const status = (c: CellInfo) => c.lifecycle.state === "blocked" ? "Needs attention" : c.lifecycle.state === "provisioning" && c.creation ? creationLabel(c.creation) : c.lifecycle.state === "deleting" && c.deletion ? deletionLabel(c.deletion) : "Not running";
  const branch = (parent: string | null): React.ReactNode => <ul>{cells.filter((c) => c.parent === parent).map((c) => <li key={cellId(c)}><div className="cell-row" data-active={c.name === cell?.name}><button className="cell-link" aria-current={c.name === cell?.name ? "page" : undefined} onClick={() => choose(cellId(c))}><span>{label(c)}</span>{c.lifecycle.state !== "available" && <small>{status(c)}</small>}</button>{canCreate && !c.external_host && c.lifecycle.state === "available" && <button className="add-subcell" aria-label={`Add subcell to ${c.name}`} title="Add subcell" onClick={() => openCreation(cellId(c))}>＋</button>}</div>{branch(cellId(c))}</li>)}</ul>;
  const creation = creating && <CreateCell cells={cells} parent={newParent} id={creationId} onId={setCreation} onClose={() => { setCreating(false); setCreation(null); void load(); }} onStarted={load} onDelete={canDelete ? openDeletion : undefined} onReady={async (job) => { await load(); setCreating(false); setCreation(null); choose(job.cell.name, job.phase.session.id, true); }} />;
  const deletion = deleting && <DeleteCell name={deleteName} id={deletionId} onId={setDeletion} onClose={() => { setDeleting(false); setDeletion(null); void load(); }} onChanged={load} onDeleted={async () => { await load(); setDeleting(false); setDeletion(null); choose(null); }} />;
  const standalone = mode === "standalone";
  const cellsPane = !standalone && !cellsHidden && <aside className="cells-pane" aria-label="Cells">
    <div className="pane-title">Cells</div>
    <button className="new-cell-button" aria-current={resources ? "page" : undefined} onClick={() => { const url = new URL(location.href); url.searchParams.set("view", "resources"); history.pushState(null, "", url); setResources(true); }}>Resources</button>
    {canMigrateIdentity && <button className="new-cell-button" onClick={() => setMigratingIdentity(true)}>Enable cell renaming…</button>}
    {canCreate && <button className="new-cell-button" onClick={() => openCreation(null)}>＋ Cell</button>}
    {error && <div role="alert" className="inline-error">{error}<button onClick={() => void load()}>Retry</button></div>}
    <nav className="cell-tree">{!loaded ? <p className="pane-note">Loading…</p> : cells.length === 0 ? <p className="pane-note">No cells yet.</p> : branch(null)}</nav>
  </aside>;
  const content = resources && !standalone ? <Resources /> : cell
    ? <ChatHealthBoundary key={cellBinding(cell)} name={cellId(cell)} label={cell.name} standalone={standalone}>{onUnavailable => <CellWorkspace onUnavailable={onUnavailable} cell={cell} standalone={standalone} local={mode !== "cells"} canApply={canApply} reload={load} onDelete={canDelete && !cell.external_host ? () => openDeletion(cellId(cell), cell.deletion?.id) : undefined} />}</ChatHealthBoundary>
    : <main className="empty-main">{!loaded ? "" : cells.length === 0 ? (canCreate ? "No cells yet. Create one with ＋ Cell." : "No cells are declared.") : "No cell is available. Choose one from the list to see its status."}</main>;
  return <><div className={standalone || cellsHidden ? "shell standalone" : "shell"}>{cellsPane}{content}{!standalone && <button className="cell-menu" aria-label={cellsHidden ? "Show cells" : "Hide cells"} aria-expanded={!cellsHidden} onClick={() => setCellsHidden(!cellsHidden)}>☰</button>}</div>{creation}{deletion}{migratingIdentity && <IdentityMigration onClose={() => setMigratingIdentity(false)} onApplied={load} />}</>;
}

function CellWorkspace({ cell, standalone, local, canApply, reload, onDelete, onUnavailable }: { cell: CellInfo; standalone: boolean; local: boolean; canApply: boolean; reload: () => Promise<void>; onDelete?: () => void; onUnavailable: () => void }) {
  const [error, setError] = useState("");
  const [metadata, setMetadata] = useState<PiThreadMetadata[]>([]);
  const [listError, setListError] = useState("");
  const [models, setModels] = useState<PiModelInfo[]>([]);
  const [modelsLoading, setModelsLoading] = useState(true);
  const [modelsError, setModelsError] = useState("");
  const [changingModel, setChangingModel] = useState(false);
  const [archived, setArchived] = useState(false);
  const pollNow = useRef(() => {});
  const [settings, setSettings] = useState(new URLSearchParams(location.search).get("setup") === "1");
  // Retain a mounted legacy transport during the additive migration. Its alias
  // remains frozen; future mounts use the UUID. Rename never recreates runtime.
  const [base] = useState(() => standalone ? "/api" : `/api/cells/${cellId(cell)}`);
  const preferenceBase = standalone ? "/api" : `/api/cells/${cellBinding(cell)}`;
  const client = useMemo(() => {
    const transport = presentationClient(createPiHttpClient({ baseUrl: `${base}/pi`, async fetchImpl(input, init) {
    const headers = new Headers(init?.headers);
    if (init?.method === "POST" && !headers.has("X-Request-ID")) headers.set("X-Request-ID", crypto.randomUUID().replaceAll("-", ""));
    // Reject oversized bodies before the server does (413) so the error is explainable.
    if (typeof init?.body === "string" && utf8Length(init.body) > MAX_MESSAGE_BODY_BYTES) {
      return Promise.reject(new Error(`Message too large (${mib(utf8Length(init.body))} MiB, limit ${mib(MAX_MESSAGE_BODY_BYTES)} MiB). Remove some images and retry.`));
    }
    try {
      const response = await fetch(input, { ...init, headers });
      if (response.status >= 500) onUnavailable();
      return response;
    } catch (error) {
      if (!(error instanceof DOMException && error.name === "AbortError")) onUnavailable();
      throw error;
    }
  } }));
    const key = `kuggis:model:${preferenceBase}`;
    const remember = (model: { provider: string; modelId: string }) => { try { localStorage.setItem(key, JSON.stringify(model)); } catch {} };
    return { ...transport,
      async listThreads(input: Parameters<typeof transport.listThreads>[0]) {
        try {
          const rows = await transport.listThreads(input);
          setListError("");
          return rows;
        } catch (error) { setListError(explain(error)); throw error; }
      },
      async setModel(id: string, model: { provider: string; modelId: string }) { await transport.setModel(id, model); remember(model); },
      async sendMessage(...args: Parameters<typeof transport.sendMessage>) {
        await transport.sendMessage(...args);
        try {
          const { config } = (await transport.getThread(args[0])).metadata;
          if (config?.provider && config.modelId) remember({ provider: config.provider, modelId: config.modelId });
        } catch { /* Sending succeeded; a preference refresh must not report failure. */ }
      },
      async createThread(input: Parameters<typeof transport.createThread>[0]) {
        const snapshot = await transport.createThread(input);
        let model;
        try { model = JSON.parse(localStorage.getItem(key) ?? "null"); } catch {}
        if (!model) {
          const recent = (await transport.listThreads({ includeArchived: true })).filter((row) => row.id !== snapshot.metadata.id)
            .sort((a, b) => (Date.parse(b.updatedAt ?? b.createdAt ?? "") || 0) - (Date.parse(a.updatedAt ?? a.createdAt ?? "") || 0))
            .find((row) => row.config?.provider && row.config.modelId);
          if (recent?.config) model = { provider: recent.config.provider, modelId: recent.config.modelId };
        }
        if (model?.provider && model?.modelId) {
          await transport.setModel(snapshot.metadata.id, model);
          return transport.getThread(snapshot.metadata.id);
        }
        return snapshot;
      },
    };
  }, [base, onUnavailable]);
  const runtime = usePiRuntime({
    client, initialThreadId: new URLSearchParams(location.search).get("chat") ?? undefined, includeArchived: true,
    isSendDisabled: changingModel,
    adapters: { attachments: guardedImageAttachments },
    onError: (e) => setError(explain(e)),
    onThreadIdChange(id) { const url = new URL(location.href); if (id) url.searchParams.set("chat", id); else url.searchParams.delete("chat"); history.replaceState(null, "", url); setError(""); pollNow.current(); },
  });
  const loadModels = async () => {
    setModelsLoading(true); setModelsError("");
    try { setModels(await client.getAvailableModels()); }
    catch (e) { setModelsError(explain(e)); }
    finally { setModelsLoading(false); }
  };
  useEffect(() => { void loadModels(); }, [client]);
  const seenKey = `kuggis:seen:${preferenceBase}`;
  const [seen, setSeen] = useState(() => loadSeen(localStorage, seenKey));
  useEffect(() => { if (seen) saveSeen(localStorage, seenKey, seen); }, [seen, seenKey]);
  const markSeen = useCallback((id: string, time: number) => setSeen((seen) => !seen || (seen[id] ?? 0) >= time ? seen : { ...seen, [id]: time }), []);
  const seenChats = useMemo(() => ({ seen: seen ?? {}, markSeen }), [seen, markSeen]);
  useEffect(() => {
    let disposed = false, latest = 0;
    const poll = async () => { const n = ++latest; try { const rows = await client.listThreads({ includeArchived: true }); if (!disposed && n === latest) { setMetadata(rows); setSeen((seen) => seen ?? baseline(rows)); } } catch {} };
    void poll(); const timer = setInterval(() => void poll(), 3000);
    // A chat left mid-run would otherwise read as idle until the next tick.
    pollNow.current = () => void poll();
    return () => { disposed = true; clearInterval(timer); pollNow.current = () => {}; };
  }, [client]);
  return <AssistantRuntimeProvider runtime={runtime}><ChatMetadata.Provider value={metadata}><SeenChats.Provider value={seenChats}>
    <div className="workspace">
      <aside className="sidebar" aria-label="Chats">
        <header className="sidebar-header"><span className="sidebar-cell" title={cell.name}>{label(cell)}</span>
          {local && <span className="local-badge">Local</span>}
          <button className="gear" aria-label="Cell settings" title="Cell settings" disabled={cell.lifecycle.state !== "available"} onClick={() => setSettings(true)}>⚙</button>
        </header>
        <ThreadListPrimitive.Root className="thread-list">
          <ThreadListPrimitive.New className="new-chat">＋ New chat</ThreadListPrimitive.New>
          <ChatList archived={archived} error={listError} />
        </ThreadListPrimitive.Root><label className="archive-toggle"><input type="checkbox" checked={archived} onChange={(e) => setArchived(e.target.checked)} />Archived</label></aside>
      <main className="chat-main"><ChatHeader client={client} models={models} modelsLoading={modelsLoading} modelsError={modelsError} reloadModels={loadModels} openSettings={() => setSettings(true)} changingModel={changingModel} setChangingModel={setChangingModel} onError={setError} />
        {error && <div className="error-banner" role="alert"><span>{error}</span><button aria-label="Dismiss error" onClick={() => setError("")}>×</button></div>}
        <Chat onError={setError} />
      </main>
    </div>
    {settings && <Settings cell={cell} base={base} canApply={canApply} onClose={() => { setSettings(false); const url = new URL(location.href); url.searchParams.delete("setup"); history.replaceState(null, "", url); void loadModels(); }} onApplied={reload} onDelete={onDelete ? () => { setSettings(false); onDelete(); } : undefined} />}
  </SeenChats.Provider></ChatMetadata.Provider></AssistantRuntimeProvider>;
}

function ChatList({ archived, error }: { archived: boolean; error: string }) {
  const aui = useAui();
  const loading = useAuiState(s => s.threads.isLoading);
  const count = useAuiState(s => s.threads.threadIds.length + s.threads.archivedThreadIds.length);
  const [retrying, setRetrying] = useState(false);
  const retry = async () => {
    setRetrying(true);
    try { await aui.threads().reload(); } catch { /* The transport exposes the error inline. */ }
    finally { setRetrying(false); }
  };
  if (!count && (retrying || (loading && !error))) return <ChatListLoading />;
  return <>
    {error && <div className="chat-list-error" role="alert"><p>Couldn’t load chats: {error}</p><button disabled={retrying} onClick={() => void retry()}>Retry loading chats</button></div>}
    {!error && !loading && !count && <p className="pane-note">No chats yet.</p>}
    <SortedChats />
    {archived && <><div className="section-label">Archived</div><SortedChats archived /></>}
  </>;
}

function SortedChats({ archived = false }: { archived?: boolean }) {
  const rows = useContext(ChatMetadata);
  const ids = useAuiState((s) => archived ? s.threads.archivedThreadIds : s.threads.threadIds);
  // A new chat's remote ID arrives after its item does; subscribe rather than read once.
  const threadItems = useAuiState((s) => s.threads.threadItems);
  const items = ids.map((id, index) => {
    const remoteId = threadItems.find((item) => item.id === id)?.remoteId;
    const row = rows.find((r) => r.id === remoteId);
    return { id, index, time: row ? activityTime(row) : 0 };
  }).sort((a, b) => b.time - a.time);
  let previous = "";
  return <>{items.map(({ id, index, time }) => {
    const date = new Date(time);
    const today = new Date(); const yesterday = new Date(); yesterday.setDate(today.getDate() - 1);
    const day = !time ? "Other chats" : date.toDateString() === today.toDateString() ? "Today" : date.toDateString() === yesterday.toDateString() ? "Yesterday" : date.toLocaleDateString(undefined, { weekday: "long", month: "short", day: "numeric", ...(date.getFullYear() !== today.getFullYear() ? { year: "numeric" as const } : {}) });
    const heading = day !== previous; previous = day;
    return <Fragment key={id}>{heading && <div className="section-label">{day}</div>}<ThreadListPrimitive.ItemByIndex index={index} archived={archived} components={{ ThreadListItem: ChatItem }} /></Fragment>;
  })}</>;
}

function ChatItem() {
  const archived = useAuiState((s) => s.threadListItem.status === "archived");
  const remoteId = useAuiState((s) => s.threadListItem.remoteId);
  const session = usePiSession();
  const rows = useContext(ChatMetadata);
  const row = rows.find((r) => r.id === remoteId);
  const extras = usePiRuntimeExtras();
  const { seen, markSeen } = useContext(SeenChats);
  const current = session?.id === remoteId;
  // The open chat's own stream reports run status and end time before the next
  // list poll does; whatever it reports while open counts as seen.
  const time = Math.max(row ? activityTime(row) : 0, current && session ? activityTime(session) : 0);
  useEffect(() => { if (current && remoteId && time) markSeen(remoteId, time); }, [current, remoteId, time, markSeen]);
  const running = current ? extras.status === "running" : row?.status === "running";
  const state = current && extras.allHostUiRequests.length > 0 ? "waiting" : running ? "running" : !current && row && isUnread(row, seen) ? "unread" : "idle";
  const label = { waiting: "Input needed", running: "Working", unread: "Unread", idle: undefined }[state];
  return <ThreadListItemPrimitive.Root className="chat-item">
    <ThreadListItemPrimitive.Trigger className="chat-link"><span className={state === "running" ? "chat-glyph spinner" : "chat-glyph"} data-state={state} title={label ?? "Chat"} aria-label={label}>{state === "idle" && "◌"}</span><span>{session && session.id === remoteId ? session.title ?? "New chat" : <ThreadListItemPrimitive.Title fallback="New chat" />}</span></ThreadListItemPrimitive.Trigger>
    {archived ? <ThreadListItemPrimitive.Unarchive className="item-action" title="Restore chat" aria-label="Restore chat"><ArchiveIcon restore /></ThreadListItemPrimitive.Unarchive>
      : <ThreadListItemPrimitive.Archive className="item-action" title="Archive chat" aria-label="Archive chat"><ArchiveIcon /></ThreadListItemPrimitive.Archive>}
  </ThreadListItemPrimitive.Root>;
}

function ArchiveIcon({ restore = false }: { restore?: boolean }) {
  return <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">
    <path d="M4 8v12h16V8M3 4h18v4H3z" />
    <path d={restore ? "M12 17v-6m-3 3 3-3 3 3" : "M12 11v6m-3-3 3 3 3-3"} />
  </svg>;
}

function ChatHeader({ client, models, modelsLoading, modelsError, reloadModels, openSettings, changingModel, setChangingModel, onError }: { client: ReturnType<typeof createPiHttpClient>; models: PiModelInfo[]; modelsLoading: boolean; modelsError: string; reloadModels: () => Promise<void>; openSettings: () => void; changingModel: boolean; setChangingModel: (busy: boolean) => void; onError: (e: string) => void }) {
  const aui = useAui();
  const session = usePiSession();
  const extras = usePiRuntimeExtras();
  const attached = session && !session.id.startsWith("__");
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState("");
  const [refreshModelFor, setRefreshModelFor] = useState<string | null>(null);
  useEffect(() => {
    if (!refreshModelFor || session?.id !== refreshModelFor) return;
    // Initializing a draft attaches its controller asynchronously. Refresh that
    // controller after the write: idle threads do not subscribe to model events.
    setRefreshModelFor(null);
    void extras.refresh().catch((e) => onError(explain(e)));
  }, [refreshModelFor, session?.id, extras.refresh]);
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
      <ModelPicker models={models} loading={modelsLoading} error={modelsError} onRetry={() => void reloadModels()} onConfigure={openSettings} selection={selection} disabled={changingModel || extras.status === "running"} onSelect={(model) => {
        void (async () => {
          setChangingModel(true);
          try {
            if (attached) await extras.setModel(model);
            else {
              const { remoteId } = await aui.threadListItem().initialize();
              await client.setModel(remoteId, model);
              setRefreshModelFor(remoteId);
            }
          } catch (e) { onError(explain(e)); }
          finally { setChangingModel(false); }
        })();
      }} />
      <select aria-label="Model effort" title="Model effort" value={session?.config?.thinkingLevel ?? "off"} disabled={changingModel || extras.status === "running"} onChange={(e) => {
        const level = e.target.value as PiThinkingLevel;
        void (async () => {
          setChangingModel(true);
          try {
            if (attached) await extras.setThinkingLevel(level);
            else { const { remoteId } = await aui.threadListItem().initialize(); await client.setThinkingLevel(remoteId, level); await extras.refresh(); }
          } catch (error) { onError(explain(error)); } finally { setChangingModel(false); }
        })();
      }}>
        {(models.find((m) => `${m.provider}/${m.modelId}` === selection)?.availableThinkingLevels ?? ["off", "minimal", "low", "medium", "high", "xhigh"]).map((level) => <option key={level} value={level}>{level}</option>)}
      </select>
    </div>
  </header>;
}

function Chat({ onError }: { onError: (e: string) => void }) {
  useAuiEvent("composer.attachmentAddError", (payload) => onError(payload.message));
  const extras = usePiRuntimeExtras();
  const isRunning = useAuiState((s) => s.thread.isRunning);
  return <ThreadPrimitive.Root className="thread">
    <ThreadPrimitive.Viewport className="viewport">

      <div className="messages"><ThreadPrimitive.Messages components={{ UserMessage, AssistantMessage }} />
        {extras.status === "running" && <div className="run-indicator" role="status"><span className="spinner" aria-hidden="true" /><span className="visually-hidden">Working…</span></div>}
      </div>
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
        <ComposerPrimitive.Attachments components={{ Attachment: AttachmentChip }} />
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
  return <MessagePrimitive.Root className="message user-message"><div className="message-label">You</div><MessagePrimitive.Parts components={{ Text: MarkdownText, Image: MessageImage }} /></MessagePrimitive.Root>;
}
function MessageImage({ image, filename }: { image: string; filename?: string }) {
  return <img src={image} alt={filename ?? "pasted image"} className="message-image" />;
}
function AttachmentChip() {
  return <AttachmentPrimitive.Root className="attachment-chip">
    <AttachmentPrimitive.unstable_Thumb className="attachment-thumb" />
    <AttachmentPrimitive.Name />
    <AttachmentPrimitive.Remove className="attachment-remove" aria-label="Remove attachment">×</AttachmentPrimitive.Remove>
  </AttachmentPrimitive.Root>;
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
