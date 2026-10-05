pub mod lifecycle;
pub mod view;

use anyhow::{Context, Result, bail};
use oxide_model::*;
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::Duration,
};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn request(path: &Path, request: Request) -> Result<Snapshot> {
    let stream = UnixStream::connect(path).with_context(|| {
        Diagnostic::new(
            "error.daemon_connect",
            format!("Cannot connect to daemon at {}", path.display()),
        )
        .arg("path", path.display())
    })?;
    let timeout = if matches!(request, Request::Snapshot) {
        2
    } else {
        120
    };
    exchange(stream, request, Duration::from_secs(timeout))
}

fn exchange(mut stream: UnixStream, request: Request, timeout: Duration) -> Result<Snapshot> {
    lifecycle::check_peer(&stream)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let data = serde_json::to_vec(&Envelope {
        version: PROTOCOL_VERSION,
        id,
        request,
    })?;
    if data.len() >= MAX_FRAME {
        bail!(Diagnostic::new(
            "error.frame_large",
            "Request exceeds size limit"
        ));
    }
    stream.write_all(&data)?;
    stream.write_all(b"\n")?;
    let mut data = Vec::new();
    BufReader::new(stream)
        .take((MAX_FRAME + 1) as u64)
        .read_until(b'\n', &mut data)?;
    if data.len() > MAX_FRAME || data.last() != Some(&b'\n') {
        bail!(Diagnostic::new(
            "error.invalid_frame",
            "Invalid daemon response frame"
        ));
    }
    let response: Response = serde_json::from_slice(&data)?;
    if response.version != PROTOCOL_VERSION || response.id != id {
        bail!(Diagnostic::new(
            "error.incompatible",
            "Incompatible daemon response"
        ));
    }
    if let Some(error) = response.error {
        return Err(response
            .diagnostic
            .unwrap_or_else(|| Diagnostic::new("error.operation", error))
            .into());
    }
    response.snapshot.context(Diagnostic::new(
        "error.no_state",
        "Daemon did not return state",
    ))
}

pub enum Update {
    State(Box<Snapshot>),
    Error(Diagnostic),
    Busy(bool),
    Connecting(String),
    Completed {
        command: Command,
        error: Option<Diagnostic>,
    },
}

/// Both frontends use this worker: no network I/O on either UI event loop.
/// Snapshot polling naturally resynchronizes after daemon restart.
pub fn connect(options: lifecycle::DaemonOptions) -> (SyncSender<Command>, Receiver<Update>) {
    let (commands, incoming) = mpsc::sync_channel(16);
    let (updates, events) = mpsc::sync_channel(4);
    std::thread::spawn(move || {
        let mut startup_error = lifecycle::ensure_running(&options, |message| {
            let _ = updates.send(Update::Connecting(message.into()));
        })
        .err()
        .map(|error| Diagnostic::from_error(&error));
        if let Some(error) = &startup_error
            && (updates.send(Update::Error(error.clone())).is_err()
                || updates.send(Update::Busy(false)).is_err())
        {
            return;
        }
        // Auto-start happens only once per UI launch. Polling never revives a killed daemon.
        loop {
            let command = match incoming.recv_timeout(Duration::from_secs(1)) {
                Ok(command) => Some(command),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if command.is_some() && updates.send(Update::Busy(true)).is_err() {
                break;
            }
            // Only an explicit reload may revive a daemon after `kill`.
            let result = (|| {
                if matches!(command, Some(Command::Reload)) {
                    lifecycle::ensure_running(&options, |message| {
                        let _ = updates.send(Update::Connecting(message.into()));
                    })?;
                    startup_error = None;
                }
                request(
                    &options.socket,
                    command
                        .clone()
                        .map(Request::Command)
                        .unwrap_or(Request::Snapshot),
                )
            })();
            let completion_error = result.as_ref().err().map(Diagnostic::from_error);
            let event = match result {
                Ok(state) => {
                    startup_error = None;
                    Update::State(Box::new(state))
                }
                Err(error) => Update::Error(
                    startup_error
                        .clone()
                        .unwrap_or_else(|| Diagnostic::from_error(&error)),
                ),
            };
            if updates.send(event).is_err() {
                break;
            }
            if let Some(command) = command
                && updates
                    .send(Update::Completed {
                        command,
                        error: completion_error,
                    })
                    .is_err()
            {
                break;
            }
            if updates.send(Update::Busy(false)).is_err() {
                break;
            }
        }
    });
    (commands, events)
}
