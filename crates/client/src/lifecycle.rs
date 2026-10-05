use anyhow::{Context, Result, bail};
use fs2::FileExt;
use oxide_model::Diagnostic;
use oxide_model::{Request, Snapshot};
use std::{
    fs::{File, OpenOptions},
    io::{self, ErrorKind},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            fs::{MetadataExt, OpenOptionsExt},
            net::UnixStream,
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const START_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct DaemonOptions {
    pub socket: PathBuf,
    pub data_dir: PathBuf,
    pub service: bool,
}

impl DaemonOptions {
    pub fn resolve(socket: Option<PathBuf>, data_dir: Option<PathBuf>) -> Result<Self> {
        let socket = socket.or_else(|| std::env::var_os("CLASH_OXIDE_SOCKET").map(PathBuf::from));
        let custom = socket.is_some() || data_dir.is_some();
        let custom_data = data_dir.is_some();
        let data_dir = std::path::absolute(data_dir.unwrap_or_else(oxide_model::data_dir))?;
        let service = !custom && oxide_model::service_installed();
        let socket = socket.unwrap_or_else(|| {
            if custom_data {
                data_dir.join("control.sock")
            } else {
                oxide_model::socket_path()
            }
        });
        Ok(Self {
            socket: std::path::absolute(socket)?,
            data_dir,
            service,
        })
    }
}

pub(crate) fn check_peer(stream: &UnixStream) -> Result<libc::pid_t> {
    let mut peer = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SO_PEERCRED reports kernel-verified credentials for this connected socket.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error().into());
    }
    if peer.uid != oxide_model::uid() {
        bail!(Diagnostic::new(
            "error.peer",
            "Daemon socket belongs to another user"
        ));
    }
    Ok(peer.pid)
}

fn connect_if_running(path: &Path) -> Result<Option<UnixStream>> {
    match UnixStream::connect(path) {
        Ok(stream) => Ok(Some(stream)),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error).with_context(|| format!("Cannot connect to {}", path.display())),
    }
}

fn probe(path: &Path) -> Result<Option<Snapshot>> {
    connect_if_running(path)?
        .map(|stream| {
            super::exchange(stream, Request::Snapshot, Duration::from_secs(2)).context(
                Diagnostic::new(
                    "error.handshake",
                    "Daemon handshake failed; check its version and socket",
                ),
            )
        })
        .transpose()
}

fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let metadata = path.symlink_metadata()?;
    if !metadata.is_dir() || metadata.uid() != oxide_model::uid() || metadata.mode() & 0o077 != 0 {
        bail!(
            "Directory must be owned by this user with mode 0700: {}",
            path.display()
        );
    }
    Ok(())
}

fn private_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != oxide_model::uid() || metadata.mode() & 0o077 != 0 {
        bail!(
            "File must be owned by this user with mode 0600: {}",
            path.display()
        );
    }
    Ok(file)
}

fn startup_lock(options: &DaemonOptions) -> Result<File> {
    private_dir(&options.data_dir)?;
    let lock = private_file(&options.data_dir.join("launcher.lock"))?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => return Ok(lock),
            Err(error) if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                return Err(error).context(Diagnostic::new(
                    "error.start_lock",
                    "Another client is starting the daemon; try again shortly",
                ));
            }
        }
    }
}

fn reap(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

fn start_service(options: &DaemonOptions) -> Result<()> {
    let mut child = Command::new("systemctl")
        .args(["--no-ask-password", "--no-block", "start"])
        .arg(format!("clash-oxide@{}.service", oxide_model::uid()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context(Diagnostic::new(
            "error.systemctl",
            "Cannot start the installed service: systemctl is unavailable",
        ))?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!(
                    "Cannot start the installed service. Run `sudo systemctl start clash-oxide@{}.service`; reinstall to enable automatic startup. Expected socket: {}",
                    oxide_model::uid(),
                    options.socket.display()
                );
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            reap(child);
            bail!(Diagnostic::new(
                "error.systemctl_timeout",
                "systemctl timed out; inspect the service with journalctl"
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn spawn_daemon(options: &DaemonOptions) -> Result<Child> {
    let log_path = options.data_dir.join("daemon.log");
    let log = private_file(&log_path)?;
    if log.metadata()?.len() > 8 * 1024 * 1024 {
        std::fs::rename(&log_path, options.data_dir.join("daemon.log.1"))?;
    }
    let log = private_file(&log_path)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("daemon")
        .arg("--socket")
        .arg(&options.socket)
        .arg("--data-dir")
        .arg(&options.data_dir)
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // Detach from the terminal without running any non-async-signal-safe Rust in the child.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            libc::umask(0o077);
            Ok(())
        });
    }
    command.spawn().context(Diagnostic::new(
        "error.spawn",
        "Cannot launch `clash-oxide daemon`",
    ))
}

pub fn ensure_running(options: &DaemonOptions, mut progress: impl FnMut(&str)) -> Result<()> {
    progress("status.connecting");
    if probe(&options.socket)?.is_some() {
        return Ok(());
    }
    let _lock = startup_lock(options)?;
    if probe(&options.socket)?.is_some() {
        return Ok(());
    }
    let mut child = if options.service {
        progress("status.starting_service");
        start_service(options)?;
        None
    } else {
        progress("status.starting_daemon");
        Some(spawn_daemon(options)?)
    };
    let deadline = Instant::now() + START_TIMEOUT;
    let mut child_exited = false;
    let result = loop {
        if let Some(child) = child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    child_exited = true;
                    break Err(anyhow::anyhow!(
                        "Daemon exited ({status}). See {}",
                        options.data_dir.join("daemon.log").display()
                    ));
                }
                Ok(None) => {}
                Err(error) => break Err(error.into()),
            }
        }
        match probe(&options.socket) {
            Ok(Some(_)) => break Ok(()),
            Ok(None) => {}
            Err(error) => break Err(error),
        }
        if Instant::now() >= deadline {
            break Err(anyhow::anyhow!(
                "Daemon startup timed out. Check {} or the systemd journal",
                options.data_dir.join("daemon.log").display()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if let Some(child) = child {
        if result.is_err() && !child_exited {
            // This is our unreaped child, so its PID cannot have been reused.
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
            }
        }
        reap(child);
    }
    result
}

/// Returns only after the daemon has finished restoring routes and system proxy settings.
/// Shutdown is requested over authenticated IPC; no pidfiles or process-name matching.
pub fn shutdown(options: &DaemonOptions) -> Result<bool> {
    let Some(stream) = connect_if_running(&options.socket)? else {
        return Ok(false);
    };
    let pid = check_peer(&stream)?;
    // Pin this exact process while it is still serving IPC, without relying on a pidfile.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
    let process = if fd >= 0 {
        Some(unsafe { OwnedFd::from_raw_fd(fd) })
    } else {
        None
    };
    super::exchange(stream, Request::Shutdown, Duration::from_secs(150)).context(
        Diagnostic::new(
            "error.shutdown_failed",
            "Daemon shutdown failed; inspect its log before retrying",
        ),
    )?;
    if let Some(process) = process {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut descriptor = libc::pollfd {
                fd: process.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut descriptor, 1, 100) };
            if result > 0 {
                break;
            }
            if result < 0 && io::Error::last_os_error().kind() != ErrorKind::Interrupted {
                return Err(io::Error::last_os_error().into());
            }
            if Instant::now() >= deadline {
                bail!(Diagnostic::new(
                    "error.shutdown_timeout",
                    "Network cleanup completed, but the daemon has not exited yet"
                ));
            }
        }
    }
    Ok(true)
}
