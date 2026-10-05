use anyhow::{Context, Result, bail};
use oxide_model::{ROUTE_TABLE, TUN_DEVICE};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

const PROTOCOL: &str = "186";

fn ip(args: &[String]) -> Result<()> {
    let output = Command::new("ip")
        .args(args)
        .output()
        .context("Install iproute2 to use TUN")?;
    if !output.status.success() {
        bail!(
            "ip {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn route_operations() -> Vec<Vec<String>> {
    let mut operations = Vec::new();
    for family in ["-4", "-6"] {
        for args in [
            vec![
                "route", "add", "default", "dev", TUN_DEVICE, "table", "20260", "proto", PROTOCOL,
            ],
            vec![
                "rule", "add", "pref", "20258", "fwmark", "20260", "lookup", "main", "protocol",
                PROTOCOL,
            ],
            vec![
                "rule", "add", "pref", "20259", "dport", "53", "lookup", "20260", "protocol",
                PROTOCOL,
            ],
            vec![
                "rule",
                "add",
                "pref",
                "20260",
                "lookup",
                "main",
                "suppress_prefixlength",
                "0",
                "protocol",
                PROTOCOL,
            ],
            vec![
                "rule", "add", "pref", "20261", "lookup", "20260", "protocol", PROTOCOL,
            ],
        ] {
            operations.push(
                std::iter::once(family)
                    .chain(args)
                    .map(str::to_owned)
                    .collect(),
            );
        }
    }
    operations
}

pub fn preflight_tun() -> Result<()> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let capabilities = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:\t"))
        .and_then(|v| u64::from_str_radix(v.trim(), 16).ok())
        .unwrap_or(0);
    if capabilities & (1 << 12) == 0 {
        bail!(
            "TUN requires CAP_NET_ADMIN. Install the supplied systemd service, then reconnect to it."
        );
    }
    if !Path::new("/dev/net/tun").exists() {
        bail!("/dev/net/tun is unavailable");
    }
    if Path::new(&format!("/sys/class/net/{TUN_DEVICE}")).exists() {
        bail!("Network interface {TUN_DEVICE} already exists; refusing to take ownership");
    }
    for family in ["-4", "-6"] {
        let output = Command::new("ip")
            .args([family, "-j", "rule", "show"])
            .output()?;
        if !output.status.success() {
            bail!("Cannot inspect Linux policy routing");
        }
        let rules: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)?;
        if rules
            .iter()
            .any(|rule| (20258..=20261).contains(&rule["priority"].as_u64().unwrap_or(0)))
        {
            bail!("Policy priorities 20258–20261 are already in use");
        }
        let routes = Command::new("ip")
            .args([
                family,
                "-j",
                "route",
                "show",
                "table",
                &ROUTE_TABLE.to_string(),
            ])
            .output()?;
        if routes.status.success()
            && !serde_json::from_slice::<Vec<serde_json::Value>>(&routes.stdout)?.is_empty()
        {
            bail!("Routing table {ROUTE_TABLE} is already in use");
        }
    }
    Ok(())
}

pub fn enable_tun_routes(directory: &Path) -> Result<()> {
    // Write intent before the first mutation, allowing startup/ExecStopPost to
    // recover after SIGKILL. Only our exact rules with protocol 186 are removed.
    crate::store::atomic_write(&directory.join("routes.json"), b"{\"version\":1}")?;
    for operation in route_operations() {
        if let Err(error) = ip(&operation) {
            let cleanup = cleanup_tun_routes(directory);
            if let Err(cleanup) = cleanup {
                return Err(error.context(format!("Route rollback also failed: {cleanup:#}")));
            }
            return Err(error);
        }
    }
    Ok(())
}

