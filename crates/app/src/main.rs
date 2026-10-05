mod ctl;

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use oxide_client::lifecycle::DaemonOptions;
use oxide_i18n::tr;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = tr!("cli.about"))]
struct Args {
    /// Print the features included in this executable.
    #[arg(long, help = tr!("cli.build_info"))]
    build_info: bool,
    /// Override the IPC socket (also disables systemd auto-discovery).
    #[arg(long, global = true, help = tr!("cli.socket"))]
    socket: Option<PathBuf>,
    /// Override the configuration directory (also disables systemd auto-discovery).
    #[arg(long, global = true, help = tr!("cli.data_dir"))]
    data_dir: Option<PathBuf>,
    #[arg(long, global = true, value_parser = ["auto", "en", "zh-CN"], help = tr!("cli.lang"))]
    lang: Option<String>,
    #[command(subcommand)]
    command: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Open the desktop interface and start the daemon if needed.
    #[command(about = tr!("cli.gui"))]
    Gui,
    /// Open the terminal interface and start the daemon if needed.
    #[command(about = tr!("cli.tui"))]
    Tui,
    /// Run the daemon in the foreground.
    #[command(about = tr!("cli.daemon"))]
    Daemon {
        #[command(subcommand)]
        command: Option<oxide_daemon::Action>,
    },
    /// Control an existing daemon; print JSON.
    #[command(about = tr!("cli.ctl"))]
    Ctl {
        #[command(subcommand)]
        command: ctl::Action,
    },
    /// Shut down the daemon after restoring network settings.
    #[command(about = tr!("cli.kill"))]
    Kill,
}

fn main() -> std::process::ExitCode {
    // Select a language before Clap renders help; `--` terminates option scanning.
    let mut arguments = std::env::args_os().skip(1);
    let mut language = None;
    while let Some(argument) = arguments.next() {
        if argument == "--" {
            break;
        }
        if argument == "--lang" {
            language = arguments.next().and_then(|s| s.into_string().ok());
        } else if let Some(value) = argument.to_str().and_then(|s| s.strip_prefix("--lang=")) {
            language = Some(value.to_owned());
        }
    }
    oxide_i18n::initialize(language.as_deref());
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "{}",
                oxide_i18n::diagnostic(&oxide_model::Diagnostic::from_error(&error))
            );
            std::process::ExitCode::FAILURE
        }
    }
}

fn help(mut command: clap::Command) -> clap::Command {
    command = command.disable_help_flag(true).arg(
        clap::Arg::new("help")
            .long("help")
            .short('h')
            .action(clap::ArgAction::Help)
            .help(tr!("cli.help")),
    );
    if command.get_version().is_some() {
        command = command.disable_version_flag(true).arg(
            clap::Arg::new("version")
                .long("version")
                .short('V')
                .action(clap::ArgAction::Version)
                .help(tr!("cli.version")),
        );
    }
    command = command
        .help_template(format!(
            "{{about-with-newline}}\n{}: {{usage}}\n\n{{all-args}}",
            tr!("cli.usage")
        ))
        .subcommand_help_heading(tr!("cli.commands"))
        .mut_args(|arg| {
            let heading = if arg.is_positional() {
                tr!("cli.arguments")
            } else {
                tr!("cli.options")
            };
            arg.help_heading(heading)
        });
    let names: Vec<_> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .collect();
    for name in names {
        command = command.mut_subcommand(name, help);
    }
    command
}

fn run() -> Result<()> {
    let matches = help(Args::command()).get_matches();
    let args = Args::from_arg_matches(&matches)?;
    if args.build_info {
        println!("gui={}", cfg!(feature = "gui"));
        return Ok(());
    }
    let options = DaemonOptions::resolve(args.socket, args.data_dir)?;
    let command = args.command.unwrap_or(if cfg!(feature = "gui") {
        Action::Gui
    } else {
        Action::Tui
    });
    match command {
        Action::Gui => {
            #[cfg(feature = "gui")]
            oxide_gui::run(options)?;
            #[cfg(not(feature = "gui"))]
            anyhow::bail!(oxide_model::Diagnostic::new(
                "error.no_gui",
                "This build has no GUI. Use `clash-oxide tui` or install the full build."
            ));
        }
        Action::Tui => oxide_tui::run(options)?,
        Action::Daemon { command } => oxide_daemon::run(oxide_daemon::Options {
            socket: options.socket,
            data_dir: options.data_dir,
            command,
        })?,
        Action::Ctl { command } => ctl::run(&options.socket, command)?,
        Action::Kill => {
            if oxide_client::lifecycle::shutdown(&options)? {
                println!("{}", tr!("cli.stopped"));
            } else {
                println!("{}", tr!("cli.not_running"));
            }
        }
    }
    Ok(())
}
