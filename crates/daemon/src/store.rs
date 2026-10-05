use anyhow::{Context, Result, bail};
use oxide_model::Diagnostic;
use oxide_model::{MAX_PROFILE, Profile, Settings};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct StoredProfile {
    pub public: Profile,
    pub source: String,
    pub local_base: Option<PathBuf>,
    #[serde(default)]
    pub selections: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Store {
    pub profiles: Vec<StoredProfile>,
    pub active: Option<String>,
    pub settings: Settings,
}

pub fn private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    let result = (|| -> Result<()> {
        file.write_all(data)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

impl Store {
    pub fn load(directory: &Path) -> Result<Self> {
        private_dir(directory)?;
        private_dir(&directory.join("profiles"))?;
        match std::fs::read(directory.join("state.json")) {
            Ok(data) => serde_json::from_slice(&data)
                .context("Saved state is invalid; preserving it for recovery"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self, directory: &Path) -> Result<()> {
        atomic_write(
            &directory.join("state.json"),
            &serde_json::to_vec_pretty(self)?,
        )
    }
    pub fn profile(&self, id: &str) -> Result<&StoredProfile> {
        self.profiles
            .iter()
            .find(|p| p.public.id == id)
            .context(Diagnostic::new(
                "error.profile_missing",
                "Profile not found",
            ))
    }
}

pub fn profile_path(directory: &Path, id: &str) -> Result<PathBuf> {
    let id = uuid::Uuid::parse_str(id)
        .context(Diagnostic::new("error.profile_id", "Invalid profile ID"))?;
    Ok(directory.join("profiles").join(format!("{id}.yaml")))
}

pub async fn read_source(
    source: &str,
    running_port: Option<u16>,
) -> Result<(String, Option<PathBuf>)> {
    if source.starts_with("https://") || source.starts_with("http://") {
        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .no_proxy();
        if let Some(port) = running_port {
            builder = builder.proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}"))?);
        }
        let mut response = builder
            .build()?
            .get(source)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?
            .error_for_status()
            .map_err(reqwest::Error::without_url)?;
        if response
            .content_length()
            .is_some_and(|n| n > MAX_PROFILE as u64)
        {
            bail!(Diagnostic::new(
                "error.subscription_large",
                "Subscription exceeds 2 MiB"
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(reqwest::Error::without_url)?
        {
            if bytes.len() + chunk.len() > MAX_PROFILE {
                bail!(Diagnostic::new(
                    "error.subscription_large",
                    "Subscription exceeds 2 MiB"
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok((
            String::from_utf8(bytes).context(Diagnostic::new(
                "error.subscription_utf8",
                "Subscription must contain UTF-8 YAML",
            ))?,
            None,
        ));
    }
    let path = Path::new(source).canonicalize().context(Diagnostic::new(
        "error.file_open",
        "Cannot open local configuration file",
    ))?;
    if !path.is_file() || path.metadata()?.len() > MAX_PROFILE as u64 {
        bail!(Diagnostic::new(
            "error.file_size",
            "Profile must be a regular file smaller than 2 MiB"
        ));
    }
    let text = tokio::fs::read_to_string(&path).await?;
    if text.len() > MAX_PROFILE {
        bail!(Diagnostic::new(
            "error.profile_large",
            "Profile exceeds 2 MiB"
        ));
    }
    Ok((text, path.parent().map(Path::to_path_buf)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_saved_state_is_not_silently_reset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("state.json"), b"{").unwrap();
        assert!(Store::load(dir.path()).is_err());
        assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), b"{");
    }
    #[test]
    fn profile_ids_cannot_escape_store() {
        assert!(profile_path(Path::new("/tmp"), "../../etc/passwd").is_err());
    }
}
