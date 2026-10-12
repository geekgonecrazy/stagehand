use crate::config::{Loaded, ROLE_TEMPLATE};
use crate::smol::Smol;
use crate::util;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Stdio;

/// Shell script installed into the template machine by `democtl build`.
/// `__PKGS__`, `__WORKSPACE__` and `__SETUP__` are substituted by [`build_script`].
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

echo "==> installing opencode"
curl -fsSL https://opencode.ai/install | bash
ln -sf "$HOME/.opencode/bin/opencode" /usr/local/bin/opencode

mkdir -p "__WORKSPACE__"

__SETUP__

echo "==> verifying"
opencode --version
echo "==> golden image setup complete"
"#;

fn build_script(tpl: &crate::config::TemplateCfg) -> String {
    let setup = match &tpl.setup {
        Some(s) => format!("\necho \"==> custom setup\"\n{s}\n"),
        None => String::new(),
    };
    SCRIPT_TEMPLATE
        .replace("__PKGS__", &tpl.packages.join(" "))
        .replace("__WORKSPACE__", &tpl.workspace)
        .replace("__SETUP__", &setup)
}

pub fn run(loaded: &Loaded, smol: &Smol, force: bool) -> Result<()> {
    let name = &loaded.template_name;
    let tpl = &loaded.config.template;

    std::fs::create_dir_all(&loaded.dot_dir)
        .with_context(|| format!("creating {}", loaded.dot_dir.display()))?;

    if force {
        if let Some(m) = smol.status(name)? {
            if m.running() {
                smol.run_inherit(&["machine", "stop", "--name", name])?;
            }
            smol.run_inherit(&["machine", "delete", "--name", name, "-f"])?;
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

    let m = smol.status(name)?.context("template machine vanished")?;
    if !m.running() {
        println!("==> starting template");
        smol.run_inherit(&["machine", "start", "--name", name])?;
    }

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
            setup: Some("echo hi".into()),
            shell: "/bin/bash".into(),
            workspace: "/workspace".into(),
        };
        let s = build_script(&tpl);
        assert!(s.contains("curl git"));
        assert!(s.contains("echo hi"));
        assert!(s.contains("mkdir -p \"/workspace\""));
        assert!(s.contains("opencode --version"));
    }
}
