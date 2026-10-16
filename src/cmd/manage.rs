use crate::cmd::demo;
use crate::config::{self, Loaded};
use crate::smol::Smol;
use crate::util::{self, state_cell};
use anyhow::{bail, Context, Result};

pub fn init(config_flag: &Option<std::path::PathBuf>) -> Result<()> {
    let path = match config_flag {
        Some(p) => p.clone(),
        None => std::path::PathBuf::from("demo.toml"),
    };
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    std::fs::write(&path, config::STARTER).with_context(|| format!("writing {}", path.display()))?;
    util::ok(&format!("wrote {}", path.display()));
    println!(
        "\nnext:\n  1. edit [demos.*] — one section per demo, each with a fixed {}\n  2. {}  (installs your tools into a template VM, once)\n  3. {}",
        util::bold("port"),
        util::bold("stagehand build"),
        util::bold("stagehand new <demo>")
    );
    Ok(())
}

pub fn list(loaded: &Loaded, smol: &Smol) -> Result<()> {
    let managed = smol.managed()?;
    let eff = loaded.effective().ok();
    let demo_machines: Vec<_> = managed
        .iter()
        .filter(|m| !m.is_template() && !m.is_builder())
        .collect();

    // width computation
    let mut session_w = "session".len();
    let mut demo_w = "demo".len();
    let mut state_w = "state".len();
    let mut port_w = "port".len();
    let mut mach_w = "machine".len();
    for m in &demo_machines {
        session_w = session_w.max(m.idx().map(|i| i.to_string().len()).unwrap_or(1));
        demo_w = demo_w.max(m.demo().unwrap_or("-").len());
        state_w = state_w.max(m.state.len());
        port_w = port_w.max(eff.as_ref().map(|e| e.host_port.to_string().len()).unwrap_or(4));
        mach_w = mach_w.max(m.name.len());
    }

    println!(
        "{:<session_w$}  {:<demo_w$}  {:<state_w$}  {:<port_w$}  {}",
        util::bold("session"),
        util::bold("demo"),
        util::bold("state"),
        util::bold("port"),
        util::bold("machine"),
    );

    // oldest first: session history reads top-down
    for m in &demo_machines {
        let session = m.idx().map(|i| i.to_string()).unwrap_or_else(|| "-".into());
        println!(
            "{:<session_w$}  {:<demo_w$}  {:<state_w$}  {:<port_w$}  {}",
            session,
            m.demo().unwrap_or("-"),
            state_cell(&m.state),
            eff.as_ref().map(|e| e.host_port.to_string()).unwrap_or_else(|| "-".into()),
            m.name,
        );
    }
    if demo_machines.is_empty() {
        println!(
            "{:<session_w$}  {:<demo_w$}  {:<state_w$}  {:<port_w$}  {}",
            util::dim("-"),
            util::dim("-"),
            util::dim("-"),
            util::dim("-"),
            util::dim("(no sessions yet — `stagehand new`)"),
        );
    }

    // infra machines (template/builder) shown separately
    let infra: Vec<_> = managed.iter().filter(|m| m.is_template() || m.is_builder()).collect();
    if !infra.is_empty() {
        println!("\n{}", util::dim("— build infrastructure —"));
        for m in &infra {
            println!("{}  {}", util::dim(&m.name), state_cell(&m.state));
        }
    }

    // unknown managed machines (labels point at this tool but nothing else)
    let known: std::collections::BTreeSet<_> = demo_machines
        .iter()
        .map(|m| m.name.clone())
        .chain(infra.iter().map(|m| m.name.clone()))
        .collect();
    let orphans: Vec<_> = managed.iter().filter(|m| !known.contains(&m.name)).collect();
    if !orphans.is_empty() {
        println!("\n{}", util::dim("— managed but not recognized —"));
        for m in orphans {
            println!("{}  {}", m.name, state_cell(&m.state));
        }
    }

    let running = managed.iter().filter(|m| m.running()).count();
    println!(
        "\n{} managed machine(s), {} running",
        util::bold(&managed.len().to_string()),
        util::bold(&running.to_string())
    );
    Ok(())
}

pub fn reset(loaded: &Loaded, smol: &Smol) -> Result<()> {
    let eff = loaded.effective()?;
    if let Some(m) = smol.newest_for_project(&loaded.slug)? {
        delete_machine(smol, &m)?;
    }
    let (name, _) = demo::spawn_fresh(loaded, smol, &eff)?;
    println!();
    println!(
        "{} fresh machine {} is live at http://localhost:{}",
        util::green("✓"),
        util::bold(&name),
        eff.host_port
    );
    Ok(())
}

fn delete_machine(smol: &Smol, m: &crate::smol::Machine) -> Result<()> {
    if m.running() {
        smol.run_inherit(&["machine", "stop", "--name", &m.name])?;
    }
    smol.run_inherit(&["machine", "delete", "--name", &m.name, "-f"])
        .with_context(|| format!("deleting {}", m.name))?;
    util::ok(&format!("deleted {}", m.name));
    Ok(())
}

pub fn rm(loaded: &Loaded, smol: &Smol, all: bool, yes: bool) -> Result<()> {
    let eff = loaded.effective()?;
    let machines = smol.managed_for_project(&eff.name)?;
    if machines.is_empty() {
        util::warn("no machines yet — `stagehand new`");
        return Ok(());
    }
    let targets: Vec<_> = if all {
        machines
    } else {
        vec![machines.last().unwrap().clone()]
    };
    let names: Vec<&str> = targets.iter().map(|m| m.name.as_str()).collect();
    if !util::confirm(
        &format!("delete {}?", names.join(", ")),
        yes,
    )? {
        bail!("aborted");
    }
    for m in &targets {
        delete_machine(smol, m)?;
    }
    Ok(())
}

pub fn strike(loaded: &Loaded, smol: &Smol, include_template: bool, yes: bool) -> Result<()> {
    let managed = smol.managed()?;
    let targets: Vec<_> = managed
        .iter()
        .filter(|m| include_template || !m.is_template())
        .cloned()
        .collect();
    if targets.is_empty() {
        util::warn("nothing to clean — no managed machines");
        return Ok(());
    }
    let names: Vec<&str> = targets.iter().map(|m| m.name.as_str()).collect();
    if !util::confirm(&format!("delete {}?", names.join(", ")), yes)? {
        bail!("aborted");
    }
    for m in &targets {
        delete_machine(smol, m)?;
    }
    if include_template {
        println!(
            "golden image kept at {} — delete it by hand if you want a full rebuild",
            loaded.golden_path.display()
        );
    }
    util::ok("strike complete — set struck");
    Ok(())
}
