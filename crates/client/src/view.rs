//! Shared presentation state for the native GUI and terminal interface.
use oxide_model::{Connection, Snapshot};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, VecDeque};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Proxies,
    Profiles,
    Connections,
    Rules,
    Logs,
    Settings,
}
impl Page {
    pub const ALL: [Self; 6] = [
        Self::Proxies,
        Self::Profiles,
        Self::Connections,
        Self::Rules,
        Self::Logs,
        Self::Settings,
    ];
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap()
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Proxies => "nav.proxies",
            Self::Profiles => "nav.profiles",
            Self::Connections => "nav.connections",
            Self::Rules => "nav.rules",
            Self::Logs => "nav.logs",
            Self::Settings => "nav.settings",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sort {
    #[default]
    Original,
    Name,
    Delay,
}
impl Sort {
    pub fn next(self) -> Self {
        match self {
            Self::Original => Self::Name,
            Self::Name => Self::Delay,
            Self::Delay => Self::Original,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Original => "proxies.order_default",
            Self::Name => "proxies.order_name",
            Self::Delay => "proxies.order_delay",
        }
    }
}

#[derive(Default)]
pub struct Session {
    pub page: Page,
    pub expanded: BTreeSet<String>,
    pub queries: [String; 6],
    pub sort: Sort,
    pub connections_paused: bool,
    pub show_closed: bool,
    pub sort_download: bool,
    pub logs_paused: bool,
    pub log_level: usize,
    pub connections: Vec<Connection>,
    pub closed: Vec<Connection>,
    pub logs: Vec<String>,
    pub traffic: VecDeque<(u64, u64)>,
    previous_connections: Vec<Connection>,
    previous_logs: Vec<String>,
    live_closed: Vec<Connection>,
    live_logs: Vec<String>,
    revision: u64,
}
impl Session {
    pub fn with_preferences(preferences: &Preferences) -> Self {
        Self {
            sort: preferences.sort,
            expanded: preferences.expanded.clone(),
            ..Default::default()
        }
    }
    pub fn update(&mut self, state: &Snapshot) {
        if state.revision == self.revision {
            return;
        }
        self.revision = state.revision;
        if state.engine.connection_count <= state.engine.connections.len() {
            let present: BTreeSet<_> = state
                .engine
                .connections
                .iter()
                .map(|c| c.id.as_str())
                .collect();
            for connection in &self.previous_connections {
                if !present.contains(connection.id.as_str())
                    && !self.live_closed.iter().any(|c| c.id == connection.id)
                {
                    self.live_closed.insert(0, connection.clone());
                }
            }
            self.live_closed.truncate(200);
        }
        self.previous_connections = state.engine.connections.clone();
        if !self.connections_paused {
            self.connections = state.engine.connections.clone();
            self.closed = self.live_closed.clone();
        }
        let overlap = (0..=self.previous_logs.len().min(state.logs.len()))
            .rev()
            .find(|&n| self.previous_logs[self.previous_logs.len() - n..] == state.logs[..n])
            .unwrap_or(0);
        self.live_logs.extend_from_slice(&state.logs[overlap..]);
        if self.live_logs.len() > 500 {
            self.live_logs.drain(..self.live_logs.len() - 500);
        }
        self.previous_logs = state.logs.clone();
        if !self.logs_paused {
            self.logs = self.live_logs.clone();
        }
        self.traffic.push_back((
            state.engine.traffic.upload_rate,
            state.engine.traffic.download_rate,
        ));
        if self.traffic.len() > 60 {
            self.traffic.pop_front();
        }
    }
    pub fn toggle_group(&mut self, name: &str) {
        if !self.expanded.remove(name) {
            self.expanded.insert(name.into());
        }
    }
    pub fn clear_logs(&mut self) {
        self.live_logs.clear();
        self.logs.clear();
    }
    pub fn clear_closed(&mut self) {
        self.live_closed.clear();
        self.closed.clear();
    }
    pub fn query(&self) -> &str {
        &self.queries[self.page.index()]
    }
    pub fn filtered_connections(&self) -> Vec<&Connection> {
        let mut rows: Vec<_> = (if self.show_closed {
            &self.closed
        } else {
            &self.connections
        })
        .iter()
        .filter(|c| {
            matches(
                &self.queries[Page::Connections.index()],
                &[
                    &c.host,
                    &c.destination,
                    &c.source,
                    &c.chain,
                    &c.network,
                    &c.rule,
                ],
            )
        })
        .collect();
        if self.sort_download {
            rows.sort_by_key(|c| std::cmp::Reverse(c.download));
        } else {
            rows.sort_by(|a, b| b.started.cmp(&a.started).then(a.id.cmp(&b.id)));
        }
        rows
    }
    pub fn filtered_logs(&self) -> Vec<&String> {
        let threshold = [5, 0, 1, 2, 3][self.log_level.min(4)];
        self.logs
            .iter()
            .rev()
            .filter(|line| {
                let level = if line.starts_with("ERROR") {
                    0
                } else if line.starts_with("WARN") {
                    1
                } else if line.starts_with("INFO") {
                    2
                } else if line.starts_with("DEBUG") {
                    3
                } else {
                    4
                };
                level <= threshold && matches(&self.queries[Page::Logs.index()], &[line])
            })
            .collect()
    }
    pub fn members<'a>(
        &self,
        group: &'a oxide_model::ProxyGroup,
        state: &Snapshot,
    ) -> Vec<&'a String> {
        let mut members: Vec<_> = group
            .members
            .iter()
            .filter(|name| matches(&self.queries[Page::Proxies.index()], &[&group.name, name]))
            .collect();
        match self.sort {
            Sort::Original => {}
            Sort::Name => members.sort_by_key(|s| s.to_lowercase()),
            Sort::Delay => members.sort_by_key(|s| {
                state
                    .engine
                    .proxies
                    .get(s.as_str())
                    .and_then(|p| p.delay)
                    .filter(|d| *d > 0)
                    .unwrap_or(u64::MAX)
            }),
        }
        members
    }
}
pub fn matches(query: &str, values: &[&str]) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || values
            .iter()
            .any(|value| value.to_lowercase().contains(&query))
}
/// Kept separate from language preferences so GUI and TUI never overwrite locale settings.
#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub test_url: String,
    pub sort: Sort,
    pub expanded: BTreeSet<String>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            test_url: DEFAULT_TEST_URL.into(),
            sort: Sort::Original,
            expanded: BTreeSet::new(),
        }
    }
}
impl Preferences {
    fn path() -> std::path::PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join(".config")
            })
            .join("clash-oxide/view.json")
    }
    pub fn load() -> anyhow::Result<Self> {
        match std::fs::read(Self::path()) {
            Ok(data) => Ok(serde_json::from_slice(&data)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (self.test_url.starts_with("https://") || self.test_url.starts_with("http://"))
                && self.test_url.len() <= 2048,
            oxide_model::Diagnostic::new("error.test_url", "Enter an HTTP or HTTPS test URL")
        );
        use std::io::Write;
        let path = Self::path();
        let parent = path.parent().unwrap();
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
}

pub const DEFAULT_TEST_URL: &str = "https://www.gstatic.com/generate_204";
pub const LOG_LEVELS: [&str; 5] = [
    "common.all",
    "logs.error",
    "logs.warn",
    "logs.info",
    "logs.debug",
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pause_clear_and_history_follow_real_snapshots() {
        let mut view = Session::default();
        let mut state = Snapshot {
            revision: 1,
            logs: vec!["INFO first".into()],
            ..Default::default()
        };
        state.engine.connections = vec![Connection {
            id: "one".into(),
            host: "例子.TEST".into(),
            download: 12,
            ..Default::default()
        }];
        state.engine.connection_count = 1;
        view.update(&state);
        view.connections_paused = true;
        view.logs_paused = true;
        state.revision += 1;
        state.engine.connections.clear();
        state.engine.connection_count = 0;
        state.logs.push("WARN second".into());
        view.update(&state);
        assert_eq!(view.connections.len(), 1);
        assert_eq!(view.logs.len(), 1);
        view.clear_logs();
        view.connections_paused = false;
        view.logs_paused = false;
        state.revision += 1;
        state.logs.push("ERROR third".into());
        view.update(&state);
        assert!(view.connections.is_empty());
        assert_eq!(view.closed[0].download, 12);
        assert_eq!(view.logs, ["ERROR third"]);
        view.show_closed = true;
        view.queries[2] = "test".into();
        assert_eq!(view.filtered_connections().len(), 1);
        view.clear_closed();
        assert!(view.filtered_connections().is_empty());
    }
    #[test]
    fn truncated_connection_samples_do_not_imply_closed_connections() {
        let mut view = Session::default();
        let mut state = Snapshot {
            revision: 1,
            ..Default::default()
        };
        state.engine.connections.push(Connection {
            id: "old".into(),
            ..Default::default()
        });
        view.update(&state);
        state.revision += 1;
        state.engine.connections.clear();
        state.engine.connection_count = 501;
        view.update(&state);
        assert!(view.closed.is_empty());
    }
}
