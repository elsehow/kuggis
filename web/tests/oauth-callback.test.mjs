import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import ts from "typescript";

// Exercise the actual extension adapter with a simulated Pi runtime. No real
// credentials, provider HTTP calls, or model requests can reach this harness.
const source = await readFile(new URL("../../cell/src/pi/auth.ts", import.meta.url), "utf8");
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText;
for (const provider of ["openai", "anthropic"]) {
  test(`${provider}: bridge preserves Pi callback coordinates and waits for credential persistence`, async () => {
    const address = provider === "openai" ? "http://127.0.0.1:1455/auth/callback" : "http://localhost:53692/callback";
    const state = "isolated-fixture-state";
    const authorization = new URL("https://provider.invalid/authorize");
    authorization.searchParams.set("redirect_uri", address);
    authorization.searchParams.set("state", state);
    const response = `${address}?state=${state}&code=isolated-code&client_id=issued-client`;
    const credentials = new Map(); // Isolated for each test, never the user's store.
    const notices = [];
    let resolveDone;
    const done = new Promise(resolve => { resolveDone = resolve; });
    let resolveExchange;
    const exchange = new Promise(resolve => { resolveExchange = resolve; });
    let sessionStart;
    const runtime = {
      async login(id, method, interaction) {
        assert.equal(id, provider);
        assert.equal(method, "oauth");
        interaction.notify({ type: "auth_url", url: authorization.href });
        const value = await interaction.prompt({ type: "manual_code", message: "Redirect URL" });
        assert.equal(value, response, "adapter must not strip provider-specific query fields");
        await exchange;
        credentials.set(id, { access: "isolated-access", refresh: "isolated-refresh" });
      },
    };
    const exports = {};
    new Function("require", "exports", "process", compiled)(() => ({ ModelRuntime: { create: async () => runtime } }), exports,
      { env: { KUGGIS_AUTH_PROVIDER: provider, KUGGIS_AUTH_METHOD: "oauth" } });
    exports.default({ on: (_event, handler) => { sessionStart = handler; } });
    sessionStart({}, { mode: "rpc", ui: {
      notify(message) { const notice = JSON.parse(message); notices.push(notice); if (notice.state === "complete" || notice.state === "failed") resolveDone(); },
      async input() {
        assert.deepEqual(notices[0].notice.callback, { address, state });
        assert.equal(credentials.size, 0);
        assert.ok(!notices.some(n => n.state === "complete"));
        resolveExchange();
        return response;
      },
    } });
    await done;
    assert.equal(notices.at(-1).state, "complete");
    assert.equal(credentials.size, 1);
    assert.ok(!JSON.stringify(notices).includes("isolated-access"));
    assert.ok(!JSON.stringify(notices).includes("isolated-code"));
  });
}
