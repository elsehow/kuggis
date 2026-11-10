# Pi deployment recipe

Pi is a manual recipe. `kuggis cell create` no longer installs it, and Kuggis no
longer supplies it a model through exe's managed gateway: `admin upgrade` removes
that grant from existing cells. Existing Pi chats keep their history; give Pi a
model by provisioning your own provider credential as described below.

Pi is installed inside the guest as `agent`. Kuggis supplies its ordinary launch
argv, package list, network endpoints and credential files. This recipe adds no
Pi/provider enum, authentication UI, approval protocol or model client to Kuggis.

`release.json` pins Pi 1.3.0's standalone Linux archives and SHA-256 digests.
`install.py` verifies the archive, installs it under `~/.local/lib/pi/1.3.0`, and
links `~/.local/bin/pi`. It can be rerun for the same release. It must run inside
the guest as `agent`; never run an uploaded recipe as host root.

`settings.json` sets Pi's `defaultProjectTrust` to `always`. The installer merges
this into `~/.pi/agent/settings.json`, preserving other settings. Pi's built-in
tools execute without per-call approvals. Authority remains the configured guest
and remote-resource boundary. See the pinned [Pi security documentation](https://github.com/earendil-works/pi/blob/v1.3.0/packages/coding-agent/docs/security.md).

## Install

Start from `workspace.json`; set the SSH target/configuration and keep the name
of an existing workspace when reusing its host. Its package declarations install
Ubuntu's `fd-find` and `ripgrep`. The recipe supplies the `fd` executable alias
expected by Pi. Apply the declaration with `kuggis setup CONFIG --bin LINUX_BIN`
before running the installer. Use `shell` during preparation; the configured Pi
launch is only started on explicit `connect`.

Download the matching architecture's URL from `release.json` on the operator
machine. Upload it and the three small recipe files with ordinary transfers:

```sh
kuggis file put CONFIG pi-linux-x64.tar.gz .setup/pi/pi.tar.gz --replace
kuggis file put CONFIG recipes/pi/release.json .setup/pi/release.json --replace
kuggis file put CONFIG recipes/pi/settings.json .setup/pi/settings.json --replace
kuggis file put CONFIG recipes/pi/install.py .setup/pi/install.py --replace
kuggis cell shell CONFIG
```

Inside that shell:

```sh
python3 .setup/pi/install.py .setup/pi/pi.tar.gz
~/.local/bin/pi --version
exit
```

The installer is noninteractive and needs no network access. Downloading on the
operator side avoids permanent GitHub/npm grants just to install the agent.

## Provision your own model credential

The example intentionally contains no provider credentials or inferred model
grants. Add the selected provider's exact HTTPS endpoints and an explicit
credential source to the declaration. For example, for an Anthropic API key
already stored in a root-owned, mode-0600 Pi `auth.json` on the deployment host:

```json
{
  "name": "model-provider",
  "endpoints": [{"host": "api.anthropic.com", "port": 443}],
  "credentials": [{
    "source": "/etc/kuggis/secrets/pi-auth.json",
    "target": ".pi/agent/auth.json"
  }]
}
```

This is one resource entry, not a complete workspace. The source and all its
ancestors must satisfy Kuggis's trusted-file checks. Pi's auth file maps provider
names to credentials; an API-key entry has `type: "api_key"` and `key`. Keep the
real values out of Git, launch argv, logs and chat. See [Pi's credential format](https://github.com/earendil-works/pi/blob/v1.3.0/packages/coding-agent/docs/providers.md).

Append `"--model", "PROVIDER/MODEL_ID"` to `launch` for a deployment-selected
model. Apply the declaration, then check readiness **inside the guest**:

```sh
~/.local/bin/pi auth check --provider PROVIDER --json
```

Do not use `--credentials` or either credential-printing command for diagnostics.
The readiness check resolves a credential; it does not prove the provider accepts
it. Verify with a small real `--print` task that writes and reads a disposable
file, then use `connect`. OAuth credentials may require additional token-refresh
endpoints; only add those required by the selected provider. Reapplying a source
OAuth file can overwrite a newer guest-refreshed token, so an old snapshot is not
a renewable deployment secret. Prefer a scoped API key for this first recipe.

`--offline` disables Pi's background startup downloads/catalog refreshes, while
explicit model calls still use the configured provider. `--continue` restores
Pi's most recent conversation for the workspace; Kuggis never resubmits a prompt.
An SSH detach leaves Pi running. After a reboot, only explicit `connect` launches
it again. These flags are described in [Pi's CLI documentation](https://github.com/earendil-works/pi/blob/v1.3.0/packages/coding-agent/docs/cli.md).

The create recipe also sets `chat_launch` to Pi with `--offline --session-dir
/home/agent/.kuggis/chats/{chat} --continue`. Use `kuggis chat list NAME --chat
research` for a separate persistent process and native session directory. The
default launch/history stays unchanged. A new name begins with an empty history;
reconnecting to an existing name attaches to its process or resumes its latest
saved conversation if that process exited. `/new`, `/name`, and `/resume` remain
native Pi commands within that history directory. These chats share project files
and Pi credentials/settings. Never point two named-chat recipes at the same
session file or shared `--continue` directory.

## Test status

For a repeatable live check, upload `smoke.py` with `kuggis put` and run it
inside the guest as `agent`:

```sh
python3 .setup/pi/smoke.py PROVIDER/MODEL_ID
```

This makes a real model request, verifies write/bash tool calls and exact file
contents in a temporary directory, and removes its test files afterward.

The x86-64 archive checksum and executable version were verified on a live exe
guest, and `connect` opens Pi's TUI. The earlier model round trip used the
retired exe gateway; ARM and independent provider credentials have not been
exercised.

The shell-only acceptance suite deliberately refuses a Pi launch configuration:
it must never send its shell commands into an agent as prompts.
