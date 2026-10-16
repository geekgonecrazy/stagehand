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

impl Cmd {
    /// The optional demo name carried by per-demo commands.
    pub fn demo_name(&self) -> Option<String> {
        match self {
            Cmd::New { demo, .. }
            | Cmd::Open { demo, .. }
            | Cmd::Exec { demo, .. }
            | Cmd::Url { demo }
            | Cmd::Start { demo }
            | Cmd::Stop { demo }
            | Cmd::Reset { demo }
            | Cmd::Rm { demo, .. } => demo.clone(),
            _ => None,
        }
    }
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

    /// Start a fresh VM from the golden image and land in its entrypoint
    New {
        /// Demo name from demo.toml (optional — uses the default, last
        /// used, or the only configured demo)
        demo: Option<String>,

        /// Run this command instead of the image entrypoint (after `--`)
        #[arg(last = true)]
        cmd: Vec<String>,

        /// Start the VM but don't attach to anything
        #[arg(long)]
        no_attach: bool,
    },

    /// Attach to the most recent machine of a demo
    Open {
        /// Demo name from demo.toml (optional)
        demo: Option<String>,

        /// Run this command instead of the image entrypoint (after `--`)
        #[arg(last = true)]
        cmd: Vec<String>,
    },

    /// Run a command in the most recent machine of a demo
    Exec {
        /// Demo name from demo.toml (optional)
        demo: Option<String>,

        /// Run the command in the background (e.g. a demo webserver)
        #[arg(long)]
        detach: bool,

        /// Command to run (after `--`)
        #[arg(last = true)]
        cmd: Vec<String>,
    },

    /// Print the URL of the most recent machine of a demo
    Url {
        /// Demo name from demo.toml (optional)
        demo: Option<String>,
    },

    /// Start the most recent machine of a demo (resume a preserved session)
    Start {
        /// Demo name from demo.toml (optional)
        demo: Option<String>,
    },

    /// Stop the most recent machine of a demo (session is preserved on disk)
    Stop {
        /// Demo name from demo.toml (optional)
        demo: Option<String>,
    },

    /// List demos, their machines, states and URLs
    #[command(alias = "ls")]
    List,

    /// Delete the most recent machine of a demo and start a fresh one
    Reset {
        /// Demo name from demo.toml (optional)
        demo: Option<String>,
    },

    /// Delete machines of a demo
    #[command(alias = "delete")]
    Rm {
        /// Demo name from demo.toml (optional)
        demo: Option<String>,

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
