pub mod config;

use anyhow::{Context, Result, bail};
use clash_lib::RuntimeComponents;
use oxide_model::*;
use serde_json::Value;
use std::{path::PathBuf, sync::Arc, thread::JoinHandle, time::Duration};
use tokio::sync::oneshot;

/// A dedicated runtime confines upstream background tasks to the engine's
/// lifetime. The daemon and IPC stay alive during reloads and startup failures.
pub struct Engine {
    components: Arc<RuntimeComponents>,
    cache_path: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    probe: Option<(String, tokio::task::JoinHandle<()>)>,
}

impl Engine {
    pub async fn start(prepared: config::Prepared, directory: PathBuf) -> Result<Self> {
        let cache_path = directory.join("cache.db");
        let parsed = clash_lib::Config::Str(prepared.yaml).try_parse()?;
        let port = prepared.port;
        // Detect conflicts before spawning upstream listeners, whose startup
        // otherwise reports errors only through tracing.
        drop(
            std::net::TcpListener::bind(("127.0.0.1", port))
                .with_context(|| format!("Mixed inbound port {port} is already in use"))?,
        );
        let (ready_tx, ready_rx) = oneshot::channel();
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("oxide-engine".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(error) => {
                        let _ = ready_tx.send(Err(anyhow::Error::from(error)));
                        return;
                    }
                };
                rt.block_on(async {
                    clash_lib::setup_default_crypto_provider();
                    // The upstream factory initializes process-wide defaults only
                    // for TUN. Clear them when restarting in ordinary proxy mode.
                    // Engine lifetimes must not overlap; stop() joins the previous
                    // runtime thread before the daemon creates its replacement.
                    if !parsed.tun.enable {
                        *clash_lib::app::net::DEFAULT_OUTBOUND_INTERFACE
                            .write()
                            .await = None;
                        *clash_lib::app::net::TUN_SOMARK.write().await = None;
                    }
                    let created = tokio::select! {
                        result = clash_lib::create_components(directory, parsed) => result,
                        _ = &mut stop_rx => return,
                    };
                    let components = match created {
                        Ok(c) => Arc::new(c),
                        Err(error) => {
                            let _ = ready_tx.send(Err(anyhow::Error::from(error)));
                            return;
                        }
                    };
                    components.start_all();
                    let ready = tokio::time::timeout(Duration::from_secs(15), async {
                        components.tun_runner.wait_ready().await?;
                        // SOCKS negotiation also proves the mixed listener is serving.
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
                        loop {
                            if let Ok(mut socket) =
                                tokio::net::TcpStream::connect(("127.0.0.1", port)).await
                            {
                                socket.write_all(&[5, 1, 0]).await?;
                                let mut reply = [0; 2];
                                socket.read_exact(&mut reply).await?;
                                if reply == [5, 0] {
                                    break;
                                }
                                bail!("Mixed listener did not accept SOCKS5 negotiation");
                            }
                            tokio::time::sleep(Duration::from_millis(30)).await;
                        }
                        Ok::<_, anyhow::Error>(())
                    })
                    .await
                    .context("Engine startup timed out")
                    .and_then(|r| r);
                    match ready {
                        Ok(()) => {
                            if ready_tx.send(Ok(components.clone())).is_ok() {
                                let _ = stop_rx.await;
                            }
                        }
                        Err(error) => {
                            let _ = ready_tx.send(Err(error));
                        }
                    }
                    components.statistics_manager.close_all().await;
                    let _ =
                        tokio::time::timeout(Duration::from_secs(5), components.stop_all()).await;
                });
                // This drops the asynchronous TUN task/device too. The upstream
                // TUN join() is a no-op; thread.join() below is our lifetime boundary.
                rt.shutdown_timeout(Duration::from_secs(2));
            })?;
        match tokio::time::timeout(Duration::from_secs(90), ready_rx).await {
            Ok(Ok(Ok(components))) => Ok(Self {
                components,
                cache_path,
                stop: Some(stop_tx),
                thread: Some(thread),
                probe: None,
            }),
            result => {
                drop(stop_tx);
                // Also join a failed startup before rollback starts another
                // engine: upstream network defaults are process-wide.
                tokio::task::spawn_blocking(move || {
                    thread.join().map_err(|_| {
                        anyhow::anyhow!("Engine thread panicked during initialization")
                    })
                })
                .await??;
                match result {
                    Ok(Ok(Err(error))) => Err(error),
                    _ => bail!("Engine initialization failed or timed out"),
                }
            }
        }
    }

    pub async fn stop(mut self) -> Result<()> {
        if let Some((_, probe)) = self.probe.take() {
            probe.abort();
            let _ = probe.await;
        }
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            tokio::task::spawn_blocking(move || {
                thread
                    .join()
                    .map_err(|_| anyhow::anyhow!("Engine thread panicked"))
            })
            .await??;
        }
        // Upstream flushes every 10 seconds. A settings change must not lose
        // DNS fake-IP mappings still cached by applications. The upstream
        // writer is gone now, so atomically persist its final state.
        let yaml = self.components.cache_store.snapshot_yaml().await?;
        let temp = self
            .cache_path
            .with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            use std::{io::Write, os::unix::fs::OpenOptionsExt};
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(yaml.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temp, &self.cache_path)?;
            std::fs::File::open(self.cache_path.parent().unwrap())?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result.context("Could not preserve the DNS cache during shutdown")?;
        Ok(())
    }

    pub async fn select(&self, group: &str, proxy: &str) -> Result<()> {
        let control = self
            .components
            .outbound_manager
            .get_selector_control(group)
            .context("This proxy group cannot be selected manually")?;
        control.select(proxy).await?;
        self.components.cache_store.set_selected(group, proxy).await;
        Ok(())
    }

    pub async fn close(&self, id: &str) -> Result<()> {
        self.components.statistics_manager.close(id.parse()?).await;
        Ok(())
    }

    pub async fn close_all(&self) {
        self.components.statistics_manager.close_all().await;
    }

    /// Tests run in the background so selecting a node or stopping the proxy stays responsive.
    pub async fn test_proxy(&mut self, name: String, url: String) -> Result<()> {
        if self
            .probe
            .as_ref()
            .is_some_and(|(_, task)| !task.is_finished())
        {
            bail!(Diagnostic::new(
                "error.test_busy",
                "A latency test is already running"
            ));
        }
        if !(url.starts_with("https://") || url.starts_with("http://")) || url.len() > 2048 {
            bail!(Diagnostic::new(
                "error.test_url",
                "Enter an HTTP or HTTPS test URL"
            ));
        }
        let manager = self.components.outbound_manager.clone();
        let proxy = manager
            .get_outbound(&name)
            .await
            .context("Proxy not found")?;
        let members = if let Some(group) = proxy.try_as_group_handler() {
            group.get_proxies().await
        } else {
            vec![proxy]
        };
        let task = tokio::spawn(async move {
            if tokio::time::timeout(
                Duration::from_secs(60),
                manager.url_test(&members, &url, Duration::from_secs(5), true),
            )
            .await
            .is_err()
            {
                tracing::warn!(
                    "Latency test reached the 60 second limit; remaining nodes were not tested"
                );
            }
        });
        self.probe = Some((name, task));
        Ok(())
    }

    pub async fn snapshot(&self) -> Result<EngineSnapshot> {
        self.components.tun_runner.wait_ready().await?;
        let raw = serde_json::to_value(self.components.outbound_manager.get_proxies().await)?;
        let mut groups = Vec::new();
        let mut details = std::collections::BTreeMap::new();
        if let Some(proxies) = raw.as_object() {
            for (name, proxy) in proxies {
                details.insert(
                    name.clone(),
                    ProxyInfo {
                        kind: string(proxy, "type"),
                        delay: proxy["history"]
                            .as_array()
                            .and_then(|h| h.last())
                            .and_then(|h| h["delay"].as_u64()),
                        alive: proxy["alive"].as_bool().unwrap_or(false),
                    },
                );
                if let Some(members) = proxy.get("all").and_then(Value::as_array) {
                    groups.push(ProxyGroup {
                        name: name.clone(),
                        kind: string(proxy, "type"),
                        selected: string(proxy, "now"),
                        members: members
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect(),
                        selectable: self
                            .components
                            .outbound_manager
                            .get_selector_control(name)
                            .is_some(),
                    });
                }
            }
        }
        // The core returns a fresh HashMap for each snapshot. GUI dependencies
        // enable serde_json's preserve_order, so explicitly stabilize the order
        // instead of relying on JSON maps to sort the groups for us.
        groups.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        let raw = serde_json::to_value(self.components.statistics_manager.snapshot().await)?;
        let rows = raw
            .get("connections")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let connection_count = rows.len();
        let connections = rows
            .into_iter()
            .take(500)
            .map(|row| {
                let meta = &row["metadata"];
                Connection {
                    id: string(&row, "id"),
                    host: string(meta, "host"),
                    destination: format!(
                        "{}:{}",
                        string(meta, "destinationIP"),
                        meta["destinationPort"]
                    ),
                    source: format!("{}:{}", string(meta, "sourceIP"), meta["sourcePort"]),
                    started: string(&row, "start"),
                    rule: format!("{} {}", string(&row, "rule"), string(&row, "rulePayload"))
                        .trim()
                        .into(),
                    network: string(meta, "network"),
                    chain: row["chains"]
                        .as_array()
                        .map(|v| {
                            v.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(" → ")
                        })
                        .unwrap_or_default(),
                    upload: row["upload"].as_u64().unwrap_or(0),
                    download: row["download"].as_u64().unwrap_or(0),
                }
            })
            .collect();
        let (upload_rate, download_rate) = self.components.statistics_manager.now();
        let rules = self.components.router.get_all_rules();
        let rule_count = rules.len();
        let rules = rules
            .iter()
            .take(5000)
            .map(|rule| {
                let value = serde_json::to_value(rule.as_map())?;
                Ok(Rule {
                    kind: string(&value, "type"),
                    payload: string(&value, "payload"),
                    target: string(&value, "proxy"),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(EngineSnapshot {
            groups,
            proxies: details,
            rules,
            rule_count,
            testing: self
                .probe
                .as_ref()
                .filter(|(_, task)| !task.is_finished())
                .map(|(name, _)| name.clone()),
            connections,
            connection_count,
            traffic: Traffic {
                upload_rate,
                download_rate,
                upload_total: raw["uploadTotal"].as_u64().unwrap_or(0),
                download_total: raw["downloadTotal"].as_u64().unwrap_or(0),
            },
        })
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some((_, probe)) = self.probe.take() {
            probe.abort();
        }
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

fn string(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}
