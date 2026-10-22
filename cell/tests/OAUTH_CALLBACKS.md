# Browser sign-in callback relay

The operator gateway binds only the explicit loopback address/port published in
Pi's authorization URL. It does so before returning the link to the UI. Pi keeps
ownership of login method selection, PKCE/state, token exchange, credentials and
refresh. The operator submits the complete redirect URL to Pi's existing input
prompt (including OpenAI's issued `client_id`).

A relay belongs to one cell/provider/attempt, expires after at most ten minutes,
and claims a callback only once. It checks the current Pi flow and prompt before
forwarding. Terminal/missing flows and gateway shutdown release the listener;
terminal-flow cleanup polls at one-second intervals. Transport ambiguity is not
retried. The callback redirects to the local gateway's cell settings without
OAuth parameters; the shared provider component polls until **Pi** confirms
completion. Receiving a callback is not success.

Port conflicts never stop or reuse another process's listener. That attempt
remains manual even if the port subsequently becomes free. Concurrent attempts
on different ports work independently; attempts sharing a port retain manual
fallback. Non-loopback redirects (including Anthropic copy-code login) remain
manual. The gateway must run on the browser's machine; this is not a public
callback service. `localhost` callbacks bind IPv4, matching Pi's default.

## Offline verification

- `cargo test --locked --manifest-path cell/Cargo.toml operator::oauth`
  uses an isolated Unix-socket cell/Pi bridge fixture and real loopback HTTP.
  Covers both providers, correct-cell return, completion gating, state/path/flow
  rejection, duplicate and simultaneous callbacks, expiry, cancellation,
  occupied ports and listener release. No credentials are read or written.
- `cd web && node --test tests/oauth-callback.test.mjs` exercises the actual Pi
  extension adapter with simulated runtimes and isolated in-memory credentials.
  Checks callback extraction, complete URL preservation, completion after the
  runtime returns, and absence of codes/tokens in outgoing notices.
- `cd web && npm run build` verifies generated Rust contracts and TypeScript.

These tests do not validate provider availability or real token exchanges.
The existing installed-Pi tests require `KUGGIS_TEST_PI` and otherwise skip;
no provider login or paid model request is needed by the relay tests.

Installed-Pi verification used the repository-pinned Pi 1.3.0 archive with its
SHA-256 verified, installed at `/opt/kuggis-pi-1.3.0/pi`:

```
cd web
KUGGIS_TEST_PI=/opt/kuggis-pi-1.3.0/pi node --test tests/pi-auth.test.mjs tests/oauth-callback.test.mjs
```

All four authentication tests passed. The general RPC smoke test initially
timed out during `createThread` while inheriting the host's Pi configuration.
It now supplies a temporary `PI_CODING_AGENT_DIR` to its child process, isolating
credentials and configuration without changing production startup or timeouts.
The full web suite with installed Pi passes all 20 tests with no skips. These
tests do not exercise a successful real provider token exchange.
