# democtl

Run repeatable conference demos in disposable [smolvm](https://smolmachines.com) micro-VMs.

One command starts a **fresh Linux VM with opencode already installed**, your API keys
and SSH agent forwarded in, and the demo's webapp port reachable at a URL that never
changes. When the demo is over, one command stops the VM — the opencode session and
workspace stay preserved on the VM's disk, ready to resume, and nothing keeps running
on your Mac.

```
democtl build            # once: template VM → golden image (opencode preinstalled)
democtl new              # fresh VM, fixed port, lands you inside opencode
democtl new --no-attach  # start it without attaching
democtl open             # re-attach later (boots it if stopped; session intact)
democtl stop             # done: VM stopped, session preserved
democtl start            # later: same machine back at the same URL
democtl prune            # after the conference: delete every demo VM
```

Demo names are optional: unnamed commands resolve to the `[defaults] demo`
pin, else the demo you last touched, else the only configured demo. Pass a
name (`democtl new search`) when you mean a different one. `democtl new`
archives any still-running run of the same demo first (session preserved),
so the fixed port is always free.

## Why this shape

- **A fresh VM per demo run.** `democtl new checkout` clones a golden image
  (`create --from`, ~sub-second boot), so every showing of the demo starts
  from a known-good state. Old machines are kept, not deleted — run the same
  demo five times and you get `democtl-checkout-1..5`, each preserved.
- **Fixed ports, declared once.** Each demo gets a permanent host port in
  `demo.toml` (`checkout → 8410`). Forwarding is attached to the VM at
  creation, so `http://localhost:8410` works the moment the guest listens —
  and it is the same place every single time.
- **Sessions survive the machine stopping.** opencode stores its session
  history and workspace on the VM's persistent overlay. `democtl stop` halts
  the VM (no CPU/RAM use); `democtl open` boots it again with the session
  intact.
- **Secrets stay on the host.** Provider keys are bound **host-side** with
  smolvm's credential feature: the VM only ever sees a meaningless
  `SMOL_PLACEHOLDER_*` value in the env var, and smolvm substitutes the real
  value from your shell environment on HTTPS requests to the provider's
  hosts. The secret itself never enters the VM — not on disk, not in the
  image, not even in process memory of the guest.

## Install

```sh
cargo install --path .          # installs `democtl` onto your PATH
```

Requires `smolvm` on PATH (or `SMOLVM_BIN=/path/to/smolvm`).

## Setup

In any directory (it becomes your demo workspace):

```sh
democtl init                    # writes a starter demo.toml
$EDITOR demo.toml               # one [demos.<name>] section per demo, with a fixed port
democtl build                   # ~5 min once: installs packages + opencode in a
                                #   template VM, then packs and extracts it to
                                #   .democtl/rootfs (what demo machines clone)
```

`demo.toml` reference:

```toml
[defaults]
demo = "checkout"               # what an unnamed `democtl new`/`open`/`stop` means
guest_port = 8080               # where your webapp listens inside the VM

# Host-side credential bindings (recommended): the VM sees a placeholder,
# the real value is substituted on HTTPS requests to the hosts you list —
# and nowhere else. List every host the credential is actually sent to
# (e.g. OpenRouter's API host is "openrouter.ai"), and have the env var
# set in your shell whenever a machine is STARTED.
[defaults.credentials.OPENROUTER_API_KEY]
hosts = ["openrouter.ai"]

# Plain secrets: the value IS visible inside the VM (for non-HTTPS tools).
# secret_env = ["SOME_PLAIN_TOKEN"]
net = true                      # outbound networking from the VM
ssh_agent = true                # forward the host SSH agent (git push/pull from the VM)

[template]                      # optional — golden image recipe
image = "debian:bookworm-slim"  # base image for the template
packages = ["git", "curl"]      # extra packages installed into the golden image
setup = """                     # extra setup shell script, runs in the template
mkdir -p /tmp/smoke && cd /tmp/smoke && atomic init
atomic agent enable --agent opencode
"""
shell = "/bin/bash"             # what `democtl open` drops you into
workspace = "/workspace"        # working directory for open/exec
tools = ["node@lts", "opencode@latest"]  # mise tools installed into the image

[build]                         # optional — Linux build machine (cross-compile)
image = "debian:bookworm-slim"  # builder base image
source = "/path/to/your/repo"   # host dir mounted at /src inside the builder
tools = ["rust@stable"]         # mise tools installed in the builder first
artifacts = ["atomic"]          # files under /build/artifacts/ to install
script = """                    # runs in the builder VM (persistent, incremental)
cargo build --release --manifest-path /src/Cargo.toml -p atomic-cli
cp /build/target/release/atomic /build/artifacts/atomic
"""

[demos.checkout]
port = 8410                     # FIXED host port — the demo's permanent URL
# guest_port = 8080             # override [defaults].guest_port for this demo
# credentials = { MY_KEY = { hosts = ["api.example.com"] } }  # replaces defaults for this demo
```

Tooling is managed by [mise](https://mise.jdx.dev): `tools` specs are installed
with `mise use -g` in the respective VM, and every template tool is symlinked
to `/usr/local/bin/` so non-mise shells (and `democtl exec`) see it.

Artifacts built by `[build]` land at `/usr/local/bin/<name>` in the golden
image — `democtl build` runs `<artifact> --version` inside the template to
verify each one. The builder machine persists (`democtl-builder-<dir>`), so
toolchains and `target/` survive across builds; only the first build is slow
(the atomic cold build took ~5 min; rebuilds are seconds).
`democtl build --force` recreates both template and builder.

## Commands

| Command | What it does |
|---|---|
| `democtl init` | Write a starter `demo.toml` |
| `democtl build [--force]` | (Re)build the golden image from the `[template]` recipe |
| `democtl new <demo> [--attach]` | Fresh VM from the golden image, started, port live |
| `democtl open <demo> [--opencode] [-- cmd]` | Attach to the newest machine (shell by default; boots it first if stopped) |
| `democtl exec <demo> [--detach] [-- cmd]` | Run a one-off command (`--detach` for background servers) |
| `democtl url <demo>` | Print the demo's permanent URL |
| `democtl start <demo>` | Boot the newest machine again (resume) |
| `democtl stop <demo>` | Stop the newest machine, session preserved |
| `democtl list` | Demos, machines, states, URLs |
| `democtl reset <demo>` | Throw away the newest machine, start a fresh one (same port) |
| `democtl rm <demo> [--all]` | Delete machines of a demo |
| `democtl prune [--template]` | Delete **all** demo machines (conference is over) |

Every per-demo command targets the *most recent* machine of that demo. `list`
shows what that is.

## Conference-day runbook

The night before:

```sh
democtl build                   # refresh the golden image if [template] changed
democtl new checkout && democtl stop checkout   # dress rehearsal, machine stays archived
```

On stage, per showing:

```sh
democtl new                     # fresh VM + straight into opencode (no name needed)
# ... run the demo; the webapp is at http://localhost:8410 ...
#   start a webserver from another terminal, e.g.:
#   democtl exec --detach -- python3 -m http.server 8080 --directory /workspace
democtl stop                    # VM off, nothing running, session archived
```

Questions from the audience about yesterday's run?

```sh
democtl open checkout           # boots the archived machine with the session intact
```

Flying home:

```sh
democtl list                    # see everything
democtl prune                   # delete all demo VMs (golden image is kept)
```

## Notes

- Machine names (`democtl-checkout-3`) and the demo they belong to are tracked
  via smolvm labels, so `list`/`rm`/`prune` only ever touch democtl's own
  machines.
- The golden image lives under `.democtl/` next to your `demo.toml`
  (gitignored): `rootfs/` is what demo machines clone, `golden.smolmachine`
  is the portable pack it was extracted from. Deleting both is safe —
  `democtl build` recreates them.
- The template machine `democtl-template-<dir>` is reused across builds; pass
  `--force` to rebuild it from scratch.
- opencode is installed under `/usr/local/bin` in the image and starts in
  `/workspace` (created empty in the golden image). Git works out of the box
  through the forwarded SSH agent.
- Credential bindings resolve the real value from the environment of whatever
  shell **starts** the machine (`democtl new` / `start`). If it's missing,
  requests to the bound hosts fail closed with
  `smolvm credentials: credential unavailable` — set the var and restart the
  machine. democtl warns at start when it can tell a var is missing.
- Demo machines are created with `machine create --image <rootfs>` rather
  than `--from <pack>` because smolvm 1.18.0 silently drops
  `--credential`/`--secret-env` flags on the pack path.
