# stagehand

Run repeatable conference demos in disposable [smolvm](https://smolmachines.com) micro-VMs.

One command starts a **fresh Linux VM with your tools already installed** (an AI
harness, language toolchains — whatever `demo.toml` declares), your API keys and
SSH agent forwarded in, and the demo's webapp port reachable at a URL that never
changes. When the demo is over, one command stops the VM — the session and
workspace stay preserved on the VM's disk, ready to resume, and nothing keeps
running on your Mac.

```
stagehand build            # once: template VM → golden image with your tools
stagehand new              # fresh VM, fixed port, lands in the image entrypoint
stagehand new -- bash      # override the entrypoint for this run
stagehand new --no-attach  # start it without attaching
stagehand open             # re-attach later (boots it if stopped; session intact)
stagehand stop             # done: VM stopped, session preserved
stagehand start            # later: same machine back at the same URL
stagehand prune            # after the conference: delete every demo VM
```

Demo names are optional: unnamed commands resolve to the `[defaults] demo`
pin, else the demo you last touched, else the only configured demo. Pass a
name (`stagehand new search`) when you mean a different one. `stagehand new`
archives any still-running run of the same demo first (session preserved),
so the fixed port is always free.

## Why this shape

- **A fresh VM per demo run.** `stagehand new checkout` clones a golden image
  (`create --from`, ~sub-second boot), so every showing of the demo starts
  from a known-good state. Old machines are kept, not deleted — run the same
  demo five times and you get `stagehand-checkout-1..5`, each preserved.
- **Fixed ports, declared once.** Each demo gets a permanent host port in
  `demo.toml` (`checkout → 8410`). Forwarding is attached to the VM at
  creation, so `http://localhost:8410` works the moment the guest listens —
  and it is the same place every single time.
- **Sessions survive the machine stopping.** Tools store their session
  history and workspace on the VM's persistent overlay. `stagehand stop` halts
  the VM (no CPU/RAM use); `stagehand open` boots it again with the session
  intact.
- **Secrets stay on the host.** Provider keys are bound **host-side** with
  smolvm's credential feature: the VM only ever sees a meaningless
  `SMOL_PLACEHOLDER_*` value in the env var, and smolvm substitutes the real
  value from your shell environment on HTTPS requests to the provider's
  hosts. The secret itself never enters the VM — not on disk, not in the
  image, not even in process memory of the guest.

## Install

```sh
cargo install --path .          # installs `stagehand` onto your PATH
```

Requires `smolvm` on PATH (or `SMOLVM_BIN=/path/to/smolvm`).

## Setup

In any directory (it becomes your demo workspace):

```sh
stagehand init                    # writes a starter demo.toml
$EDITOR demo.toml               # one [demos.<name>] section per demo, with a fixed port
stagehand build                   # ~5 min once: installs packages + tools in a
                                #   template VM, then packs and extracts it to
                                #   .stagehand/rootfs (what demo machines clone)
```

`demo.toml` reference:

```toml
[build]                        # the golden image recipe — assembled by `stagehand build`
image = "debian:bookworm-slim" # base image
packages = ["git", "curl"]     # distro packages installed into the image
tools = ["node@lts", "some-cli@latest"]  # mise tools installed into the image
entrypoint = "some-cli"        # what `stagehand new` lands in (default: shell)
shell = "/bin/bash"            # shell used when no entrypoint is set
workspace = "/workspace"       # working directory for new/open/exec
setup = """                    # extra provisioning script, runs in the template
your-setup-command --here
"""

[builder]                      # optional — cross-compile stage for Linux artifacts
image = "debian:bookworm-slim" # builder base image
source = "/path/to/your/repo"  # host dir mounted at /src inside the builder
tools = ["rust@stable"]        # mise tools installed in the builder first
artifacts = ["my-tool"]        # files under /build/artifacts/ to install into the image
script = """                   # runs in the builder VM (persistent, incremental)
cargo build --release --manifest-path /src/Cargo.toml -p my-cli
cp /build/target/release/my-tool /build/artifacts/my-tool
"""

[defaults]
demo = "checkout"              # what an unnamed `stagehand new`/`open`/`stop` means
guest_port = 8080              # where your webapp listens inside the VM
net = true                     # outbound networking from the VM
ssh_agent = true               # forward the host SSH agent (git push/pull from the VM)

# Host-side credential bindings (recommended): the VM sees a placeholder,
# the real value is substituted on HTTPS requests to the hosts you list —
# and nowhere else. List every host the credential is actually sent to
# (the host must match what the client really connects to), and have the
# env var set in your shell whenever a machine is STARTED.
[defaults.credentials.MY_API_KEY]
hosts = ["api.example.com"]

# Plain secrets: the value IS visible inside the VM (for non-HTTPS tools).
# secret_env = ["SOME_PLAIN_TOKEN"]

[demos.checkout]
port = 8410                    # FIXED host port — the demo's permanent URL
# guest_port = 8080            # override [defaults].guest_port for this demo
# credentials = { MY_KEY = { hosts = ["api.example.com"] } }  # replaces defaults for this demo
```

Two-stage model, like a multi-stage Dockerfile: `[builder]` (optional)
produces Linux artifacts, `[build]` assembles the golden image from a base
image + packages + mise tools + `setup`, and copies `[builder]` artifacts
into it at `/usr/local/bin/`. Demo VMs are clones of that image.

Tooling is managed by [mise](https://mise.jdx.dev): `tools` specs are installed
with `mise use -g` in the respective VM, and every template tool is symlinked
to `/usr/local/bin/` so non-mise shells (and `stagehand exec`) see it.
**All of this happens inside the VMs — the host never needs mise; it only
needs [smolvm](https://smolmachines.com).**

Artifacts built by `[build]` land at `/usr/local/bin/<name>` in the golden
image — `stagehand build` runs `<artifact> --version` inside the template to
verify each one. The builder machine persists (`stagehand-builder-<dir>`), so
toolchains and `target/` survive across builds; only the first build is slow
(a cold Rust build took ~5 min; rebuilds are seconds).
`stagehand build --force` recreates both template and builder.

## Commands

| Command | What it does |
|---|---|
| `stagehand init` | Write a starter `demo.toml` |
| `stagehand build [--force]` | (Re)build the golden image from the `[build]` recipe + `[builder]` stage |
| `stagehand new <demo> [--no-attach] [-- cmd]` | Fresh VM from the golden image, started, lands in the entrypoint |
| `stagehand open <demo> [-- cmd]` | Attach to the newest machine — image entrypoint, or a command override (boots it first if stopped) |
| `stagehand exec <demo> [--detach] [-- cmd]` | Run a one-off command (`--detach` for background servers) |
| `stagehand url <demo>` | Print the demo's permanent URL |
| `stagehand start <demo>` | Boot the newest machine again (resume) |
| `stagehand stop <demo>` | Stop the newest machine, session preserved |
| `stagehand list` | Demos, machines, states, URLs |
| `stagehand reset <demo>` | Throw away the newest machine, start a fresh one (same port) |
| `stagehand rm <demo> [--all]` | Delete machines of a demo |
| `stagehand prune [--template]` | Delete **all** demo machines (conference is over) |

Every per-demo command targets the *most recent* machine of that demo. `list`
shows what that is.

## Conference-day runbook

The night before:

```sh
stagehand build                   # refresh the golden image if [build] changed
stagehand new checkout && stagehand stop checkout   # dress rehearsal, machine stays archived
```

On stage, per showing:

```sh
stagehand new                     # fresh VM + straight into the entrypoint (no name needed)
# ... run the demo; the webapp is at http://localhost:8410 ...
#   start a webserver from another terminal, e.g.:
#   stagehand exec --detach -- python3 -m http.server 8080 --directory /workspace
stagehand stop                    # VM off, nothing running, session archived
```

Questions from the audience about yesterday's run?

```sh
stagehand open checkout           # boots the archived machine with the session intact
```

Flying home:

```sh
stagehand list                    # see everything
stagehand prune                   # delete all demo VMs (golden image is kept)
```

## Notes

- Machine names (`stagehand-checkout-3`) and the demo they belong to are tracked
  via smolvm labels, so `list`/`rm`/`prune` only ever touch stagehand's own
  machines.
- The golden image lives under `.stagehand/` next to your `demo.toml`
  (gitignored): `rootfs/` is what demo machines clone, `golden.smolmachine`
  is the portable pack it was extracted from. Deleting both is safe —
  `stagehand build` recreates them.
- The template machine `stagehand-template-<dir>` is reused across builds; pass
  `--force` to rebuild it from scratch.
- Every `tools` binary is on PATH in the image; sessions start in
  `/workspace` (created empty in the golden image). Git works out of the box
  through the forwarded SSH agent.
- Credential bindings resolve the real value from the environment of whatever
  shell **starts** the machine (`stagehand new` / `start`). If it's missing,
  requests to the bound hosts fail closed with
  `smolvm credentials: credential unavailable` — set the var and restart the
  machine. stagehand warns at start when it can tell a var is missing.
- Demo machines are created with `machine create --image <rootfs>` rather
  than `--from <pack>` because smolvm 1.18.0 silently drops
  `--credential`/`--secret-env` flags on the pack path.
