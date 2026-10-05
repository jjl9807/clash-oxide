use anyhow::{Context, Result, bail};
use oxide_model::Diagnostic;
use oxide_model::{MAX_PROFILE, ROUTE_TABLE, Settings, TUN_DEVICE};
use serde_yaml::{Mapping, Value};
use std::path::{Component, Path};

#[derive(Debug)]
pub struct Prepared {
    pub yaml: String,
    pub port: u16,
    pub warnings: Vec<String>,
}

pub fn prepare(
    raw: &str,
    settings: &Settings,
    resources: &Path,
    local_base: Option<&Path>,
) -> Result<Prepared> {
    if raw.len() > MAX_PROFILE {
        bail!(Diagnostic::new(
            "error.profile_limit_size",
            "Profile exceeds the 2 MiB limit"
        ));
    }
    if settings.mixed_port == 0 {
        bail!(Diagnostic::new(
            "error.port",
            "Mixed port must be between 1 and 65535"
        ));
    }
    if !["rule", "global", "direct"].contains(&settings.mode.as_str()) {
        bail!(Diagnostic::new("error.mode", "Unknown routing mode"));
    }
    let mut value: Value = serde_yaml::from_str(raw).context(Diagnostic::new(
        "error.yaml",
        "Expected a Clash/Mihomo YAML configuration",
    ))?;
    value.apply_merge()?;
    let map = value.as_mapping_mut().context(Diagnostic::new(
        "error.yaml_mapping",
        "Profile must be a YAML mapping",
    ))?;
    let mut warnings = Vec::new();
    for key in ["listeners", "inbound-providers"] {
        if map.contains_key(Value::from(key)) {
            bail!("{key} is not supported by this MVP; use the application's mixed inbound");
        }
    }
    // Application-owned listeners and controller settings cannot be overridden
    // by subscriptions. The original profile is preserved verbatim on disk.
    for key in [
        "port",
        "socks-port",
        "redir-port",
        "tproxy-port",
        "external-controller",
        "external-controller-ipc",
        "external-controller-tls",
        "external-controller-unix",
        "external-controller-pipe",
        "external-ui",
        "external-ui-url",
        "external-ui-name",
        "secret",
        "authentication",
        "skip-auth-prefixes",
    ] {
        if map.remove(Value::from(key)).is_some() {
            warnings.push(format!(
                "{key} is managed by Clash Oxide and is not applied"
            ));
        }
    }
    set(map, "mixed-port", settings.mixed_port)?;
    set(map, "bind-address", "127.0.0.1")?;
    set(map, "allow-lan", false)?;
    set(map, "mode", &settings.mode)?;
    if settings.tun {
        set(map, "routing-mark", ROUTE_TABLE)?;
    } else {
        map.remove(Value::from("routing-mark"));
    }
    let tun = serde_yaml::from_str::<Value>(&format!(
        "enable: {}\ndevice-id: {TUN_DEVICE}\ngateway: 198.18.0.1/30\ngateway-v6: fdfe:dcba:9876::1/126\nroute-all: false\nso-mark: {ROUTE_TABLE}\nmtu: 1500\ndns-hijack: true\n",
        settings.tun
    ))?;
    let mut tun = tun;
    if !settings.tun {
        tun.as_mapping_mut().unwrap().remove(Value::from("so-mark"));
    }
    map.insert(Value::from("tun"), tun);
    let profile = map
        .entry(Value::from("profile"))
        .or_insert_with(|| Value::Mapping(Mapping::new()))
        .as_mapping_mut()
        .context("profile must be a mapping")?;
    set(profile, "store-selected", true)?;
    set(profile, "store-fake-ip", true)?;
    let dns = map
        .entry(Value::from("dns"))
        .or_insert_with(|| Value::Mapping(Mapping::new()))
        .as_mapping_mut()
        .context("dns must be a mapping")?;
    set(dns, "enable", true)?;
    // TUN uses the resolver directly; no privileged or conflicting DNS port.
    dns.remove(Value::from("listen"));
    if !dns.contains_key(Value::from("nameserver")) {
        set(dns, "nameserver", vec!["1.1.1.1", "8.8.8.8"])?;
    }
    if let Some(policy) = dns
        .get(Value::from("nameserver-policy"))
        .and_then(Value::as_mapping)
        && policy.values().any(Value::is_sequence)
    {
        bail!(
            "clash-rs currently requires a single resolver string for each nameserver-policy entry; resolver lists cannot be imported without changing their semantics"
        );
    }
    for key in ["proxy-providers", "rule-providers"] {
        if let Some(providers) = map
            .get_mut(Value::from(key))
            .and_then(Value::as_mapping_mut)
        {
            for (_, entry) in providers {
                let entry = entry.as_mapping_mut().context("Invalid provider entry")?;
                if let Some(path) = entry.get(Value::from("path")).and_then(Value::as_str) {
                    let path = Path::new(path);
                    let file_provider =
                        entry.get(Value::from("type")).and_then(Value::as_str) == Some("file");
                    let resolved = if file_provider {
                        let base = local_base
                            .context("Remote subscriptions cannot read local file providers")?;
                        if path.is_absolute() {
                            path.to_path_buf()
                        } else {
                            base.join(path)
                        }
                    } else {
                        if path.is_absolute()
                            || path.components().any(|c| matches!(c, Component::ParentDir))
                        {
                            bail!(
                                "HTTP provider cache paths must be relative and may not contain '..'"
                            );
                        }
                        resources.join(path)
                    };
                    set(entry, "path", resolved.to_string_lossy().as_ref())?;
                }
            }
        }
    }
    // Cache and geodata are always private to this application.
    for key in ["mmdb", "geosite", "asn-mmdb"] {
        if let Some(path) = map.get(Value::from(key)).and_then(Value::as_str) {
            let file = Path::new(path)
                .file_name()
                .context("Invalid geodata path")?;
            set(map, key, resources.join(file).to_string_lossy().as_ref())?;
        }
    }
    let yaml = serde_yaml::to_string(&value)?;
    clash_lib::Config::Str(yaml.clone())
        .try_parse()
        .context(Diagnostic::new(
            "error.config_invalid",
            "Unsupported or invalid clash-rs configuration",
        ))?;
    Ok(Prepared {
        yaml,
        port: settings.mixed_port,
        warnings,
    })
}

