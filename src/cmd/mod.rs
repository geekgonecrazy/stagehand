pub mod build;
pub mod demo;
pub mod manage;

use crate::smol::{Machine, Smol};
use anyhow::Result;

/// The machine a per-demo command targets: the newest one, else a helpful error.
pub fn target(smol: &Smol, demo: &str) -> Result<Machine> {
    smol.newest_for_demo(demo)?.ok_or_else(|| {
        anyhow::anyhow!(
            "no machine for demo `{demo}` yet — start one with `stagehand new {demo}`"
        )
    })
}
