use crate::cmd::target;
use crate::config::{EffectiveDemo, Loaded, LABEL_DEMO, LABEL_IDX, LABEL_MANAGED};
use crate::smol::{exec_args, Machine, Smol};
use crate::util::{self};
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

fn machine_name(slug: &str, idx: i64) -> String {
    format!("{slug}-{idx}")
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
        "golden rootfs not found at {} — build it first with `stagehand build`",
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
    let existing = smol.managed_for_project(&loaded.slug)?;

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
        util::warn("no credentials configured — tools in the VM that need API keys won't have them");
    }

    let args = create_args(loaded, eff, &name, idx, &secrets);
    smol.run_inherit(&refs(&args))
        .context("creating demo machine")?;
    start_demo_machine(smol, &name, &eff.name)
        .context("starting demo machine")?;

    // Bring in demo folders: fresh copies, host originals untouched.
    bring_includes(loaded, smol, eff, &name)?;

    Ok((name, secrets))
}

/// Extract the holder machine from smolvm's port-conflict error
/// ("host port 8410 is already in use by running machine 'X'").
fn port_conflict_holder(msg: &str) -> Option<String> {
    let marker = "already in use by running machine '";
    let start = msg.find(marker)? + marker.len();
    let rest = &msg[start..];
    let end = rest.find('\'')?;
    Some(rest[..end].to_string())
}

/// Start a demo machine. If the fixed host port is still held by a previous
/// run (label drift, resurrected machines, …), archive the holder and retry
/// once. Refuses to touch machines stagehand doesn't manage.
fn start_demo_machine(smol: &Smol, name: &str, demo: &str) -> Result<()> {
    let attempt = smol.capture_both(&["machine", "start", "--name", name]);
    if attempt.is_ok() {
        return Ok(());
    }
    let msg = format!("{:#}", attempt.unwrap_err());
    let holder = port_conflict_holder(&msg).ok_or_else(|| anyhow::anyhow!("{msg}"))?;
    let holder_machine = smol
        .machines()?
        .into_iter()
        .find(|m| m.name == holder)
        .with_context(|| format!("port is held by `{holder}`, which stagehand doesn't know"))?;
    if !holder_machine.is_managed() || holder_machine.is_template() || holder_machine.is_builder() {
        bail!(
            "host port is held by `{holder}`, which is not a stagehand demo machine — \
             stop it manually (`smolvm machine stop {holder}`) and retry"
        );
    }
    util::warn(&format!(
        "port still held by previous run `{holder}` — archiving it (session preserved) and retrying"
    ));
    smol.run_inherit(&["machine", "stop", "--name", &holder])?;
    smol.capture_both(&["machine", "start", "--name", name]).map(|_| ())
        .with_context(|| format!("starting demo machine after archiving {holder}"))?;
    let _ = demo;
    Ok(())
}

fn humansize(n: u64) -> String {
    let mb = n as f64 / (1024.0 * 1024.0);
    if mb >= 1.0 {
        format!("{mb:.1} MiB")
    } else {
        format!("{:.0} KiB", n as f64 / 1024.0)
    }
}

fn print_ready(eff: &crate::config::EffectiveDemo, name: &str) {
    println!(
        "\n{} demo is live as {} at http://localhost:{}  (host {} → guest {})",
        util::green("✓"),
        util::bold(name),
        eff.host_port,
        eff.host_port,
        eff.guest_port,
    );
}

/// Attach to a machine: `-- cmd` override, else the image entrypoint.
fn attach(
    loaded: &Loaded,
    smol: &Smol,
    machine: &str,
    secrets: &[String],
    cmd: &[String],
) -> Result<()> {
    let build = &loaded.config.build;
    let program: Vec<String> = if !cmd.is_empty() {
        cmd.to_vec()
    } else {
        build.entrypoint_programs()
    };
    smol.run_inherit(&refs(&exec_args(
        machine,
        &build.workspace,
        true,
        false,
        secrets,
        &program,
    )))
}

/// Strike the set: stop the machine, keep the session on disk.
fn strike(smol: &Smol, machine: &str) -> Result<()> {
    smol.run_inherit(&["machine", "stop", "--name", machine])?;
    util::ok(&format!(
        "{machine} stopped — session preserved (`stagehand resume` to jump back in)"
    ));
    Ok(())
}

