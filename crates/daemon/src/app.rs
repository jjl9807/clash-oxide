use crate::{
    Logs, Pending, View, platform,
    store::{self, Store, StoredProfile},
};
use anyhow::{Context, Result, bail};
use oxide_engine_clash::{Engine, config};
use oxide_model::*;
use std::{path::PathBuf, time::Duration};
use tokio::sync::{mpsc, oneshot};

// Used only before a user configuration has been selected. Never substitute
// direct routing for a saved configuration that fails to load.
const DEFAULT_PROFILE: &str = "proxies: []\nproxy-groups: []\nrules: ['MATCH,DIRECT']\n";

pub struct App {
    store: Store,
    directory: PathBuf,
    engine: Option<Engine>,
    snapshot: Snapshot,
    view: View,
    logs: Logs,
}

impl App {
    pub fn new(store: Store, directory: PathBuf, view: View, logs: Logs) -> Self {
        Self {
            store,
            directory,
            engine: None,
            snapshot: Snapshot::default(),
            view,
            logs,
        }
    }
    fn publish(&mut self) {
        self.snapshot.settings = self.store.settings.clone();
        self.snapshot.profiles = self
            .store
            .profiles
            .iter()
            .map(|p| p.public.clone())
            .collect();
        self.snapshot.active_profile = self.store.active.clone();
        self.snapshot.logs = self.logs.lock().unwrap().iter().cloned().collect();
        self.snapshot.revision += 1;
        *self.view.write().unwrap() = self.snapshot.clone();
    }
    pub async fn run(
        mut self,
        mut commands: mpsc::Receiver<Pending>,
        mut stop: oneshot::Receiver<()>,
    ) -> Result<()> {
        self.snapshot.phase = Phase::Starting;
        self.publish();
        if let Err(error) = self.replace_runtime(self.store.clone(), None).await {
            self.snapshot.phase = Phase::Failed;
            self.snapshot.last_error = Some(format!("{error:#}"));
            self.snapshot.last_diagnostic = Some(Diagnostic::from_error(&error));
            self.publish();
        }
        self.publish();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = &mut stop => break,
                _ = tick.tick() => {
                    if let Some(engine) = &self.engine {
                        match engine.snapshot().await {
                            Ok(snapshot) => self.snapshot.engine = snapshot,
                            Err(error) => {
                                self.snapshot.last_error = Some(format!("Engine failed: {error:#}"));
                                self.snapshot.last_diagnostic = Some(Diagnostic::new("error.engine", format!("Engine failed: {error:#}")));
                                let _ = self.stop_runtime().await;
                                self.snapshot.phase = Phase::Failed;
                            }
                        }
                    }
                    self.publish();
                }
                Some((command, reply)) = commands.recv() => {
                    let result = self.handle(command).await;
                    if let Err(error) = &result {
                        tracing::warn!(error = %error, "Operation failed");
                        self.snapshot.last_error = Some(format!("{error:#}"));
                        self.snapshot.last_diagnostic = Some(Diagnostic::from_error(error));
                    } else { self.snapshot.last_error = None; self.snapshot.last_diagnostic = None; }
                    if let Some(engine) = &self.engine
                        && let Ok(snapshot) = engine.snapshot().await { self.snapshot.engine = snapshot; }
                    self.publish();
                    let _ = reply.send(result.map(|_| self.snapshot.clone()).map_err(|e| Diagnostic::from_error(&e)));
                }
            }
        }
        // Only daemon shutdown stops the core permanently.
        let result = self.stop_runtime().await;
        self.publish();
        result
    }

    fn prepared(&self, candidate: &Store, raw: Option<&str>) -> Result<config::Prepared> {
        let Some(id) = candidate.active.as_deref() else {
            let resources = self.directory.join("default");
            store::private_dir(&resources)?;
            return config::prepare(DEFAULT_PROFILE, &candidate.settings, &resources, None);
        };
        let profile = candidate.profile(id)?;
        let saved;
        let raw = match raw {
            Some(raw) => raw,
            None => {
                saved = std::fs::read_to_string(store::profile_path(&self.directory, id)?)?;
                &saved
            }
        };
        let resources = self.directory.join("profiles").join(id);
        store::private_dir(&resources)?;
        config::prepare(
            raw,
            &candidate.settings,
            &resources,
            profile.local_base.as_deref(),
        )
    }
    async fn start_runtime(&mut self, candidate: &Store, prepared: config::Prepared) -> Result<()> {
        if candidate.settings.tun {
            platform::preflight_tun()?;
        }
        self.snapshot.warnings = prepared.warnings.clone();
        let directory = candidate.active.as_ref().map_or_else(
            || self.directory.join("default"),
            |id| self.directory.join("profiles").join(id),
        );
        let engine = Engine::start(prepared, directory).await?;
        self.engine = Some(engine);
        let engine = self.engine.as_ref().unwrap();
        let groups = engine.snapshot().await?.groups;
        let selections = candidate
            .active
            .as_ref()
            .map(|id| candidate.profile(id).map(|profile| &profile.selections))
            .transpose()?;
        for (name, selected) in selections.into_iter().flatten() {
            if groups
                .iter()
                .any(|g| g.name == *name && g.selectable && g.members.contains(selected))
            {
                engine.select(name, selected).await?;
            }
        }
        let integration = (|| -> Result<()> {
            if candidate.settings.tun {
                platform::enable_tun_routes(&self.directory)?;
                self.snapshot.tun_active = true;
            }
            if candidate.settings.system_proxy {
                platform::enable_system_proxy(&self.directory, candidate.settings.mixed_port)?;
                self.snapshot.system_proxy_active = true;
            }
            Ok(())
        })();
        if let Err(error) = integration {
            let cleanup = self.stop_runtime().await;
            return Err(error.context(format!("Platform setup failed; rollback: {cleanup:?}")));
        }
        self.snapshot.engine = self.engine.as_ref().unwrap().snapshot().await?;
        self.snapshot.phase = Phase::Running;
        Ok(())
    }
    async fn stop_runtime(&mut self) -> Result<()> {
        self.snapshot.phase = Phase::Stopping;
        self.publish();
        let routes = platform::cleanup_tun_routes(&self.directory);
        let proxy = platform::restore_system_proxy(&self.directory);
        let stopped = match self.engine.take() {
            Some(engine) => engine.stop().await,
            None => Ok(()),
        };
        self.snapshot.tun_active = routes.is_err();
        self.snapshot.system_proxy_active = proxy.is_err();
        self.snapshot.engine = EngineSnapshot::default();
        self.snapshot.phase = if routes.is_ok() && proxy.is_ok() && stopped.is_ok() {
            Phase::Stopped
        } else {
            Phase::Failed
        };
        routes?;
        proxy?;
        stopped?;
        Ok(())
    }
    async fn replace_runtime(&mut self, candidate: Store, raw: Option<&str>) -> Result<()> {
        let prepared = self.prepared(&candidate, raw)?;
        let previous = self.store.clone();
        let was_running = self.engine.is_some();
        let old_prepared = if was_running {
            Some(self.prepared(&previous, None)?)
        } else {
            None
        };
        self.snapshot.phase = if was_running {
            Phase::Reloading
        } else {
            Phase::Starting
        };
        self.publish();
        if was_running {
            self.stop_runtime().await?;
        }
        let outcome = self.start_runtime(&candidate, prepared).await;
        let outcome = outcome.and_then(|_| candidate.save(&self.directory));
        if let Err(error) = outcome {
            let _ = self.stop_runtime().await;
            if let Some(old) = old_prepared {
                if let Err(rollback) = self.start_runtime(&previous, old).await {
                    self.snapshot.phase = Phase::Failed;
                    return Err(error.context(format!(
                        "Previous configuration could not be restored: {rollback:#}"
                    )));
                }
            } else {
                self.snapshot.phase = Phase::Failed;
            }
            return Err(error);
        }
        self.store = candidate;
        Ok(())
    }
    async fn apply_store(&mut self, candidate: Store) -> Result<()> {
        self.replace_runtime(candidate, None).await
    }
    async fn handle(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Reload => {
                self.replace_runtime(self.store.clone(), None).await?;
            }
            Command::Import { name, source } => {
                if self.store.profiles.len() >= 100 {
                    bail!(Diagnostic::new(
                        "error.profile_limit",
                        "The MVP supports up to 100 profiles"
                    ));
                }
                let source = source.trim();
                if source.is_empty() {
                    bail!(Diagnostic::new(
                        "error.source_empty",
                        "Enter a local file path or subscription URL"
                    ));
                }
                let (raw, local_base) = store::read_source(
                    source,
                    self.engine.as_ref().map(|_| self.store.settings.mixed_port),
                )
                .await?;
                let id = uuid::Uuid::new_v4().to_string();
                let resources = self.directory.join("profiles").join(&id);
                store::private_dir(&resources)?;
                let prepared = config::prepare(
                    &raw,
                    &self.store.settings,
                    &resources,
                    local_base.as_deref(),
                )?;
                let name = if name.trim().is_empty() {
                    format!("Profile {}", self.store.profiles.len() + 1)
                } else {
                    name.trim().chars().take(120).collect()
                };
                let source = if local_base.is_some() {
                    std::path::Path::new(source)
                        .canonicalize()?
                        .to_string_lossy()
                        .into_owned()
                } else {
                    source.into()
                };
                let profile = StoredProfile {
                    public: Profile {
                        id: id.clone(),
                        name,
                        subscription: local_base.is_none(),
                        updated_at: now(),
                    },
                    source,
                    local_base,
                    selections: Default::default(),
                };
                store::atomic_write(&store::profile_path(&self.directory, &id)?, raw.as_bytes())?;
                let mut candidate = self.store.clone();
                candidate.profiles.push(profile);
                if candidate.active.is_none() {
                    candidate.active = Some(id.clone());
                    if let Err(error) = self.apply_store(candidate).await {
                        // This import never committed. Keep the original source
                        // untouched and remove only the new private copy/cache.
                        let _ = std::fs::remove_file(store::profile_path(&self.directory, &id)?);
                        let _ = std::fs::remove_dir_all(&resources);
                        return Err(error);
                    }
                } else {
                    candidate.save(&self.directory)?;
                    self.store = candidate;
                    self.snapshot.warnings = prepared.warnings;
                }
                tracing::info!("Profile imported");
            }
            Command::Refresh { id } => {
                let profile = self.store.profile(&id)?.clone();
                let (raw, _) = store::read_source(
                    &profile.source,
                    self.engine.as_ref().map(|_| self.store.settings.mixed_port),
                )
                .await?;
                config::prepare(
                    &raw,
                    &self.store.settings,
                    &self.directory.join("profiles").join(&id),
                    profile.local_base.as_deref(),
                )?;
                let active = self.store.active.as_ref() == Some(&id);
                if active {
                    self.replace_runtime(self.store.clone(), Some(&raw)).await?;
                }
                if let Err(error) =
                    store::atomic_write(&store::profile_path(&self.directory, &id)?, raw.as_bytes())
                {
                    if active {
                        self.replace_runtime(self.store.clone(), None).await
                            .context("Failed to persist the refreshed profile and restore the saved runtime")?;
                    }
                    return Err(error.context("Refreshed profile could not be saved"));
                }
                let mut candidate = self.store.clone();
                candidate
                    .profiles
                    .iter_mut()
                    .find(|p| p.public.id == id)
                    .unwrap()
                    .public
                    .updated_at = now();
                candidate.save(&self.directory)?;
                self.store = candidate;
            }
            Command::RenameProfile { id, name } => {
                let name = name.trim();
                if name.is_empty() || name.chars().count() > 120 {
                    bail!(Diagnostic::new(
                        "error.profile_name",
                        "Use a name between 1 and 120 characters"
                    ));
                }
                self.store.profile(&id)?;
                let mut candidate = self.store.clone();
                candidate
                    .profiles
                    .iter_mut()
                    .find(|p| p.public.id == id)
                    .unwrap()
                    .public
                    .name = name.into();
                candidate.save(&self.directory)?;
                self.store = candidate;
            }
            Command::TestProxy { name, url } => {
                self.engine
                    .as_mut()
                    .context(Diagnostic::new("error.proxy_stopped", "Proxy is stopped"))?
                    .test_proxy(name, url)
                    .await?;
            }
            Command::CloseAllConnections => {
                self.engine
                    .as_ref()
                    .context(Diagnostic::new("error.proxy_stopped", "Proxy is stopped"))?
                    .close_all()
                    .await;
            }
            Command::SwitchProfile { id } => {
                self.store.profile(&id)?;
                let mut candidate = self.store.clone();
                candidate.active = Some(id);
                self.apply_store(candidate).await?;
            }
            Command::RemoveProfile { id } => {
                if self.store.active.as_ref() == Some(&id) {
                    bail!(Diagnostic::new(
                        "error.remove_active",
                        "Select another profile before removing the active profile"
                    ));
                }
                self.store.profile(&id)?;
                let mut candidate = self.store.clone();
                candidate.profiles.retain(|p| p.public.id != id);
                candidate.save(&self.directory)?;
                self.store = candidate;
                std::fs::remove_file(store::profile_path(&self.directory, &id)?)?;
            }
            Command::SelectProxy { group, proxy } => {
                let engine = self
                    .engine
                    .as_ref()
                    .context(Diagnostic::new("error.proxy_stopped", "Proxy is stopped"))?;
                let previous = engine
                    .snapshot()
                    .await?
                    .groups
                    .into_iter()
                    .find(|g| g.name == group)
                    .map(|g| g.selected);
                engine.select(&group, &proxy).await?;
                let mut candidate = self.store.clone();
                let Some(id) = candidate.active.as_ref() else {
                    // The built-in configuration has no user profile to save.
                    return Ok(());
                };
                candidate
                    .profiles
                    .iter_mut()
                    .find(|p| &p.public.id == id)
                    .context(Diagnostic::new(
                        "error.profile_missing",
                        "Profile not found",
                    ))?
                    .selections
                    .insert(group.clone(), proxy);
                if let Err(error) = candidate.save(&self.directory) {
                    if let Some(previous) = previous {
                        engine.select(&group, &previous).await?;
                    }
                    return Err(error);
                }
                self.store = candidate;
            }
            Command::CloseConnection { id } => {
                self.engine
                    .as_ref()
                    .context(Diagnostic::new("error.proxy_stopped", "Proxy is stopped"))?
                    .close(&id)
                    .await?;
            }
            Command::SetTun(value) => {
                let mut candidate = self.store.clone();
                candidate.settings.tun = value;
                self.apply_store(candidate).await?;
            }
            Command::SetSystemProxy(value) => {
                let mut candidate = self.store.clone();
                candidate.settings.system_proxy = value;
                self.apply_store(candidate).await?;
            }
            Command::SetMode(mode) => {
                if !["rule", "global", "direct"].contains(&mode.as_str()) {
                    bail!(Diagnostic::new("error.mode", "Unknown routing mode"));
                }
                let mut candidate = self.store.clone();
                candidate.settings.mode = mode;
                self.apply_store(candidate).await?;
            }
            Command::SetMixedPort(port) => {
                if port == 0 {
                    bail!(Diagnostic::new(
                        "error.port",
                        "Mixed port must be between 1 and 65535"
                    ));
                }
                let mut candidate = self.store.clone();
                candidate.settings.mixed_port = port;
                self.apply_store(candidate).await?;
            }
        }
        Ok(())
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