pub fn cleanup_tun_routes(directory: &Path) -> Result<()> {
    let journal = directory.join("routes.json");
    if !journal.exists() {
        return Ok(());
    }
    let mut errors = Vec::new();
    for mut operation in route_operations().into_iter().rev() {
        operation[2] = "del".into();
        if let Err(error) = ip(&operation) {
            let text = error.to_string();
            if !text.contains("No such")
                && !text.contains("Cannot find device")
                && !text.contains("does not exist")
            {
                errors.push(text);
            }
        }
    }
    if !errors.is_empty() {
        bail!("Route cleanup incomplete: {}", errors.join("; "));
    }
    std::fs::remove_file(journal)?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct ProxyValue {
    schema: String,
    key: String,
    previous: String,
    applied: String,
}

fn gsettings(action: &str, schema: &str, key: &str, value: Option<&str>) -> Result<String> {
    let mut command = Command::new("gsettings");
    command.args([action, schema, key]);
    if let Some(value) = value {
        command.arg(value);
    }
    let output = command.output().context("System proxy integration requires GNOME-compatible GSettings; TUN works independently of the desktop")?;
    if !output.status.success() {
        bail!(
            "System proxy: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn enable_system_proxy(directory: &Path, port: u16) -> Result<()> {
    let path = directory.join("system-proxy.json");
    if path.exists() {
        restore_system_proxy(directory)?;
    }
    let mut entries = Vec::new();
    for scheme in ["http", "https", "socks"] {
        let schema = format!("org.gnome.system.proxy.{scheme}");
        for (key, value) in [("host", "'127.0.0.1'".into()), ("port", port.to_string())] {
            entries.push(ProxyValue {
                previous: gsettings("get", &schema, key, None)?,
                schema: schema.clone(),
                key: key.into(),
                applied: value,
            });
        }
    }
    for (key, value) in [("use-same-proxy", "false"), ("mode", "'manual'")] {
        entries.push(ProxyValue {
            schema: "org.gnome.system.proxy".into(),
            key: key.into(),
            previous: gsettings("get", "org.gnome.system.proxy", key, None)?,
            applied: value.into(),
        });
    }
    crate::store::atomic_write(&path, &serde_json::to_vec(&entries)?)?;
    for entry in &entries {
        if let Err(error) = gsettings("set", &entry.schema, &entry.key, Some(&entry.applied)) {
            // Roll back only fields which were actually changed by this attempt.
            for entry in entries.iter().rev() {
                if gsettings("get", &entry.schema, &entry.key, None)
                    .ok()
                    .as_deref()
                    == Some(&entry.applied)
                {
                    let _ = gsettings("set", &entry.schema, &entry.key, Some(&entry.previous));
                }
            }
            return Err(error);
        }
    }
    // GSettings can emit dconf warnings and exit 0 without applying anything.
    for entry in &entries {
        if gsettings("get", &entry.schema, &entry.key, None)? != entry.applied {
            restore_system_proxy(directory)?;
            bail!(
                "The desktop settings service did not apply the proxy; a user D-Bus session is required"
            );
        }
    }
    Ok(())
}

pub fn restore_system_proxy(directory: &Path) -> Result<()> {
    let path: PathBuf = directory.join("system-proxy.json");
    if !path.exists() {
        return Ok(());
    }
    let entries: Vec<ProxyValue> = serde_json::from_slice(&std::fs::read(&path)?)?;
    let mut still_owned = true;
    for entry in &entries {
        let current = gsettings("get", &entry.schema, &entry.key, None)?;
        if current != entry.applied && current != entry.previous {
            still_owned = false;
        }
    }
    if still_owned {
        for entry in entries.iter().rev() {
            gsettings("set", &entry.schema, &entry.key, Some(&entry.previous))?;
        }
        for entry in &entries {
            if gsettings("get", &entry.schema, &entry.key, None)? != entry.previous {
                bail!(
                    "The desktop settings service did not restore the proxy; recovery information has been retained"
                );
            }
        }
    } else {
        tracing::warn!("Desktop proxy was changed externally; retaining those settings");
    }
    std::fs::remove_file(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn network_operations_are_scoped_and_mark_bypass_precedes_capture() {
        let operations = route_operations();
        assert_eq!(operations.len(), 10);
        for family in operations.chunks(5) {
            assert!(family[1].contains(&"fwmark".to_owned()));
            for operation in family {
                assert!(operation.contains(&PROTOCOL.to_owned()));
                assert!(!operation.contains(&"flush".to_owned()));
                assert!(!operation.contains(&"replace".to_owned()));
            }
        }
    }
}
