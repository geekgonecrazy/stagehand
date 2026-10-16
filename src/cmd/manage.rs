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

    // width computation
    let mut demo_w = "demo".len();
    let mut mach_w = "machine".len();
    for name in loaded.config.demos.keys() {
        demo_w = demo_w.max(name.len());
    }
    for m in &managed {
        mach_w = mach_w.max(m.name.len());
    }

    println!(
        "{:<demo_w$}  {:<mach_w$}  {:<9}  url",
        util::bold("demo"),
        util::bold("machine"),
        util::bold("state"),
    );

    let mut referenced = std::collections::BTreeSet::new();
    for name in loaded.config.demos.keys() {
        let machines: Vec<_> = managed.iter().filter(|m| m.demo() == Some(name.as_str())).collect();
        let newest = machines.iter().copied().max_by_key(|m| (m.idx(), m.created_at));
        for m in &machines {
            referenced.insert(m.name.clone());
        }
        let eff = loaded.config.effective(name).ok();
        match newest {
            Some(m) => {
                println!(
                    "{:<demo_w$}  {:<mach_w$}  {:<9}  http://localhost:{}",
                    name,
                    m.name,
                    state_cell(&m.state),
                    eff.as_ref().map(|e| e.host_port).unwrap_or(0),
                );
                if machines.len() > 1 {
                    println!(
                        "{:<demo_w$}  {:<mach_w$}  {:<9}  {}",
                        "",
                        util::dim(&format!("+ {} older (preserved)", machines.len() - 1)),
                        util::dim(""),
                        util::dim("stagehand rm --all to clean up"),
                    );
                }
            }
            None => {
                let url = eff
                    .map(|e| format!("http://localhost:{}", e.host_port))
                    .unwrap_or_else(|| "-".into());
                println!("{:<demo_w$}  {:<mach_w$}  {:<9}  {}", name, "-", util::dim("-"), url);
            }
        }
    }

    // machines without a matching demo (or template machine)
    let orphans: Vec<_> = managed.iter().filter(|m| !referenced.contains(&m.name)).collect();
    if !orphans.is_empty() {
        println!("{}", util::dim("— managed but not configured —"));
        for m in orphans {
            let kind = if m.is_template() {
                "template"
            } else if m.is_builder() {
                "builder"
            } else {
                "orphan"
            };
            println!(
                "{:<demo_w$}  {:<mach_w$}  {:<9}  {}",
                util::dim(kind),
                m.name,
                state_cell(&m.state),
                util::dim("stagehand rm/prune to clean up"),
            );
        }
    }

    let running = managed.iter().filter(|m| m.running()).count();
    let total = managed.len();
    println!(
        "\n{} managed machine(s), {} running",
        util::bold(&total.to_string()),
        util::bold(&running.to_string())
    );
    Ok(())
}

pub fn reset(loaded: &Loaded, smol: &Smol, demo_name: &str) -> Result<()> {
    let eff = loaded.config.effective(demo_name)?;
    if let Some(m) = smol.newest_for_demo(demo_name)? {
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

pub fn rm(loaded: &Loaded, smol: &Smol, demo_name: &str, all: bool, yes: bool) -> Result<()> {
    loaded.config.effective(demo_name)?; // validate demo name
    let machines = smol.managed_for_demo(demo_name)?;
    if machines.is_empty() {
        util::warn(&format!("no machines for demo `{demo_name}`"));
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

pub fn prune(loaded: &Loaded, smol: &Smol, include_template: bool, yes: bool) -> Result<()> {
    let managed = smol.managed()?;
    let targets: Vec<_> = managed
        .iter()
        .filter(|m| include_template || !m.is_template())
        .cloned()
        .collect();
    if targets.is_empty() {
        util::warn("nothing to prune — no managed machines");
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
    util::ok("prune complete");
    Ok(())
}
