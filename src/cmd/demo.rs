use crate::cmd::target;
use crate::config::{EffectiveDemo, Loaded, LABEL_DEMO, LABEL_IDX, LABEL_MANAGED};
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

/// Host env vars that must be present when the machine is started:
/// plain secrets (ref resolution) and credentials (request-time substitution).
fn missing_env(eff: &EffectiveDemo) -> Vec<String> {
    let mut missing: Vec<String> = eff
        .secret_env
        .iter()
        .chain(eff.credentials.keys())
        .filter(|name| match std::env::var(name) {
            Ok(v) => v.is_empty(),
            Err(_) => true,
        })
        .cloned()
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

fn warn_missing_env(eff: &EffectiveDemo, when: &str) {
    let missing = missing_env(eff);
    if missing.is_empty() {
        return;
    }
    let creds: Vec<&String> = missing
        .iter()
        .filter(|n| eff.credentials.contains_key(*n))
        .collect();
    util::warn(&format!(
        "env not set in your shell ({}): {}{}",
        when,
        missing.join(", "),
        if creds.is_empty() {
            String::new()
        } else {
            format!(
                "\n  credential-bound requests will fail until these are set and the machine is restarted: {}",
                creds.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            )
        }
    ));
}

fn create_args(
    loaded: &Loaded,
    eff: &EffectiveDemo,
    name: &str,
    idx: i64,
    secrets: &[String],
) -> Vec<String> {
    let mut a: Vec<String> = [
        "machine",
        "create",
        "--name",
        name,
        "--image",
        loaded.rootfs_path.to_string_lossy().as_ref(),
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
    // Host-side credential bindings: the guest only ever sees a
    // SMOL_PLACEHOLDER_* env value; smolvm substitutes the real value from the
    // start-time environment on HTTPS requests to the bound hosts. The
    // --secret-env ref is what carries the value into the VM's runtime.
    for (name, hosts) in &eff.credentials {
        a.push("--credential".into());
        a.push(format!("{name}={name}@{}", hosts.join(",")));
        a.push("--secret-env".into());
        a.push(format!("{name}={name}"));
    }
    for s in secrets {
        a.push("--secret-env".into());
        a.push(s.clone());
    }
    if let Some(cpus) = eff.cpus {
        a.push("--cpus".into());
        a.push(cpus.to_string());
    }
    if let Some(mem) = eff.mem_mib {
        a.push("--mem".into());
        a.push(mem.to_string());
    }
    a
}

fn refs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

/// Ensure the golden rootfs exists.
fn require_golden(loaded: &Loaded) -> Result<()> {
    if loaded.rootfs_path.is_dir() {
        return Ok(());
    }
    bail!(
        "golden rootfs not found at {} — build it first with `democtl build`",
        loaded.rootfs_path.display()
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

    // Only one live instance per demo: the fixed host port can't be bound
    // twice. Archive (stop) any previous run — it stays preserved on disk.
    for m in existing.iter().filter(|m| m.running()) {
        println!(
            "{}",
            util::dim(&format!(
                "==> archiving previous run {} (session preserved)",
                m.name
            ))
        );
        smol.run_inherit(&["machine", "stop", "--name", &m.name])?;
    }

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
    warn_missing_env(eff, "the machine starts without these");
    if eff.secret_env.is_empty() && eff.credentials.is_empty() {
        util::warn("no credentials configured — opencode in the VM may have no provider API key");
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

fn ensure_running(smol: &Smol, m: &Machine, eff: &EffectiveDemo) -> Result<()> {
    if m.running() {
        return Ok(());
    }
    warn_missing_env(eff, "starting now");
    println!(
        "{}",
        util::dim(&format!("==> starting {} (resuming preserved session)", m.name))
    );
    smol.run_inherit(&["machine", "start", "--name", &m.name])
}

pub fn open(loaded: &Loaded, smol: &Smol, demo: &str, use_opencode: bool, cmd: &[String]) -> Result<()> {
    let eff = loaded.config.effective(demo)?;
    let m = target(smol, demo)?;
    ensure_running(smol, &m, &eff)?;

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
    ensure_running(smol, &m, &eff)?;
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
    ensure_running(smol, &m, &eff)?;
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
        let mut credentials = std::collections::BTreeMap::new();
        credentials.insert(
            "ANTHROPIC_API_KEY".to_string(),
            vec!["api.anthropic.com".to_string()],
        );
        let eff = crate::config::EffectiveDemo {
            name: "checkout".into(),
            host_port: 8410,
            guest_port: 8080,
            net: true,
            ssh_agent: true,
            cpus: None,
            mem_mib: None,
            secret_env: vec!["K".into()],
            credentials,
        };
        let a = create_args(&loaded, &eff, "democtl-checkout-1", 1, &["K=K".to_string()]);
        let s: Vec<&str> = a.iter().map(String::as_str).collect();
        assert_eq!(
            s,
            vec![
                "machine", "create", "--name", "democtl-checkout-1", "--image", "/p/.democtl/rootfs",
                "--net", "-p", "8410:8080",
                "--label", "democtl=1", "--label", "demo=checkout", "--label", "idx=1",
                "--ssh-agent",
                "--credential", "ANTHROPIC_API_KEY=ANTHROPIC_API_KEY@api.anthropic.com",
                "--secret-env", "ANTHROPIC_API_KEY=ANTHROPIC_API_KEY",
                "--secret-env", "K=K",
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
                    tools: vec![],
                    setup: None,
                    shell: "/bin/bash".into(),
                    workspace: "/workspace".into(),
                },
                build: None,
                defaults: crate::config::DefaultsCfg {
                    demo: None,
                    net: true,
                    ssh_agent: true,
                    secret_env: vec![],
                    credentials: Default::default(),
                    guest_port: 8080,
                    cpus: None,
                    mem_mib: None,
                },
                demos: Default::default(),
            },
            dot_dir: "/p/.democtl".into(),
            template_name: "democtl-template-p".into(),
            golden_path: "/p/.democtl/golden.smolmachine".into(),
            rootfs_path: "/p/.democtl/rootfs".into(),
            builder_name: "democtl-builder-p".into(),
            artifacts_dir: "/p/.democtl/artifacts".into(),
        }
    }

    #[test]
    fn machine_names_are_ident_safe() {
        assert_eq!(machine_name("My Demo!", 3), "democtl-my-demo-3");
        assert_eq!(crate::config::ROLE_TEMPLATE, "template");
    }
}
