use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Locale-independent user message. `detail` remains useful to older clients and
/// for diagnostics; clients translate `code` and interpolate `args` themselves.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    #[serde(default)]
    pub args: std::collections::BTreeMap<String, String>,
    pub detail: String,
}

impl Diagnostic {
    pub fn new(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            args: Default::default(),
            detail: detail.into(),
        }
    }

    pub fn arg(mut self, name: impl Into<String>, value: impl ToString) -> Self {
        self.args.insert(name.into(), value.to_string());
        self
    }

    pub fn from_error(error: &anyhow::Error) -> Self {
        let mut message = error
            .downcast_ref::<Self>()
            .cloned()
            .unwrap_or_else(|| Self::new("error.operation", ""));
        message.detail = format!("{error:#}");
        message
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.detail.fmt(f)
    }
}
impl std::error::Error for Diagnostic {}

pub const PROTOCOL_VERSION: u32 = 2;
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
pub const MAX_PROFILE: usize = 2 * 1024 * 1024;
pub const TUN_DEVICE: &str = "oxide0";
pub const ROUTE_TABLE: u32 = 20260;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Stopped,
    Starting,
    Running,
    Reloading,
    Stopping,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    pub mixed_port: u16,
    pub tun: bool,
    pub system_proxy: bool,
    pub mode: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            mixed_port: 7890,
            tun: false,
            system_proxy: false,
            mode: "rule".into(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub subscription: bool,
    #[serde(default)]
    pub updated_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProxyGroup {
    pub name: String,
    pub kind: String,
    pub selected: String,
    pub members: Vec<String>,
    pub selectable: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProxyInfo {
    pub kind: String,
    pub delay: Option<u64>,
    pub alive: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Rule {
    pub kind: String,
    pub payload: String,
    pub target: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub host: String,
    pub destination: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub started: String,
    #[serde(default)]
    pub rule: String,
    pub network: String,
    pub chain: String,
    pub upload: u64,
    pub download: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Traffic {
    pub upload_rate: u64,
    pub download_rate: u64,
    pub upload_total: u64,
    pub download_total: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EngineSnapshot {
    pub groups: Vec<ProxyGroup>,
    #[serde(default)]
    pub proxies: std::collections::BTreeMap<String, ProxyInfo>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub rule_count: usize,
    #[serde(default)]
    pub testing: Option<String>,
    pub connections: Vec<Connection>,
    pub connection_count: usize,
    pub traffic: Traffic,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision: u64,
    pub phase: Phase,
    pub settings: Settings,
    pub profiles: Vec<Profile>,
    pub active_profile: Option<String>,
    pub engine: EngineSnapshot,
    pub tun_active: bool,
    pub system_proxy_active: bool,
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_diagnostic: Option<Diagnostic>,
    pub warnings: Vec<String>,
    pub logs: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    Reload,
    Import { name: String, source: String },
    Refresh { id: String },
    SwitchProfile { id: String },
    RemoveProfile { id: String },
    SelectProxy { group: String, proxy: String },
    SetTun(bool),
    SetSystemProxy(bool),
    SetMode(String),
    SetMixedPort(u16),
    CloseConnection { id: String },
    CloseAllConnections,
    RenameProfile { id: String, name: String },
    TestProxy { name: String, url: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Snapshot,
    Command(Command),
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u32,
    pub id: u64,
    pub request: Request,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub id: u64,
    pub snapshot: Option<Snapshot>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<Diagnostic>,
}

pub fn uid() -> u32 {
    // getuid has no arguments and cannot fail.
    unsafe { libc::getuid() }
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        })
        .join("clash-oxide")
}

pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("CLASH_OXIDE_SOCKET") {
        return path.into();
    }
    if service_installed() || system_socket_path().exists() {
        return system_socket_path();
    }
    local_socket_path()
}

pub fn system_socket_path() -> PathBuf {
    PathBuf::from(format!("/run/clash-oxide-{}/control.sock", uid()))
}

pub fn service_installed() -> bool {
    std::path::Path::new(&format!("/etc/clash-oxide/{}.env", uid())).exists()
}

pub fn local_socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", uid())))
        .join("clash-oxide/control.sock")
}

pub fn bytes(value: u64) -> String {
    if value >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", value as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if value >= 1024 * 1024 {
        format!("{:.1} MiB", value as f64 / (1024.0 * 1024.0))
    } else if value >= 1024 {
        format!("{:.1} KiB", value as f64 / 1024.0)
    } else {
        format!("{value} B")
    }
}
