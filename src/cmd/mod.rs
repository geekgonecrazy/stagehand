pub mod build;
pub mod demo;
pub mod manage;

use crate::config::Loaded;
use crate::smol::{Machine, Smol};
use anyhow::Result;

/// The machine a project command targets: the newest one, else a helpful error.
pub fn target(smol: &Smol, loaded: &Loaded) -> Result<Machine> {
    let name = loaded.effective()?.name;
    smol.newest_for_project(&name)?.ok_or_else(|| {
        anyhow::anyhow!("no machine yet for this demo — start one with `stagehand new`")
    })
}
