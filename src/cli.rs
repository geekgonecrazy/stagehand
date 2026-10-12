use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "democtl",
    version,
    about = "Run repeatable conference demos in disposable smolvm micro-VMs",
    after_help = "Typical flow:\n  democtl init              write a starter demo.toml\n  democtl build             build the golden image (once)\n  democtl new checkout      fresh VM, fixed port, ready to demo\n  democtl stop checkout     preserve the session, free the machine"
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

    /// Build (or refresh) the golden image with opencode preinstalled
    Build {
        /// Recreate the template machine from scratch
        #[arg(long)]
        force: bool,
    },

    /// Start a fresh VM for a demo from the golden image
    New {
        /// Demo name from demo.toml
        demo: String,

        /// Attach to opencode immediately after starting
        #[arg(long)]
        attach: bool,
    },

    /// Attach to the most recent machine of a demo
    Open {
        /// Demo name from demo.toml
        demo: String,

        /// Attach to opencode instead of a shell
        #[arg(long)]
        opencode: bool,

        /// Run this command instead of a shell (after `--`)
        #[arg(last = true)]
        cmd: Vec<String>,
    },

    /// Run a command in the most recent machine of a demo
    Exec {
        /// Demo name from demo.toml
        demo: String,

        /// Run the command in the background (e.g. a demo webserver)
        #[arg(long)]
        detach: bool,

        /// Command to run (after `--`)
        #[arg(last = true)]
        cmd: Vec<String>,
    },

    /// Print the URL of the most recent machine of a demo
    Url {
        /// Demo name from demo.toml
        demo: String,
    },

    /// Start the most recent machine of a demo (resume a preserved session)
    Start {
        /// Demo name from demo.toml
        demo: String,
    },

    /// Stop the most recent machine of a demo (session is preserved on disk)
    Stop {
        /// Demo name from demo.toml
        demo: String,
    },

    /// List demos, their machines, states and URLs
    #[command(alias = "ls")]
    List,

    /// Delete the most recent machine of a demo and start a fresh one
    Reset {
        /// Demo name from demo.toml
        demo: String,
    },

    /// Delete machines of a demo
    #[command(alias = "delete")]
    Rm {
        /// Demo name from demo.toml
        demo: String,

        /// Delete every machine of this demo, not just the most recent
        #[arg(long)]
        all: bool,

        /// Skip the confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Delete every democtl-managed machine (after the conference)
    Prune {
        /// Also delete the template machine (the golden image is kept)
        #[arg(long)]
        template: bool,

        /// Skip the confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
}
