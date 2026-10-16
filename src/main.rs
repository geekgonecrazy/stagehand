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
        Cmd::Prune { template, yes } => {
            cmd::manage::prune(&config::load(cli.config.as_deref())?, &smol, template, yes)
        }
        // Per-demo commands: the demo name is optional and remembered.
        cmd => {
            let loaded = config::load(cli.config.as_deref())?;
            let demo = loaded.resolve_demo(cmd.demo_name().as_deref())?;
            let result = match &cmd {
                Cmd::New { no_attach, .. } => {
                    cmd::demo::new(&loaded, &smol, &demo, !*no_attach)
                }
                Cmd::Open { opencode, cmd, .. } => {
                    cmd::demo::open(&loaded, &smol, &demo, *opencode, cmd)
                }
                Cmd::Exec { detach, cmd, .. } => {
                    cmd::demo::exec(&loaded, &smol, &demo, *detach, cmd)
                }
                Cmd::Url { .. } => cmd::demo::url(&loaded, &smol, &demo),
                Cmd::Start { .. } => cmd::demo::start(&loaded, &smol, &demo),
                Cmd::Stop { .. } => cmd::demo::stop(&loaded, &smol, &demo),
                Cmd::Reset { .. } => cmd::manage::reset(&loaded, &smol, &demo),
                Cmd::Rm { all, yes, .. } => {
                    cmd::manage::rm(&loaded, &smol, &demo, *all, *yes)
                }
                _ => unreachable!("handled above"),
            };
            if result.is_ok() {
                loaded.write_last_demo(&demo);
            }
            result
        }
    }
}
