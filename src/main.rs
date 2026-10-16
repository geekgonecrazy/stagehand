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
        Cmd::List => cmd::manage::list(&config::load(cli.config.as_deref())?, &smol),
        Cmd::Strike { template, yes } => {
            cmd::manage::strike(&config::load(cli.config.as_deref())?, &smol, template, yes)
        }
        Cmd::New { no_attach, cmd } => {
            let loaded = config::load(cli.config.as_deref())?;
            cmd::demo::new(&loaded, &smol, no_attach, &cmd)
        }
        Cmd::Exec { detach, cmd } => {
            let loaded = config::load(cli.config.as_deref())?;
            cmd::demo::exec(&loaded, &smol, detach, &cmd)
        }
        Cmd::Url => {
            let loaded = config::load(cli.config.as_deref())?;
            cmd::demo::url(&loaded, &smol)
        }
        Cmd::Resume { session, cmd } => {
            let loaded = config::load(cli.config.as_deref())?;
            cmd::demo::resume(&loaded, &smol, session, &cmd)
        }
        Cmd::Stop => {
            let loaded = config::load(cli.config.as_deref())?;
            cmd::demo::stop(&loaded, &smol)
        }
        Cmd::Reset => {
            let loaded = config::load(cli.config.as_deref())?;
            cmd::manage::reset(&loaded, &smol)
        }
        Cmd::Rm { all, yes } => {
            let loaded = config::load(cli.config.as_deref())?;
            cmd::manage::rm(&loaded, &smol, all, yes)
        }
    }
}
