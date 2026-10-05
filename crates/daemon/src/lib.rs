mod app;
mod platform;
mod store;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use fs2::FileExt;
use oxide_model::*;
use std::{
    collections::VecDeque,
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot},
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub struct Options {
    pub socket: PathBuf,
    pub data_dir: PathBuf,
    pub command: Option<Action>,
}
#[derive(Subcommand)]
pub enum Action {
    /// Validate YAML without starting listeners or changing the network.
    CheckConfig { file: PathBuf },
    /// Recover application-owned network settings after an abnormal exit.
    Cleanup,
}
type Logs = Arc<Mutex<VecDeque<String>>>;
type View = Arc<RwLock<Snapshot>>;
type Pending = (Command, oneshot::Sender<Result<Snapshot, Diagnostic>>);
type Shutdown = (
    oneshot::Sender<Result<Snapshot, Diagnostic>>,
    oneshot::Receiver<()>,
);

struct LogLayer(Logs);
impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for LogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Visitor(String);
        impl tracing::field::Visit for Visitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                if self.0.len() < 2048 {
                    let _ = write!(self.0, " {}={value:?}", field.name());
                }
            }
        }
        let mut visitor = Visitor(event.metadata().level().to_string());
        event.record(&mut visitor);
        let line: String = visitor.0.chars().take(2048).collect();
        let mut logs = self.0.lock().unwrap();
        if logs.len() >= 200 {
            logs.pop_front();
        }
        logs.push_back(line);
    }
}

#[tokio::main]
pub async fn run(args: Options) -> Result<()> {
    let logs = Logs::default();
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(LogLayer(logs.clone()))
        .init();
    if let Some(Action::CheckConfig { ref file }) = args.command {
        let file = file.canonicalize()?;
        let prepared = oxide_engine_clash::config::prepare(
            &std::fs::read_to_string(&file)?,
            &Settings::default(),
            &args.data_dir,
            file.parent(),
        )?;
        for warning in prepared.warnings {
            println!("Warning: {warning}");
        }
        println!("Configuration is valid");
        return Ok(());
    }
    store::private_dir(&args.data_dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(args.data_dir.join("daemon.lock"))?;
    lock.try_lock_exclusive()
        .context("Another daemon is already using this data directory")?;
    platform::cleanup_tun_routes(&args.data_dir)?;
    platform::restore_system_proxy(&args.data_dir)?;
    if matches!(args.command, Some(Action::Cleanup)) {
        return Ok(());
    }
    let state = store::Store::load(&args.data_dir)?;
    let parent = args
        .socket
        .parent()
        .context("Socket needs a parent directory")?;
    if !parent.exists() {
        store::private_dir(parent)?;
    }
    let metadata = parent.symlink_metadata()?;
    if !metadata.is_dir() || metadata.uid() != uid() || metadata.mode() & 0o077 != 0 {
        bail!(
            "Socket directory must be owned by the daemon user with mode 0700: {}",
            parent.display()
        );
    }
    if args.socket.exists() {
        if !args.socket.symlink_metadata()?.file_type().is_socket() {
            bail!("Refusing to replace a non-socket file");
        }
        if UnixStream::connect(&args.socket).await.is_ok() {
            bail!("A daemon is already listening on this socket");
        }
        std::fs::remove_file(&args.socket)?;
    }
    let listener = UnixListener::bind(&args.socket)?;
    std::fs::set_permissions(&args.socket, std::fs::Permissions::from_mode(0o600))?;
    let view = View::default();
    let (commands, receiver) = mpsc::channel(32);
    let (shutdown, mut shutdown_requests) = mpsc::channel::<Shutdown>(1);
    let (stop, stop_receiver) = oneshot::channel();
    let app = app::App::new(state, args.data_dir, view.clone(), logs);
    let mut task = tokio::spawn(app.run(receiver, stop_receiver));
    tracing::info!(socket = %args.socket.display(), "Daemon ready");
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let limit = Arc::new(tokio::sync::Semaphore::new(16));
    let mut app_result = None;
    let mut shutdown_reply = None;
    loop {
        tokio::select! {
            result = &mut task => { app_result = Some(result); break; },
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
            Some(reply) = shutdown_requests.recv() => { shutdown_reply = Some(reply); break; },
            result = listener.accept() => {
                let (stream, _) = result?;
                if stream.peer_cred()?.uid() != uid() { continue; }
                let Ok(permit) = limit.clone().try_acquire_owned() else { continue; };
                let (commands, view, shutdown) = (commands.clone(), view.clone(), shutdown.clone());
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = serve(stream, commands, view, shutdown).await { tracing::debug!(%error, "IPC client disconnected"); }
                });
            }
        }
    }
    let _ = stop.send(());
    let result = match app_result {
        Some(result) => result,
        None => task.await,
    };
    drop(listener);
    let result = result
        .context("Daemon task failed")
        .and_then(|result| result)
        .and(std::fs::remove_file(args.socket).map_err(Into::into));
    if let Some((reply, acknowledged)) = shutdown_reply {
        let response = result
            .as_ref()
            .map(|_| view.read().unwrap().clone())
            .map_err(Diagnostic::from_error);
        let _ = reply.send(response);
        // Give the IPC task time to flush the shutdown result before its runtime ends.
        let _ = tokio::time::timeout(Duration::from_secs(10), acknowledged).await;
    }
    result
}

