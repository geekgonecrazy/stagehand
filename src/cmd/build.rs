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
  for tool in __TOOLNAMES__; do
    ln -sf "$(mise which "$tool")" "/usr/local/bin/$tool"
  done
fi

mkdir -p "__WORKSPACE__"

__SETUP__

echo "==> verifying"
for tool in __TOOLNAMES__; do
  "/usr/local/bin/$tool" --version
done
echo "==> golden image setup complete"
"#;

fn build_script(tpl: &crate::config::TemplateCfg) -> String {
    let setup = match &tpl.setup {
        Some(s) => format!("\necho \"==> custom setup\"\n{s}\n"),
        None => String::new(),
    };
    let tool_names: Vec<&str> = tpl.tools.iter().map(|s| tool_name(s)).collect();
    SCRIPT_TEMPLATE
        .replace("__PKGS__", &tpl.packages.join(" "))
        .replace("__TOOLS__", &tpl.tools.join(" "))
        .replace("__TOOLNAMES__", &tool_names.join(" "))
        .replace("__WORKSPACE__", &tpl.workspace)
        .replace("__SETUP__", &setup)
}

/// The tool name part of a mise spec: `opencode@1.19.0` → `opencode`.
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
    let tpl = &loaded.config.template;

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
        let mut args = vec![
            "machine".to_string(),
            "create".to_string(),
            "--name".to_string(),
            name.clone(),
            "--image".to_string(),
            tpl.image.clone(),
            "--net".to_string(),
            "--label".to_string(),
            format!("{}=1", crate::config::LABEL_MANAGED),
            "--label".to_string(),
            format!("{}={ROLE_TEMPLATE}", crate::config::LABEL_ROLE),
        ];
        if let Some(cpus) = tpl.cpus {
            args.push("--cpus".into());
            args.push(cpus.to_string());
        }
        if let Some(mem) = tpl.mem_mib {
            args.push("--mem".into());
            args.push(mem.to_string());
        }
        smol.run_inherit(&refs(&args))?;
    } else {
        println!("==> reusing existing template machine {name}");
    }

    // Cross-compile Linux artifacts first (if configured) — the template
    // setup script may rely on them being installed.
    if let Some(build) = &loaded.config.build {
        build_artifacts(loaded, smol, build)?;
    }

    let m = smol.status(name)?.context("template machine vanished")?;
    if !m.running() {
        println!("==> starting template");
        smol.run_inherit(&["machine", "start", "--name", name])?;
    }

    install_artifacts(loaded, smol)?;

    println!(
        "{}",
        util::bold("==> installing packages + opencode (first build takes a few minutes)")
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
        &build_script(tpl),
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
fn build_artifacts(loaded: &Loaded, smol: &Smol, build: &crate::config::BuildCfg) -> Result<()> {
    let name = &loaded.builder_name;
    let exists = smol.status(name)?.is_some();
    if !exists {
        println!(
            "{}",
            util::bold(&format!("==> creating builder machine {name}"))
        );
        let source = build
            .source
            .canonicalize()
            .unwrap_or_else(|_| build.source.clone());
        let args = vec![
            "machine".to_string(),
            "create".to_string(),
            "--name".to_string(),
            name.clone(),
            "--image".to_string(),
            build.image.clone(),
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
        mise_prelude(&build.tools),
        build.script
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
    for artifact in &build.artifacts {
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
    let build = match &loaded.config.build {
        Some(b) => b,
        None => return Ok(()),
    };
    let name = &loaded.template_name;
    for artifact in &build.artifacts {
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
        let tpl = crate::config::TemplateCfg {
            image: "debian:bookworm-slim".into(),
            cpus: None,
            mem_mib: None,
            packages: vec!["curl".into(), "git".into()],
            tools: vec!["opencode@1.19.0".into()],
            setup: Some("echo hi".into()),
            shell: "/bin/bash".into(),
            workspace: "/workspace".into(),
        };
        let s = build_script(&tpl);
        assert!(s.contains("curl git"));
        assert!(s.contains("echo hi"));
        assert!(s.contains("mkdir -p \"/workspace\""));
        assert!(s.contains("if [ -n \"opencode@1.19.0\" ]; then"));
        assert!(s.contains("for tool in opencode@1.19.0; do mise use -g \"$tool\"; done"));
        assert!(s.contains("$(mise which \"$tool\")"));
        assert!(s.contains("for tool in opencode; do\n  \"/usr/local/bin/$tool\" --version"));
        assert!(s.contains("\"/usr/local/bin/$tool\" --version"));
    }

    #[test]
    fn script_without_tools_skips_mise() {
        let tpl = crate::config::TemplateCfg {
            image: "debian:bookworm-slim".into(),
            cpus: None,
            mem_mib: None,
            packages: vec![],
            tools: vec![],
            setup: None,
            shell: "/bin/bash".into(),
            workspace: "/workspace".into(),
        };
        let s = build_script(&tpl);
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
        assert_eq!(tool_name("opencode@1.19.0"), "opencode");
        assert_eq!(tool_name("rust"), "rust");
    }
}
