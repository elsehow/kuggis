import { useEffect, useRef, useState } from "react";
import { api, explain } from "./api";
import type { ProviderConnection } from "./settings-types";
import type { AuthFlow } from "./generated/AuthFlow";
import type { LoginMethod } from "./generated/LoginMethod";

export function ProviderAuth({ provider, base, refresh }: { provider: ProviderConnection; base: string; refresh: () => Promise<void> }) {
  const [value, setValue] = useState("");
  const [login, setLogin] = useState<AuthFlow>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  const path = `${base}/settings/providers/${provider.id}`;
  useEffect(() => {
    const url = new URL(location.href);
    const id = url.searchParams.get("signin");
    if (!id || url.searchParams.get("provider") !== provider.id) return;
    let disposed = false;
    void api<AuthFlow>(`${path}/login/${encodeURIComponent(id)}`).then(async flow => {
      if (disposed) return;
      url.searchParams.delete("signin");
      url.searchParams.delete("provider");
      history.replaceState(null, "", url);
      setLogin(flow);
      if (flow.state === "complete") await refreshRef.current();
    }).catch(e => { if (!disposed) setError(explain(e)); });
    return () => { disposed = true; };
  }, [path, provider.id]);
  const active = login && ["starting", "waiting"].includes(login.state) ? login.id : undefined;
  const act = async (fn: () => Promise<void>) => { setBusy(true); setError(""); try { await fn(); } catch (e) { setError(explain(e)); } finally { setBusy(false); } };
  const start = (method: LoginMethod) => void act(async () => {
    setValue("");
    setLogin(await api<AuthFlow>(`${path}/login`, { method: "POST", body: JSON.stringify({ method }) }));
  });
  useEffect(() => {
    if (!active) return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const next = await api<AuthFlow>(`${path}/login/${active}`);
        if (disposed) return;
        setLogin(next);
        if (next.state === "complete") { await refreshRef.current(); return; }
        if (next.state === "failed" || next.state === "cancelled") return;
      } catch (e) { if (!disposed) setError(explain(e)); }
      if (!disposed) timer = setTimeout(() => void poll(), 600);
    };
    void poll();
    return () => {
      disposed = true;
      clearTimeout(timer);
      // Closing settings cancels an unfinished login on the cell as well.
      void api(`${path}/login/${active}`, { method: "DELETE" }).catch(() => {});
    };
  }, [path, active]);
  const credential = provider.credential;
  const supportsOAuth = provider.login_methods.includes("oauth");
  const supportsApiKey = provider.login_methods.includes("api_key");
  const hasCredential = credential.state !== "missing";
  const account = provider.account.state === "verified" ? provider.account : undefined;
  const authLabel = credential.state === "missing" ? "Not signed in"
    : credential.state === "invalid" ? "Invalid saved credential"
    : credential.state === "expired" ? "Sign-in needs refresh"
    : credential.method === "oauth" ? "Subscription sign-in saved" : "API key saved";
  const prompt = login?.state === "waiting" ? login.prompt : undefined;
  const notice = login?.state === "waiting" ? login.notice : undefined;
  const reply = (response: string) => void act(async () => {
    if (!login || !prompt) return;
    await api(`${path}/login/${login.id}`, { method: "POST", body: JSON.stringify({ id: prompt.id, value: response }) });
    setValue("");
    setLogin({ ...login, state: "waiting", notice: notice ?? null, prompt: null });
  });
  return <div className="provider-auth">
    <div className="auth-status">{authLabel}
      {hasCredential && <button disabled={busy || !!active} onClick={() => void act(async () => { await api(`${path}/check`, { method: "POST" }); await refresh(); })}>Check</button>}
      {hasCredential && <button disabled={busy || !!active} onClick={() => void act(async () => { await api(path, { method: "DELETE" }); await refresh(); })}>Disconnect</button>}
      {!active && supportsOAuth && <button disabled={busy} onClick={() => start("oauth")}>Sign in</button>}
      {!active && !supportsOAuth && supportsApiKey && <button disabled={busy} onClick={() => start("api_key")}>Enter API key</button>}
    </div>
    {account && <div className="auth-status" title={`Verified ${new Date(account.checked_at).toLocaleString()}`}>{account.identity.email ?? "Account verified"}{account.identity.plan ? ` · ${account.identity.plan.replaceAll("_", " ")}` : ""}{account.identity.subscription_status ? ` · ${account.identity.subscription_status}` : ""}</div>}
    {provider.authentication.state === "unavailable" && <div role="alert" className="inline-error">{provider.authentication.message}</div>}
    {provider.account.state === "unavailable" && <div role="alert" className="inline-error">{provider.account.message}</div>}
    {provider.last_request && <div className={provider.last_request.outcome.state === "failed" ? "inline-error" : "auth-status"} title={new Date(provider.last_request.observed_at).toLocaleString()}>
      {provider.last_request.outcome.state === "succeeded" ? "Last request succeeded" : provider.last_request.outcome.failure.kind === "extra_usage" ? "Last request rejected: extra usage" : `Last request failed: ${provider.last_request.outcome.failure.message}`}
    </div>}
    {active && <div className="signin-flow">
      {notice && notice.kind !== "info" && notice.url.startsWith("https://") && <a href={notice.url} target="_blank" rel="noreferrer">Continue in browser ↗</a>}
      {notice?.kind === "link" && <p role="status">{login?.relay === "ready" ? "Browser callback ready. Return here after approving." : login?.relay === "forwarded" ? "Response received; waiting for Pi to confirm sign-in…" : "Automatic return unavailable. Paste the code or final redirect URL below."}</p>}
      {notice?.kind === "link" && notice.instructions && <p>{notice.instructions}</p>}
      {notice?.kind === "device_code" && <p>Code: <code>{notice.code}</code></p>}
      {notice?.kind === "info" && <p>{notice.message}</p>}
      {prompt?.kind === "input" && <form onSubmit={e => { e.preventDefault(); reply(value); }}>
        <label>{prompt.message}<input type={prompt.secret ? "password" : "text"} aria-label={prompt.message} placeholder={prompt.placeholder ?? undefined} value={value} onChange={e => setValue(e.target.value)} autoComplete="off" autoFocus /></label>
        <button disabled={busy || !value}>Continue</button>
      </form>}
      {prompt?.kind === "select" && <div role="group" aria-label={prompt.message}>{prompt.options.map(option => <button key={option.value} disabled={busy} onClick={() => reply(option.value)}>{option.label}</button>)}</div>}
      {!prompt && <span role="status">{login?.state === "starting" ? "Starting sign-in…" : "Waiting for sign-in…"}</span>}
      <button className="text-button" disabled={busy} onClick={() => void act(async () => { await api(`${path}/login/${active}`, { method: "DELETE" }); setLogin(undefined); })}>Cancel</button>
    </div>}
    {login?.state === "complete" && <p role="status">Sign-in complete.</p>}
    {login?.state === "failed" && <div role="alert" className="inline-error">{login.message}</div>}
    {!active && supportsOAuth && supportsApiKey && <details><summary>API key</summary><button disabled={busy} onClick={() => start("api_key")}>Enter API key</button></details>}
    {error && <div role="alert" className="inline-error">{error}</div>}
  </div>;
}