async fn serve(
    stream: UnixStream,
    commands: mpsc::Sender<Pending>,
    view: View,
    shutdown: mpsc::Sender<Shutdown>,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut data = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        BufReader::new(read)
            .take((MAX_FRAME + 1) as u64)
            .read_until(b'\n', &mut data),
    )
    .await??;
    if data.len() > MAX_FRAME || data.last() != Some(&b'\n') {
        bail!("Invalid IPC frame");
    }
    let envelope: Envelope = serde_json::from_slice(&data)?;
    let mut shutdown_ack = None;
    let result = if envelope.version != PROTOCOL_VERSION {
        Err(Diagnostic::new(
            "error.protocol",
            "Protocol version mismatch; update client and daemon together",
        ))
    } else {
        match envelope.request {
            Request::Snapshot => Ok(view.read().unwrap().clone()),
            Request::Shutdown => {
                let (reply, receive) = oneshot::channel();
                let (ack, acknowledged) = oneshot::channel();
                if shutdown.try_send((reply, acknowledged)).is_err() {
                    Err(Diagnostic::new(
                        "error.shutting_down",
                        "Daemon is already shutting down",
                    ))
                } else {
                    shutdown_ack = Some(ack);
                    receive.await.unwrap_or_else(|_| {
                        Err(Diagnostic::new(
                            "error.daemon_shutdown",
                            "Daemon shutdown failed",
                        ))
                    })
                }
            }
            Request::Command(command) => {
                let (reply, receive) = oneshot::channel();
                if commands.try_send((command, reply)).is_err() {
                    Err(Diagnostic::new(
                        "error.daemon_busy",
                        "Daemon is busy; retry shortly",
                    ))
                } else {
                    match tokio::time::timeout(Duration::from_secs(115), receive).await {
                        Ok(Ok(result)) => result,
                        _ => Err(Diagnostic::new(
                            "error.command_timeout",
                            "Operation timed out; reconnect to inspect its current state",
                        )),
                    }
                }
            }
        }
    };
    let response = match result {
        Ok(snapshot) => Response {
            version: PROTOCOL_VERSION,
            id: envelope.id,
            snapshot: Some(snapshot),
            error: None,
            diagnostic: None,
        },
        Err(error) => Response {
            version: PROTOCOL_VERSION,
            id: envelope.id,
            snapshot: None,
            error: Some(error.detail.clone()),
            diagnostic: Some(error),
        },
    };
    let mut data = serde_json::to_vec(&response)?;
    if data.len() >= MAX_FRAME {
        bail!("Snapshot exceeded frame limit");
    }
    data.push(b'\n');
    tokio::time::timeout(Duration::from_secs(10), write.write_all(&data)).await??;
    if let Some(ack) = shutdown_ack {
        let _ = ack.send(());
    }
    Ok(())
}