fn set(map: &mut Mapping, key: &str, value: impl serde::Serialize) -> Result<()> {
    map.insert(Value::from(key), serde_yaml::to_value(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_cannot_expose_controller_or_write_provider_outside_cache() {
        let raw = "external-controller: 0.0.0.0:9090\nallow-lan: true\nproxy-providers:\n  x:\n    type: http\n    url: https://example.com/a.yaml\n    path: ../../outside\n";
        assert!(
            prepare(raw, &Settings::default(), Path::new("/tmp/cache"), None)
                .unwrap_err()
                .to_string()
                .contains("cache paths")
        );
    }

    #[test]
    fn app_tun_settings_override_imported_mihomo_tun() {
        let raw = "tun:\n  enable: true\n  auto-route: true\nproxies: []\nproxy-groups: []\nrules: [MATCH,DIRECT]\n";
        // A valid MATCH rule is a single YAML string.
        let raw = raw.replace("[MATCH,DIRECT]", "['MATCH,DIRECT']");
        let prepared = prepare(&raw, &Settings::default(), Path::new("/tmp/cache"), None).unwrap();
        let doc: Value = serde_yaml::from_str(&prepared.yaml).unwrap();
        assert_eq!(doc["tun"]["enable"].as_bool(), Some(false));
        assert_eq!(doc["tun"]["route-all"].as_bool(), Some(false));
        assert_eq!(doc["bind-address"].as_str(), Some("127.0.0.1"));
    }
}
