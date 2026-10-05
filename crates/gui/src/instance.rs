use anyhow::{Context, Result, bail};
use oxide_i18n::Language;
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    os::{
        fd::AsRawFd,
        linux::net::SocketAddrExt,
        unix::{
            ffi::OsStrExt,
            net::{SocketAddr, UnixListener, UnixStream},
        },
    },
    path::{Component, Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

pub struct Activation {
    pub stream: UnixStream,
    pub language: Option<Language>,
}

/// A Linux abstract socket has no stale filesystem entry after a crash. Scope it
/// to the user, display session and daemon endpoint, allowing isolated instances.
pub fn acquire(endpoint: &Path) -> Result<Option<Receiver<Activation>>> {
    let mut path = PathBuf::new();
    for component in endpoint.components() {
        if component == Component::ParentDir {
            path.pop();
        } else {
            path.push(component);
        }
    }
    let display = std::env::var_os("WAYLAND_DISPLAY")
        .or_else(|| std::env::var_os("DISPLAY"))
        .unwrap_or_default();
    let mut hash = Sha256::new();
    hash.update(path.as_os_str().as_bytes());
    hash.update([0]);
    hash.update(display.as_bytes());
    let address = SocketAddr::from_abstract_name(format!(
        "clash-oxide-gui-{}-{:x}",
        oxide_model::uid(),
        hash.finalize()
    ))?;
    let listener = match UnixListener::bind_addr(&address) {
        Ok(listener) => listener,
        Err(error) if error.kind() == ErrorKind::AddrInUse => {
            let mut stream = UnixStream::connect_addr(&address)
                .context("The desktop instance is exiting; try again shortly")?;
            check_peer(&stream)?;
            stream.set_read_timeout(Some(Duration::from_secs(30)))?;
            stream.set_write_timeout(Some(Duration::from_secs(2)))?;
            if let Some(language) = oxide_i18n::launch_override() {
                writeln!(stream, "activate {}", language.id())?;
            } else {
                stream.write_all(b"activate\n")?;
            }
            let mut reply = [0; 3];
            stream
                .read_exact(&mut reply)
                .context("The desktop instance did not respond")?;
            if &reply != b"ok\n" {
                bail!("Could not activate the desktop window");
            }
            return Ok(None);
        }
        Err(error) => return Err(error).context("Could not reserve the desktop instance"),
    };
    let (send, receive) = mpsc::sync_channel(8);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let request = (|| -> Result<Option<Language>> {
                check_peer(&stream)?;
                stream.set_read_timeout(Some(Duration::from_secs(1)))?;
                stream.set_write_timeout(Some(Duration::from_secs(1)))?;
                let mut line = String::new();
                BufReader::new(&mut stream).take(64).read_line(&mut line)?;
                if line == "activate\n" {
                    return Ok(None);
                }
                if let Some(language) = line
                    .strip_prefix("activate ")
                    .and_then(|s| s.strip_suffix('\n'))
                    .and_then(Language::parse)
                {
                    return Ok(Some(language));
                }
                bail!("Invalid activation request")
            })();
            if let Ok(language) = request {
                let _ = send.try_send(Activation { stream, language });
            }
        }
    });
    Ok(Some(receive))
}

fn check_peer(stream: &UnixStream) -> Result<()> {
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&peer) as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if peer.uid != oxide_model::uid() {
        bail!("Desktop instance belongs to another user");
    }
    Ok(())
}
