use crate::config::{Loaded, ROLE_TEMPLATE};
use crate::smol::Smol;
use crate::util;
use anyhow::{Context, Result};

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
    util::ok(&format!(
        "golden image ready at {}{size}",
        loaded.golden_path.display()
    ));
    println!(
        "\nnext: {} to start a demo VM",
        util::bold("democtl new <demo>")
    );
    Ok(())
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
