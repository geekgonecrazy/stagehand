use crate::config::{Loaded, ROLE_TEMPLATE};
use crate::smol::Smol;
use crate::util;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Stdio;

/// Shell script installed into the template machine by `democtl build`.
/// `__PKGS__`, `__TOOLS__`, `__TOOLNAMES__`, `__WORKSPACE__` and `__SETUP__`
/// are substituted by [`build_script`].
const SCRIPT_TEMPLATE: &str = r#"set -eu

echo "==> installing packages"
PKGS="__PKGS__"
if command -v apt-get >/dev/null 2>&1; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends $PKGS
elif command -v apk >/dev/null 2>&1; then
  apk add --no-cache $PKGS
elif command -v dnf >/dev/null 2>&1; then
  dnf install -y -q $PKGS
else
  echo "no known package manager found" >&2
  exit 1
fi

if [ -n "__TOOLS__" ]; then
  echo "==> installing mise + tools: __TOOLS__"
  if ! command -v mise >/dev/null 2>&1; then
    curl -fsSL https://mise.run | MISE_INSTALL_PATH=/usr/local/bin/mise sh
  fi
  export PATH="$HOME/.local/share/mise/shims:$PATH"
  mise --version
  for tool in __TOOLS__; do mise use -g "$tool"; done
  mise reshim
  # Mirror every shim into /usr/local/bin so non-mise shells (and `sh -c`
  # from democtl exec) see all tool binaries — cargo, rustc, go, gofmt, ...
  if [ -d "$HOME/.local/share/mise/shims" ]; then
    for shim in "$HOME/.local/share/mise/shims"/*; do
      ln -sf "$shim" "/usr/local/bin/$(basename "$shim")"
    done
  fi
fi

mkdir -p "__WORKSPACE__"

__SETUP__

echo "==> verifying"
# Best-effort: skip multi-binary tools (e.g. rust) and nonstandard flags
# (e.g. go wants `version`, not `--version`); setup scripts do strict checks.
for tool in __TOOLNAMES__; do
  if [ -e "/usr/local/bin/$tool" ]; then
    "/usr/local/bin/$tool" --version 2>/dev/null \
      || "/usr/local/bin/$tool" version 2>/dev/null \
      || true
  fi
done
echo "==> golden image setup complete"
"#;

fn build_script(build: &crate::config::BuildCfg) -> String {
    let setup = match &build.setup {
        Some(s) => format!("\necho \"==> custom setup\"\n{s}\n"),
        None => String::new(),
    };
    let tool_names: Vec<&str> = build.tools.iter().map(|s| tool_name(s)).collect();
    SCRIPT_TEMPLATE
        .replace("__PKGS__", &build.packages.join(" "))
        .replace("__TOOLS__", &build.tools.join(" "))
        .replace("__TOOLNAMES__", &tool_names.join(" "))
        .replace("__WORKSPACE__", &build.workspace)
        .replace("__SETUP__", &setup)
}

/// The tool name part of a mise spec: `some-cli@1.19.0` → `some-cli`.
fn tool_name(spec: &str) -> &str {
    spec.split('@').next().unwrap_or(spec)
}

/// mise bootstrap prelude prepended to the builder script when `[build].tools`
/// is configured: installs mise, activates the tools, exports shims on PATH.
fn mise_prelude(tools: &[String]) -> String {
    if tools.is_empty() {
        return String::new();
    }
    let joined = tools.join(" ");
    format!(
        r#"echo "==> installing mise + tools: {joined}"
if ! command -v mise >/dev/null 2>&1; then
  curl -fsSL https://mise.run | MISE_INSTALL_PATH=/usr/local/bin/mise sh
fi
export PATH="$HOME/.local/share/mise/shims:$PATH"
mise --version
for tool in {joined}; do mise use -g "$tool"; done
"#
    )
}

pub fn run(loaded: &Loaded, smol: &Smol, force: bool) -> Result<()> {
    let name = &loaded.template_name;
    let build = &loaded.config.build;

    std::fs::create_dir_all(&loaded.dot_dir)
        .with_context(|| format!("creating {}", loaded.dot_dir.display()))?;

    if force {
        for machine in [name, &loaded.builder_name] {
            if let Some(m) = smol.status(machine)? {
                if m.running() {
                    smol.run_inherit(&["machine", "stop", "--name", machine])?;
                }
                smol.run_inherit(&["machine", "delete", "--name", machine, "-f"])?;
            }
        }
    }

    let exists = smol.status(name)?.is_some();
    if !exists {
        println!("{}", util::bold(&format!("==> creating template machine {name}")));
        let args = vec![
            "machine".to_string(),
            "create".to_string(),
            "--name".to_string(),
            name.clone(),
            "--image".to_string(),
            build.image.clone(),
            "--net".to_string(),
            "--label".to_string(),
            format!("{}=1", crate::config::LABEL_MANAGED),
            "--label".to_string(),
            format!("{}={ROLE_TEMPLATE}", crate::config::LABEL_ROLE),
        ];
        smol.run_inherit(&refs(&args))?;
    } else {
        println!("==> reusing existing template machine {name}");
    }

    // Cross-compile Linux artifacts first (if configured) — the template
    // setup script may rely on them being installed.
    if let Some(builder) = &loaded.config.builder {
        build_artifacts(loaded, smol, builder)?;
    }

    let m = smol.status(name)?.context("template machine vanished")?;
    if !m.running() {
        println!("==> starting template");
        smol.run_inherit(&["machine", "start", "--name", name])?;
    }

    install_artifacts(loaded, smol)?;

    println!(
        "{}",
        util::bold("==> installing packages + tools (first build takes a few minutes)")
    );
    // Pipe the script through `exec -i -- sh -s`: exec only forwards host
    // stdin with `-i`, and `--stream` gives live progress for long installs.
    smol.run_with_stdin(
        &[
            "machine",
            "exec",
            "-i",
            "--stream",
            "--name",
            name,
            "--timeout",
            "20m",
            "--",
            "sh",
            "-s",
        ],
        &build_script(build),
    )?;

    println!("==> stopping template");
    smol.run_inherit(&["machine", "stop", "--name", name])?;

    println!(
        "{}",
        util::bold("==> packing golden image (this copies layers, be patient)")
    );
    // `pack create -o X` writes an executable stub at X and the pack sidecar at
    // X.smolmachine — `machine create --from` consumes the sidecar.
    let out_base = loaded.dot_dir.join("golden");
    smol.run_inherit(&[
        "pack",
        "create",
        "--from-vm",
        name,
        "-o",
        &out_base.to_string_lossy(),
    ])?;

    let size = std::fs::metadata(&loaded.golden_path)
        .map(|m| format!(" ({})", humansize(m.len())))
        .unwrap_or_default();
    util::ok(&format!("packed golden image{size}"));

    extract_rootfs(loaded)?;

    util::ok(&format!(
        "golden rootfs ready at {}",
        loaded.rootfs_path.display()
    ));
    println!(
        "\nnext: {} to start a demo VM",
        util::bold("democtl new <demo>")
    );
    Ok(())
}

/// Cross-compile on Linux inside the persistent builder machine and copy the
/// artifacts back to the host.
fn build_artifacts(loaded: &Loaded, smol: &Smol, builder: &crate::config::BuilderCfg) -> Result<()> {
    let name = &loaded.builder_name;
    let exists = smol.status(name)?.is_some();
    if !exists {
        println!(
            "{}",
            util::bold(&format!("==> creating builder machine {name}"))
        );
        let source = builder
            .source
            .canonicalize()
            .unwrap_or_else(|_| builder.source.clone());
        let args = vec![
            "machine".to_string(),
            "create".to_string(),
            "--name".to_string(),
            name.clone(),
            "--image".to_string(),
            builder.image.clone(),
            "--net".to_string(),
            "-v".to_string(),
            format!("{}:/src", source.to_string_lossy()),
            "--label".to_string(),
            format!("{}=1", crate::config::LABEL_MANAGED),
            "--label".to_string(),
            format!(
                "{}={}",
                crate::config::LABEL_ROLE,
                crate::config::ROLE_BUILDER
            ),
        ];
        smol.run_inherit(&refs(&args))?;
    } else {
        println!("==> reusing existing builder machine {name}");
    }

    let m = smol.status(name)?.context("builder machine vanished")?;
    if !m.running() {
        println!("==> starting builder");
        smol.run_inherit(&["machine", "start", "--name", name])?;
    }

    println!("{}", util::bold("==> building Linux artifacts in builder VM"));
    let script = format!(
        "set -eu\nmkdir -p /build/artifacts\ncd /build\n\n{}{}\n",
        mise_prelude(&builder.tools),
        builder.script
    );
    // 60m: a cold Rust release build inside the microVM is slow; incremental
    // rebuilds reuse the persistent target dir and finish in seconds.
    smol.run_with_stdin(
        &[
            "machine",
            "exec",
            "-i",
            "--stream",
            "--name",
            name,
            "--timeout",
            "60m",
            "-w",
            "/build",
            "--",
            "sh",
            "-s",
        ],
        &script,
    )?;

    std::fs::create_dir_all(&loaded.artifacts_dir)?;
    for artifact in &builder.artifacts {
        let dest = loaded.artifacts_dir.join(artifact);
        println!("==> exporting {artifact}");
        smol.run_inherit(&[
            "machine",
            "cp",
            &format!("{name}:/build/artifacts/{artifact}"),
            &dest.to_string_lossy(),
        ])?;
    }
    Ok(())
}

/// Copy exported artifacts into the template at /usr/local/bin and verify
/// each one runs.
fn install_artifacts(loaded: &Loaded, smol: &Smol) -> Result<()> {
    let builder = match &loaded.config.builder {
        Some(b) => b,
        None => return Ok(()),
    };
    let name = &loaded.template_name;
    for artifact in &builder.artifacts {
        let host = loaded.artifacts_dir.join(artifact);
        if !host.is_file() {
            bail!("build artifact missing on host: {}", host.display());
        }
        println!("==> installing {artifact} into template");
        smol.run_inherit(&[
            "machine",
            "cp",
            &host.to_string_lossy(),
            &format!("{name}:/usr/local/bin/{artifact}"),
        ])?;
        smol.run_inherit(&[
            "machine",
            "exec",
            "--name",
            name,
            "--",
            "chmod",
            "+x",
            &format!("/usr/local/bin/{artifact}"),
        ])?;
        smol.run_inherit(&[
            "machine",
            "exec",
            "--name",
            name,
            "--timeout",
            "5m",
            "--",
            &format!("/usr/local/bin/{artifact}"),
            "--version",
        ])
        .with_context(|| format!("verifying {artifact} --version in template"))?;
    }
    Ok(())
}

/// Unpack the pack's flattened rootfs layer into `.democtl/rootfs`.
///
/// Demo machines are created with `machine create --image <rootfs>` because
/// smolvm's `create --from <pack>` silently drops `--credential` and
/// `--secret-env` bindings (smolvm 1.18.0); the `--image` path honors them.
fn extract_rootfs(loaded: &Loaded) -> Result<()> {
    let pack_extract = loaded.dot_dir.join("pack-extract");
    let _ = std::fs::remove_dir_all(&pack_extract);
    let _ = std::fs::remove_dir_all(&loaded.rootfs_path);
    std::fs::create_dir_all(&pack_extract)?;
    std::fs::create_dir_all(&loaded.rootfs_path)?;

    let pack = loaded.golden_path.to_string_lossy().into_owned();
    let extract_dir = pack_extract.to_string_lossy().into_owned();
    // The pack tar also contains a sparse storage.ext4 member that bsdtar
    // reports as truncated even when the layers extract fine.
    tar(&["-xf", &pack, "-C", &extract_dir, "layers"], "extracting pack", true)?;

    let layers_dir = pack_extract.join("layers");
    let mut layers: Vec<PathBuf> = std::fs::read_dir(&layers_dir)
        .with_context(|| format!("reading {}", layers_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "tar").unwrap_or(false))
        .collect();
    if layers.len() != 1 {
        bail!(
            "expected exactly one rootfs layer in the pack, found {}",
            layers.len()
        );
    }
    let layer = layers.pop().unwrap().to_string_lossy().into_owned();
    let rootfs = loaded.rootfs_path.to_string_lossy().into_owned();
    tar(&["-xf", &layer, "-C", &rootfs], "extracting rootfs", false)?;

    let _ = std::fs::remove_dir_all(&pack_extract);
    Ok(())
}

fn tar(args: &[&str], what: &str, allow_truncated: bool) -> Result<()> {
    let out = std::process::Command::new("tar")
        .args(args)
        .stderr(Stdio::piped())
        .output()
        .context("running tar (is bsdtar with zstd support available?)")?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if allow_truncated && stderr.contains("Truncated input file") {
        return Ok(());
    }
    bail!("tar failed while {what}:\n{stderr}");
}

fn refs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

fn humansize(n: u64) -> String {
    let mb = n as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.1} GiB", mb / 1024.0)
    } else {
        format!("{mb:.0} MiB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_contains_packages_and_setup() {
        let build = crate::config::BuildCfg {
            image: "debian:bookworm-slim".into(),
            packages: vec!["curl".into(), "git".into()],
            tools: vec!["some-cli@1.0".into()],
            entrypoint: vec![],
            setup: Some("echo hi".into()),
            shell: "/bin/bash".into(),
            workspace: "/workspace".into(),
        };
        let s = build_script(&build);
        assert!(s.contains("curl git"));
        assert!(s.contains("echo hi"));
        assert!(s.contains("mkdir -p \"/workspace\""));
        assert!(s.contains("if [ -n \"some-cli@1.0\" ]; then"));
        assert!(s.contains("for tool in some-cli@1.0; do mise use -g \"$tool\"; done"));
        assert!(s.contains("for shim in \"$HOME/.local/share/mise/shims\"/*; do"));
        assert!(s.contains("for tool in some-cli; do"));
        assert!(s.contains("if [ -e \"/usr/local/bin/$tool\" ]"));
    }

    #[test]
    fn script_without_tools_skips_mise() {
        let build = crate::config::BuildCfg {
            image: "debian:bookworm-slim".into(),
            packages: vec![],
            tools: vec![],
            entrypoint: vec![],
            setup: None,
            shell: "/bin/bash".into(),
            workspace: "/workspace".into(),
        };
        let s = build_script(&build);
        assert!(s.contains("if [ -n \"\" ]; then"));
    }

    #[test]
    fn mise_prelude_activates_tools() {
        assert!(mise_prelude(&[]).is_empty());
        let p = mise_prelude(&["rust@stable".into()]);
        assert!(p.contains("for tool in rust@stable; do mise use -g \"$tool\"; done"));
        assert!(p.contains("mise/shims"));
    }

    #[test]
    fn tool_name_strips_version() {
        assert_eq!(tool_name("some-cli@1.0"), "some-cli");
        assert_eq!(tool_name("rust"), "rust");
    }
}