pub fn new(
    loaded: &Loaded,
    smol: &Smol,
    no_attach: bool,
    cmd: &[String],
) -> Result<()> {
    let eff = loaded.effective()?;
    let (name, secrets) = spawn_fresh(loaded, smol, &eff)?;
    print_ready(&eff, &name);
    if no_attach {
        util::warn("running without attach — `stagehand resume` to step in, `stagehand stop` to halt");
        return Ok(());
    }
    let r = attach(loaded, smol, &name, &secrets, cmd);
    // Exiting the session strikes the set — always.
    if let Err(e) = strike(smol, &name) {
        util::warn(&format!("could not stop {name}: {e}"));
    }
    r
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

pub fn exec(loaded: &Loaded, smol: &Smol, detach: bool, cmd: &[String]) -> Result<()> {
    if cmd.is_empty() {
        bail!("nothing to run — try: stagehand exec -- ls -la");
    }
    let eff = loaded.effective()?;
    let m = target(smol, loaded)?;
    ensure_running(smol, &m, &eff)?;
    let (secrets, _) = secret_refs(&eff);
    let interactive = !detach && std::io::IsTerminal::is_terminal(&std::io::stdin());
    let build = &loaded.config.build;
    smol.run_inherit(&refs(&exec_args(
        &m.name,
        &build.workspace,
        interactive,
        detach,
        &secrets,
        cmd,
    )))
}

pub fn url(loaded: &Loaded, smol: &Smol) -> Result<()> {
    let eff = loaded.effective()?;
    let m = smol.newest_for_project(&loaded.slug)?;
    println!("http://localhost:{}", eff.host_port);
    if let Some(m) = m {
        eprintln!(
            "{}",
            util::dim(&format!("{} is {}", m.name, m.state))
        );
    } else {
        eprintln!("{}", util::dim("no machine yet — `stagehand new`"));
    }
    Ok(())
}

/// Resume the most recent machine, land inside it, and strike it on exit.
pub fn resume(
    loaded: &Loaded,
    smol: &Smol,
    session: Option<u32>,
    cmd: &[String],
) -> Result<()> {
    let eff = loaded.effective()?;
    let machines = smol.managed_for_project(&eff.name)?;
    let m = match session {
        Some(n) => {
            machines
                .iter()
                .find(|m| m.idx() == Some(n as i64))
                .cloned()
                .ok_or_else(|| {
                    let available: Vec<String> = machines
                        .iter()
                        .filter_map(|m| m.idx())
                        .map(|i| i.to_string())
                        .collect();
                    anyhow::anyhow!(
                        "no session {n} — preserved sessions: {}",
                        if available.is_empty() { "(none)".to_string() } else { available.join(", ") }
                    )
                })?
        }
        None => machines.last().cloned().ok_or_else(|| {
            anyhow::anyhow!("no preserved sessions yet — `stagehand new` first")
        })?,
    };
    ensure_running(smol, &m, &eff)?;
    print_ready(&eff, &m.name);
    let (secrets, _) = secret_refs(&eff);
    let r = attach(loaded, smol, &m.name, &secrets, cmd);
    // Exiting the session strikes the set — always.
    if let Err(e) = strike(smol, &m.name) {
        util::warn(&format!("could not stop {}: {e}", m.name));
    }
    r
}

pub fn stop(loaded: &Loaded, smol: &Smol) -> Result<()> {
    let eff = loaded.effective()?;
    let machines = smol.managed_for_project(&eff.name)?;
    if machines.is_empty() {
        util::warn("no machines yet — `stagehand new`");
        return Ok(());
    }
    // Stop whichever session is running (only one can hold the port).
    let m = machines
        .iter()
        .find(|m| m.running())
        .or_else(|| machines.last())
        .cloned()
        .unwrap();
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

/// Resolve an include path: absolute as-is, relative to the demo.toml dir.
fn resolve_include(dir: &std::path::Path, path: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        dir.join(p)
    }
}

/// Copy a host folder into the VM's workspace: tar on the host, `machine cp`
/// the tarball, extract inside. A copy, never a mount — the demo can mangle
/// it freely and the host original doesn't change.
fn copy_include(
    smol: &Smol,
    machine: &str,
    workspace: &str,
    src: &std::path::Path,
) -> Result<()> {
    let name = src
        .file_name()
        .and_then(|n| n.to_str())
        .context("include path has no file name (trailing slash?)")?
        .to_string();
    let tgz = std::env::temp_dir().join(format!(
        "stagehand-include-{}-{}.tgz",
        std::process::id(),
        name
    ));
    // Host tar directly (NOT via smolvm) — we're packaging a host folder.
    let status = std::process::Command::new("tar")
        .args(["-czf"])
        .arg(&tgz)
        .args(["-C"])
        .arg(src.parent().unwrap_or(src))
        .arg(&name)
        .status()
        .context("running tar (host)")?;
    if !status.success() {
        bail!("tar failed while packaging include {}", src.display());
    }

    let size = humansize(std::fs::metadata(&tgz).map(|m| m.len()).unwrap_or(0));
    println!(
        "{}",
        util::dim(&format!("==> bringing in {name} ({size}) — copied, not mounted"))
    );
    let remote_tgz = format!("/tmp/stagehand-include-{name}.tgz");
    // `machine cp` right after `machine start` can silently no-op (agent's
    // transfer channel not up yet, despite the progress bar) — verify the
    // file landed guest-side and retry the copy if not.
    let mut landed = false;
    for attempt in 1..=3 {
        smol.run_inherit(&[
            "machine",
            "cp",
            &tgz.to_string_lossy(),
            &format!("{machine}:{remote_tgz}"),
        ])?;
        if smol
            .capture_both(&["machine", "exec", "--name", machine, "--", "test", "-f", &remote_tgz])
            .is_ok()
        {
            landed = true;
            break;
        }
        util::warn(&format!("include upload didn't land (attempt {attempt}/3) — retrying"));
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let _ = std::fs::remove_file(&tgz);
    if !landed {
        bail!("include {name} never landed in the VM after 3 attempts");
    }
    smol.run_inherit(&[
        "machine",
        "exec",
        "--name",
        machine,
        "--",
        "sh",
        "-c",
        &format!(
            "rm -rf '{ws}/{name}' && tar xzf '{tgz}' -C '{ws}'",
            ws = workspace,
            tgz = remote_tgz
        ),
    ])?;
    Ok(())
}

/// Copy every `include` folder for a demo into the fresh VM's workspace.
fn bring_includes(
    loaded: &Loaded,
    smol: &Smol,
    eff: &crate::config::EffectiveDemo,
    machine: &str,
) -> Result<()> {
    if eff.include.is_empty() {
        return Ok(());
    }
    let workspace = &loaded.config.build.workspace;
    let mut seen = std::collections::BTreeSet::new();
    for path in &eff.include {
        let src = resolve_include(&loaded.dir, path);
        if !src.is_dir() {
            bail!("include path is not a directory: {}", src.display());
        }
        let name = src
            .file_name()
            .and_then(|n| n.to_str())
            .context("include path has no file name")?;
        if !seen.insert(name.to_string()) {
            bail!("two includes share the folder name `{name}` — rename one");
        }
        copy_include(smol, machine, workspace, &src)?;
    }
    println!(
        "{}",
        util::dim(&format!(
            "==> includes copied into {workspace} (fresh copy every `stagehand new`)"
        ))
    );
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
            "MY_API_KEY".to_string(),
            vec!["api.example.com".to_string()],
        );
        let eff = crate::config::EffectiveDemo {
            name: "my-demo".into(),
            host_port: 8410,
            guest_port: 8080,
            net: true,
            ssh_agent: true,
            cpus: None,
            mem_mib: None,
            secret_env: vec!["K".into()],
            credentials,
            include: vec![],
        };
        let a = create_args(&loaded, &eff, "stagehand-my-demo-1", 1, &["K=K".to_string()]);
        let s: Vec<&str> = a.iter().map(String::as_str).collect();
        assert_eq!(
            s,
            vec![
                "machine", "create", "--name", "stagehand-my-demo-1", "--image", "/p/.stagehand/rootfs",
                "--net", "-p", "8410:8080",
                "--label", "stagehand=1", "--label", "demo=my-demo", "--label", "idx=1",
                "--ssh-agent",
                "--credential", "MY_API_KEY=MY_API_KEY@api.example.com",
                "--secret-env", "MY_API_KEY=MY_API_KEY",
                "--secret-env", "K=K",
            ]
        );
    }

    fn test_loaded() -> Loaded {
        Loaded {
            config: crate::config::Config {
                build: crate::config::BuildCfg {
                    image: "debian:bookworm-slim".into(),
                    packages: vec![],
                    tools: vec![],
                    entrypoint: vec![],
                    setup: None,
                    shell: "/bin/bash".into(),
                    workspace: "/workspace".into(),
                },
                builder: None,
                defaults: crate::config::DefaultsCfg {
                    net: true,
                    ssh_agent: true,
                    secret_env: vec![],
                    credentials: Default::default(),
                    guest_port: 8080,
                    cpus: None,
                    mem_mib: None,
                },
                demo: crate::config::DemoCfg {
                    name: None,
                    port: Some(8410),
                    include: None,
                },
            },
            dot_dir: "/p/.stagehand".into(),
            dir: "/p".into(),
            slug: "p".into(),
            template_name: "stagehand-template-p".into(),
            golden_path: "/p/.stagehand/golden.smolmachine".into(),
            rootfs_path: "/p/.stagehand/rootfs".into(),
            builder_name: "stagehand-builder-p".into(),
            artifacts_dir: "/p/.stagehand/artifacts".into(),
        }
    }

    #[test]
    fn machine_names_are_ident_safe() {
        assert_eq!(machine_name("my-demo", 3), "my-demo-3");
        assert_eq!(crate::config::ROLE_TEMPLATE, "template");
    }
}
