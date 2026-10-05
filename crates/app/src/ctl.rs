use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use oxide_model::{Command, Request};
use std::path::Path;
#[derive(Clone, ValueEnum)]
pub enum Toggle {
    On,
    Off,
}
#[derive(Subcommand)]
pub enum Action {
    Status,
    CloseAll,
    Rename {
        id: String,
        name: String,
    },
    Test {
        name: String,
        #[arg(long,default_value=oxide_client::view::DEFAULT_TEST_URL)]
        url: String,
    },
    Reload,
    Import {
        source: String,
        #[arg(long, default_value = "")]
        name: String,
    },
    Refresh {
        id: String,
    },
    Use {
        id: String,
    },
    Remove {
        id: String,
    },
    Select {
        group: String,
        proxy: String,
    },
    Tun {
        value: Toggle,
    },
    SystemProxy {
        value: Toggle,
    },
    Mode {
        mode: String,
    },
    Port {
        port: u16,
    },
    Close {
        id: String,
    },
}
pub fn run(socket: &Path, action: Action) -> Result<()> {
    let command = match action {
        Action::Status => None,
        Action::CloseAll => Some(Command::CloseAllConnections),
        Action::Rename { id, name } => Some(Command::RenameProfile { id, name }),
        Action::Test { name, url } => Some(Command::TestProxy { name, url }),
        Action::Reload => Some(Command::Reload),
        Action::Import { source, name } => Some(Command::Import { source, name }),
        Action::Refresh { id } => Some(Command::Refresh { id }),
        Action::Use { id } => Some(Command::SwitchProfile { id }),
        Action::Remove { id } => Some(Command::RemoveProfile { id }),
        Action::Select { group, proxy } => Some(Command::SelectProxy { group, proxy }),
        Action::Tun { value } => Some(Command::SetTun(matches!(value, Toggle::On))),
        Action::SystemProxy { value } => Some(Command::SetSystemProxy(matches!(value, Toggle::On))),
        Action::Mode { mode } => Some(Command::SetMode(mode)),
        Action::Port { port } => Some(Command::SetMixedPort(port)),
        Action::Close { id } => Some(Command::CloseConnection { id }),
    };
    let state = oxide_client::request(
        socket,
        command.map(Request::Command).unwrap_or(Request::Snapshot),
    )?;
    println!("{}", serde_json::to_string_pretty(&state)?);
    Ok(())
}
