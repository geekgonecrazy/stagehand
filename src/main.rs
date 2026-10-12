mod cli;
mod cmd;
mod config;
mod smol;
mod util;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Cmd};
use smol::Smol;

fn main() {
    if let Err(e) = run() {
        eprintln!("{} {e:#}", util::red("error:"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let smol = Smol::new()?;

    match cli.cmd {
        Cmd::Init => cmd::manage::init(&cli.config),
        Cmd::Build { force } => cmd::build::run(&config::load(cli.config.as_deref())?, &smol, force),
        Cmd::New { demo, attach } => {
            cmd::demo::new(&config::load(cli.config.as_deref())?, &smol, &demo, attach)
        }
        Cmd::Open { demo, opencode, cmd } => {
            cmd::demo::open(&config::load(cli.config.as_deref())?, &smol, &demo, opencode, &cmd)
        }
        Cmd::Exec { demo, detach, cmd } => {
            cmd::demo::exec(&config::load(cli.config.as_deref())?, &smol, &demo, detach, &cmd)
        }
        Cmd::Url { demo } => cmd::demo::url(&config::load(cli.config.as_deref())?, &smol, &demo),
        Cmd::Start { demo } => cmd::demo::start(&config::load(cli.config.as_deref())?, &smol, &demo),
        Cmd::Stop { demo } => cmd::demo::stop(&config::load(cli.config.as_deref())?, &smol, &demo),
        Cmd::List => cmd::manage::list(&config::load(cli.config.as_deref())?, &smol),
        Cmd::Reset { demo } => cmd::manage::reset(&config::load(cli.config.as_deref())?, &smol, &demo),
        Cmd::Rm { demo, all, yes } => {
            cmd::manage::rm(&config::load(cli.config.as_deref())?, &smol, &demo, all, yes)
        }
        Cmd::Prune { template, yes } => {
            cmd::manage::prune(&config::load(cli.config.as_deref())?, &smol, template, yes)
        }
    }
}
