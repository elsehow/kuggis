// Loaded only by the isolated sign-in process. Pi owns provider protocols,
// credential storage, locking and refresh; this adapts its prompts to RPC UI.
import { ModelRuntime, type ExtensionAPI } from "@earendil-works/pi-coding-agent";
import type { AuthInteraction } from "@earendil-works/pi-ai";

export default function authBridge(pi: ExtensionAPI) {
  pi.on("session_start", (_event, ctx) => {
    if (ctx.mode !== "rpc") throw new Error("Sign-in requires RPC mode");
    const send = (event: object) => ctx.ui.notify(JSON.stringify(event));
    // Return from session_start so RPC can read replies while login waits.
    void (async () => {
      try {
        const runtime = await ModelRuntime.create({ refreshOnCreate: false });
        const provider = process.env.KUGGIS_AUTH_PROVIDER!;
        const method = process.env.KUGGIS_AUTH_METHOD!;
        if (!["anthropic", "openai", "openai-codex", "paperinstruments"].includes(provider)) throw new Error("Unknown provider");
        if (method === "logout") {
          for (const id of provider === "openai" ? ["openai", "openai-codex"] : [provider]) await runtime.logout(id);
        } else {
          if (method !== "oauth" && method !== "api_key") throw new Error("Unknown sign-in method");
          const interaction: AuthInteraction = {
            notify(event) {
              switch (event.type) {
                case "auth_url": send({ state: "notice", notice: { kind: "link", url: event.url, instructions: event.instructions } }); break;
                case "device_code": send({ state: "notice", notice: { kind: "device_code", url: event.verificationUri, code: event.userCode } }); break;
                case "info": send({ state: "notice", notice: { kind: "info", message: event.message } }); break;
                // Keep the actionable URL/code visible while Pi polls.
                case "progress": break;
              }
            },
            async prompt(prompt) {
              // Relay every Pi option, including browser login and its remote
              // redirect-URL fallback. Do not choose a method for the user.
              const description = prompt.type === "select"
                ? { kind: "select", message: prompt.message, options: prompt.options.map(o => ({ value: o.id, label: o.label })) }
                : { kind: "input", message: prompt.message, placeholder: prompt.placeholder, secret: prompt.type === "secret" };
              const value = await ctx.ui.input(JSON.stringify(description), undefined, { signal: prompt.signal });
              if (value === undefined) throw new Error("Sign-in cancelled");
              return value;
            },
          };
          await runtime.login(provider, method, interaction);
        }
        send({ state: "complete" });
      } catch {
        // Provider errors may contain request bodies. Never relay credentials.
        send({ state: "failed", message: "Pi could not complete sign-in. Try again." });
      }
    })();
  });
}
