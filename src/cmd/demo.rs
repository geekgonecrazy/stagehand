use crate::cmd::target;
use crate::config::{Loaded, LABEL_DEMO, LABEL_IDX, LABEL_MANAGED};
use crate::smol::{exec_args, Machine, Smol};
use crate::util::{self, ident};
use anyhow::{bail, Context, Result};

/// Resolve `secret_env` entries that exist in the host environment to
/// `GUEST_VAR=HOST_VAR` secret references. Returns (refs, missing names).
fn secret_refs(eff: &crate::config::EffectiveDemo) -> (Vec<String>, Vec<String>) {
    let (mut refs, mut missing) = (Vec::new(), Vec::new());
    for name in &eff.secret_env {
        match std::env::var(name) {
            Ok(v) if !v.is_empty() => refs.push(format!("{name}={name}")),
            _ => missing.push(name.clone()),
        }
    }
    (refs, missing)
}

fn machine_name(demo: &str, idx: i64) -> String {
    format!("democtl-{}-{idx}", ident(demo))
}

fn create_args(
    loaded: &Loaded,
    eff: &crate::config::EffectiveDemo,
    name: &str,
    idx: i64,
    secrets: &[String],
) -> Vec<String> {
    let mut a: Vec<String> = [
        "machine",
        "create",
        "--name",
        name,
        "--from",
        loaded.golden_path.to_string_lossy().as_ref(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if eff.net {
        a.push("--net".into());
    }
    a.push("-p".into());
    a.push(format!("{}:{}", eff.host_port, eff.guest_port));
    for (k, v) in [
        (LABEL_MANAGED, "1"),
        (LABEL_DEMO, eff.name.as_str()),
        (LABEL_IDX, &idx.to_string()),
    ] {
        a.push("--label".into());
        a.push(format!("{k}={v}"));
    }
    if eff.ssh_agent {
        a.push("--ssh-agent".into());
    }
    if let Some(cpus) = eff.cpus {
        a.push("--cpus".into());
        a.push(cpus.to_string());
    }
    if let Some(mem) = eff.mem_mib {
        a.push("--mem".into());
        a.push(mem.to_string());
    }
    for s in secrets {
        a.push("--secret-env".into());
        a.push(s.clone());
    }
    a
}

fn refs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

/// Ensure the golden image exists.
fn require_golden(loaded: &Loaded) -> Result<()> {
    if loaded.golden_path.is_file() {
        return Ok(());
    }
    bail!(
        "golden image not found at {} — build it first with `democtl build`",
        loaded.golden_path.display()
    );
}

/// Create a fresh machine for a demo and start it. Returns (machine name, secrets).
pub fn spawn_fresh(
    loaded: &Loaded,
    smol: &Smol,
    eff: &crate::config::EffectiveDemo,
) -> Result<(String, Vec<String>)> {
    require_golden(loaded)?;
    let existing = smol.managed_for_demo(&eff.name)?;
    let mut idx = existing
        .iter()
        .filter_map(|m: &Machine| m.idx())
        .max()
        .unwrap_or(0)
        + 1;
    let mut name = machine_name(&eff.name, idx);
    while smol.status(&name)?.is_some() {
        idx += 1;
        name = machine_name(&eff.name, idx);
    }

    let (secrets, missing) = secret_refs(eff);
    if !missing.is_empty() {
        util::warn(&format!(
            "secret env not set in your shell (skipped): {}",
            missing.join(", ")
        ));
    }
    if eff.secret_env.is_empty() {
        util::warn("no secret_env configured — opencode in the VM may have no provider API key");
    }

    let args = create_args(loaded, eff, &name, idx, &secrets);
    smol.run_inherit(&refs(&args))
        .context("creating demo machine")?;
    smol.run_inherit(&["machine", "start", "--name", &name])
        .context("starting demo machine")?;

    Ok((name, secrets))
}

fn print_ready(eff: &crate::config::EffectiveDemo, name: &str) {
    let port = format!("http://localhost:{}", eff.host_port);
    println!(
        "\n{} demo `{}` is live as {}\n  url    {port}   (host {} → guest {})\n  attach {}\n  stop   {}",
        util::green("✓"),
        eff.name,
        util::bold(name),
        eff.host_port,
        eff.guest_port,
        util::bold(&format!("democtl open {}", eff.name)),
        util::bold(&format!("democtl stop {}", eff.name)),
    );
}

pub fn new(loaded: &Loaded, smol: &Smol, demo: &str, attach: bool) -> Result<()> {
    let eff = loaded.config.effective(demo)?;
    let tpl = &loaded.config.template;
    let (name, secrets) = spawn_fresh(loaded, smol, &eff)?;
    print_ready(&eff, &name);
    if attach {
        let cmd = vec!["opencode".to_string()];
        smol.run_inherit(&refs(&exec_args(
            &name,
            &tpl.workspace,
            true,
            false,
            &secrets,
            &cmd,
        )))?;
        util::warn(&format!(
            "machine still running — `democtl stop {}` when the demo is done",
            eff.name
        ));
    }
    Ok(())
}

fn ensure_running(smol: &Smol, m: &Machine) -> Result<()> {
    if m.running() {
        return Ok(());
    }
    println!(
        "{}",
        util::dim(&format!("==> starting {} (resuming preserved session)", m.name))
    );
    smol.run_inherit(&["machine", "start", "--name", &m.name])
}

pub fn open(loaded: &Loaded, smol: &Smol, demo: &str, use_opencode: bool, cmd: &[String]) -> Result<()> {
    let eff = loaded.config.effective(demo)?;
    let m = target(smol, demo)?;
    ensure_running(smol, &m)?;

    let (secrets, _) = secret_refs(&eff);
    let tpl = &loaded.config.template;
    let program: Vec<String> = if !cmd.is_empty() {
        cmd.to_vec()
    } else if use_opencode {
        vec!["opencode".to_string()]
    } else {
        vec![tpl.shell.clone()]
    };
    smol.run_inherit(&refs(&exec_args(
        &m.name,
        &tpl.workspace,
        true,
        false,
        &secrets,
        &program,
    )))?;
    if m.running() {
        util::warn(&format!(
            "machine still running — `democtl stop {}` when the demo is done",
            eff.name
        ));
    }
    Ok(())
}

pub fn exec(loaded: &Loaded, smol: &Smol, demo: &str, detach: bool, cmd: &[String]) -> Result<()> {
    if cmd.is_empty() {
        bail!("nothing to run — try: democtl exec {demo} -- ls -la");
    }
    let eff = loaded.config.effective(demo)?;
    let m = target(smol, demo)?;
    ensure_running(smol, &m)?;
    let (secrets, _) = secret_refs(&eff);
    let interactive = !detach && std::io::IsTerminal::is_terminal(&std::io::stdin());
    let tpl = &loaded.config.template;
    smol.run_inherit(&refs(&exec_args(
        &m.name,
        &tpl.workspace,
        interactive,
        detach,
        &secrets,
        cmd,
    )))
}

pub fn url(loaded: &Loaded, smol: &Smol, demo: &str) -> Result<()> {
    let eff = loaded.config.effective(demo)?;
    let m = smol.newest_for_demo(demo)?;
    println!("http://localhost:{}", eff.host_port);
    if let Some(m) = m {
        eprintln!(
            "{}",
            util::dim(&format!("{} is {}", m.name, m.state))
        );
    } else {
        eprintln!("{}", util::dim(&format!("no machine yet — `democtl new {demo}`")));
    }
    Ok(())
}

pub fn start(loaded: &Loaded, smol: &Smol, demo: &str) -> Result<()> {
    let eff = loaded.config.effective(demo)?;
    let m = target(smol, demo)?;
    if m.running() {
        util::warn(&format!("{} is already running", m.name));
        return Ok(());
    }
    ensure_running(smol, &m)?;
    print_ready(&eff, &m.name);
    Ok(())
}

pub fn stop(_loaded: &Loaded, smol: &Smol, demo: &str) -> Result<()> {
    let m = target(smol, demo)?;
    if !m.running() {
        util::warn(&format!("{} is already stopped (session preserved)", m.name));
        return Ok(());
    }
    smol.run_inherit(&["machine", "stop", "--name", &m.name])?;
    util::ok(&format!(
        "{} stopped — session preserved on disk, port stays reserved for this demo",
        m.name
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_args_shape() {
        let loaded = test_loaded();
        let eff = crate::config::EffectiveDemo {
            name: "checkout".into(),
            host_port: 8410,
            guest_port: 8080,
            net: true,
            ssh_agent: true,
            cpus: None,
            mem_mib: None,
            secret_env: vec!["K".into()],
        };
        let a = create_args(&loaded, &eff, "democtl-checkout-1", 1, &["K=K".to_string()]);
        let s: Vec<&str> = a.iter().map(String::as_str).collect();
        assert_eq!(
            s,
            vec![
                "machine", "create", "--name", "democtl-checkout-1", "--from", "/p/.democtl/golden.smolmachine",
                "--net", "-p", "8410:8080",
                "--label", "democtl=1", "--label", "demo=checkout", "--label", "idx=1",
                "--ssh-agent", "--secret-env", "K=K",
            ]
        );
    }

    fn test_loaded() -> Loaded {
        Loaded {
            config: crate::config::Config {
                template: crate::config::TemplateCfg {
                    image: "debian:bookworm-slim".into(),
                    cpus: None,
                    mem_mib: None,
                    packages: vec![],
                    setup: None,
                    shell: "/bin/bash".into(),
                    workspace: "/workspace".into(),
                },
                defaults: crate::config::DefaultsCfg {
                    net: true,
                    ssh_agent: true,
                    secret_env: vec![],
                    guest_port: 8080,
                    cpus: None,
                    mem_mib: None,
                },
                demos: Default::default(),
            },
            dot_dir: "/p/.democtl".into(),
            template_name: "democtl-template-p".into(),
            golden_path: "/p/.democtl/golden.smolmachine".into(),
        }
    }

    #[test]
    fn machine_names_are_ident_safe() {
        assert_eq!(machine_name("My Demo!", 3), "democtl-my-demo-3");
        assert_eq!(crate::config::ROLE_TEMPLATE, "template");
    }
}
