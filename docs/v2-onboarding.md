# Kuggis v2 onboarding

Status: the initial exe/Pi create flow is implemented. See the
[current CLI guide](../v2/README.md#create-an-exe-workspace) for supported inputs
and prerequisites. The future agent/login section and `edit` command below
remain a roadmap, not available options.

The initial recipe includes a private `preview` mapping (guest 3000, host 8000).
`kuggis web NAME` opens it over SSH; `--no-open` and `--plan` support agents.
Custom preview ports remain available through the deployment configuration.

The BigBrain trial exposed an incomplete product flow. Creating one workspace
required provider commands, SSH trust setup, a handwritten deployment file,
manual resource insertion, agent downloads and transfers, host-side files, and
multiple installations. A wizard should complete those operations, rather than
print another checklist for the operator.

## One operation, two input methods

Human entry:

```text
$ kuggis create bigbrain
Host: exe.dev
Repository: elsehow/BigBrain
Repository access: read and write
Agent: Pi
Model: existing exe-managed integration

Creating host...
Configuring repository access...
Installing workspace and Pi...
Cloning repository...
Checking Git, model tools and isolation...
Ready. Run: kuggis connect bigbrain
```

These are configuration questions. Once the required answers are supplied,
creation proceeds without an approval question. Defaults are shown, explicit
arguments skip their questions, and the selected environment gives the agent
its authority. There is no approval mode inside the workspace.

Equivalent unattended entry:

```sh
kuggis create bigbrain --host exe --repo elsehow/BigBrain \
  --github-access write --agent pi --model exe-managed --json
```

Also accept `kuggis create --file setup.json --json`. Flags, a declarative input
file and interactive answers produce the same validated request. `--json` and
non-TTY invocations never prompt; missing inputs yield structured errors with
field names and a nonzero exit. Progress goes to stderr, one final report to
stdout. An agent does not need to drive a terminal wizard.

The first version supports fresh exe hosts, Pi, the existing managed-model
integration, and one GitHub repository. Keep the existing advanced SSH setup
command available. Other hosts and agent recipes can follow independently;
do not present options that the implementation cannot actually install.

## Later: choose an agent, then configure it inside

Extend agent selection through tested installation recipes for Pi, Codex and
Claude Code. Choosing a recipe installs the program, configures unattended tool
execution within the workspace boundary, and sets its native terminal launch.
The workspace, repository and files stay the same if the user later switches
agents. Conversations remain in each agent's native format; do not promise
cross-agent session conversion or interrupt a running session implicitly.

Model setup should offer an existing managed integration or the explicit choice
to configure models and logins after connecting. The latter opens the selected
agent's native setup/model/login interface where supported. Users can configure
multiple supported accounts and models and change their selection there. Avoid
implementing a parallel model catalog or OAuth client in Kuggis. Keep an agent's
own selected model authoritative unless the operator explicitly chooses a fixed
launch model; do not silently force it back through a generated `--model` flag.

Provider connectivity still belongs to deployment configuration. Tested provider
profiles declare the exact inference, login and refresh endpoints needed by the
selected agent. Native setup can use configured providers without seeking
runtime approvals. A new provider outside those grants is an explicit operator
configuration change through the same setup interface, not a guest request that
widens its own network boundary. Provider login may require a human browser or
device flow; unattended setup must report that prerequisite rather than hang.

Separate deployment-managed credential files from agent-managed login stores.
Native logins persist in the guest's private agent state, and reapplying setup
must not overwrite refreshed tokens or replace that state with an older host
snapshot. Keep secrets out of setup JSON, CLI arguments and result reports.
Agent recipes must define and test how managed and native authentication coexist
before advertising both options; not every agent supports the same login types.

Report workspace readiness and model readiness separately. An explicit deferred
login can yield `workspace: ready, model: needs_login` with a connect command.
That is a completed workspace setup with deferred authentication, not a claim
that inference passed. The unattended interface can supply an existing model
resource or explicitly defer login using the same typed choice. Core boundary
checks remain mandatory; model/tool checks run once credentials are configured.

## Complete the setup

1. Validate the request; find the matching GitHub integration and, when selected,
   model grant;
   check operator/provider authentication and local installation artifacts.
   A reusable GitHub integration must match the requested repository and access
   mode. Never silently reuse a broader grant or modify an integration shared
   by other workspaces. If initial account linking is missing, report the exact
   prerequisite before allocating a host. Authentication is distinct from
   permission to execute agent actions.
2. Create the dedicated VM using the provider's default image. Validate the
   actual OS and architecture. Do not copy a display image name into a registry
   image override. Choose a documented small capacity profile and leave host
   memory headroom; permit explicit overrides.
3. Establish the new SSH hostname from trusted provider identity or authenticated
   host-key metadata. Pin it in a workspace-owned SSH configuration. A key scan
   alone is not verification. Never disable strict host-key checking; unknown
   identity is an actionable failure, not a hidden trust prompt.
4. Attach the exact repository integration to this VM and verify the attachment.
   Resolve existing model access and account-wide grants. Record the effective
   grants: a shared gateway is not isolated by the hostname in a CONNECT rule.
   Keep account-management credentials on the operator side.
5. Generate a complete validated Workspace declaration, with no hand-spliced
   JSON. Provision root-owned recipe configuration on the host, then use the
   existing setup implementation once for the complete boundary.
6. Transfer and install the pinned Pi recipe inside the guest as the agent user.
   Clone into an empty project root; keep installation staging outside that
   root. Never execute uploaded guest helpers as host root or overwrite an
   existing checkout during a retry.
7. Verify the boundary, repository access, installed Pi and model readiness
   (or report the explicitly deferred authentication state described above).
   Make a bounded real model/tool smoke call in disposable guest storage before
   reporting model execution as verified. Record repository write scope from
   provider configuration separately from a read-only Git probe; do not claim
   that a push was tested. Do not start a coding task as part of setup.
8. Publish the ready report with the workspace name and connect command. Only
   add a private preview URL when preview was requested and its ingress checks
   passed. Do not claim readiness for a partially installed workspace.

Installing the local CLI must also provide its matching Linux helper and pinned
agent recipe metadata, or resolve checksum-verified matching artifacts. Normal
users should not compile Rust or pass `--bin`. During development, resolve the
existing repository build artifacts and report a missing build before creating
a VM. Release packaging is required before calling this a general installation
experience.

## Keep the core small

Put onboarding in a local module above the existing boundary implementation.
The runtime Workspace schema, transport and Linux helpers remain provider- and
agent-neutral. Pi and exe are deployment recipes, not runtime agent variants.

Use validated types for the important choices: workspace/repository identifiers,
repository access, host choice, recipe selection and capacity. A pure resolver
maps a request, an observed provider inventory and pinned recipe data to a
resolved setup containing the ordinary Workspace and concrete provisioning
parameters. Interactive input and flags share that resolver.

The effect layer is a short explicit sequence calling existing setup, transfer
and guest-entry primitives. Add one argv-based noninteractive guest execution
path if needed by the installer, using the existing namespace boundary. No
generic workflow engine, daemon, provider plugin framework, or approval protocol.

Store plaintext under a workspace-owned local directory:

- `setup.json`: validated user intent, with defaults made explicit.
- `workspace.json`: generated low-level declaration consumed by existing setup.
- `receipt.json`: provider identity, owned resources, artifact checksums and
  observed results needed to recover a partial attempt.
- Standalone SSH configuration and pinned known hosts; no copied operator keys.

The input is authoritative for generation; the generated declaration is not a
second independently edited source. Save complete validated documents atomically.
Later provide `kuggis edit bigbrain` through the same fields and validation, so ordinary
changes do not require manual JSON surgery. Existing path-based commands remain
available. Name-based connect/shell/put/get simply resolve the local declaration;
there is no hosted registry or cell hierarchy.

## Failure and retry

Record a created host's identity immediately. Retrying the same request checks
observed state and continues with that host, rather than allocating another.
If an existing name is not owned by this attempt, fail with a conflict; never
adopt or overwrite it implicitly. Serialize attempts for the same workspace.
After an ambiguous provider timeout, reconcile the exact creation result before
issuing another create. Do not treat a checkpoint as proof that remote state is
still correct.

An interrupted run leaves a usable diagnostic and preserves existing files.
Report the failed operation and which resources were created. Do not auto-delete
a workspace to hide an error. Explicit removal deletes the chosen workspace's
host and attachments; it preserves reusable account integrations and unrelated
services. Never infer retirement of a deployment from its repository name.

## Implementation order and acceptance

1. Implement the typed request/resolver, atomic files and name lookup. Test
   malformed input, exact repository scope, capacity constraints, defaults and
   equivalence of interactive and unattended inputs.
2. Add the exe/Pi creation sequence using existing recipes and boundary tools.
   Test failures at external-operation boundaries, interruption/retry, ambiguous
   creation, existing-name conflicts and prevention of duplicate hosts. Validate
   provider response bodies as well as exit status.
3. Add the thin terminal questions and structured CLI output. Test unattended
   missing-input behavior and ensure there are no hidden prompts in subprocesses.
4. Exercise both entry methods on disposable hosts: one human setup from a
   repository name, one fully unattended setup, then interrupted setup recovery.
   Verify Git reads, declared write scope, model/tool execution, persistent
   reconnect, preserved files and a clean boundary. Package the local CLI and
   matching Linux helper before a fresh-machine acceptance test.

Success means the user supplies their choices once and lands at a ready
workspace without editing JSON, handling tarballs, copying host configuration,
running provider grant commands, or diagnosing host-key plumbing manually.
