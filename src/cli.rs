use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "stagehand",
    version,
    about = "Run repeatable conference demos in disposable smolvm micro-VMs",
    after_help = "Typical flow:\n  stagehand init              write a starter demo.toml\n  stagehand build             build the golden image (once)\n  stagehand new               fresh VM, fixed port, drop straight in\n                              (exiting stops the machine — session preserved)\n  stagehand resume            resume a preserved session, drop back in"
)]
pub struct Cli {
    /// Path to demo.toml (default: search upwards from the current directory)
    #[arg(long, short = 'c', global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Write a starter demo.toml in the current directory
    Init,

    /// Build (or refresh) the golden image with your tools preinstalled
    Build {
        /// Recreate the template machine from scratch
        #[arg(long)]
        force: bool,
    },

    /// Start a fresh VM from the golden image, land in its entrypoint; exiting stops it
    New {
        /// Run this command instead of the image entrypoint (after `--`)
        #[arg(last = true)]
        cmd: Vec<String>,

        /// Start the VM but don't attach to anything
        #[arg(long)]
        no_attach: bool,
    },

    /// Run a command in the most recent machine
    Exec {
        /// Run the command in the background (e.g. a demo webserver)
        #[arg(long)]
        detach: bool,

        /// Command to run (after `--`)
        #[arg(last = true)]
        cmd: Vec<String>,
    },

    /// Print the URL of the demo
    Url,

    /// Resume a preserved session, land inside it, and stop it on exit
    Resume {
        /// Session number to resume (see `stagehand list`); default: newest
        session: Option<u32>,

        /// Run this command instead of the image entrypoint (after `--`)
        #[arg(last = true)]
        cmd: Vec<String>,
    },

    /// Halt the running session (session is preserved on disk)
    Stop,

    /// List sessions, their demo, state, port and machine
    #[command(alias = "ls")]
    List,

    /// Delete the most recent session and start a fresh one
    Reset,

    /// Delete machines of this demo
    #[command(alias = "delete")]
    Rm {
        /// Delete every machine of this demo, not just the most recent
        #[arg(long)]
        all: bool,

        /// Skip the confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Delete every stagehand-managed machine (after the conference)
    #[command(visible_alias = "cleanup")]
    Strike {
        /// Also delete the template machine (the golden image is kept)
        #[arg(long)]
        template: bool,

        /// Skip the confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
}
