//! Persistent settings of the desktop app (server, skin, workspace folder) and
//! the personal access token.
//!
//! Both live in the per-user configuration directory of the OS
//! (`~/.config/io.github.rudalabs.teamcraft`, `~/Library/Application Support/…`,
//! `%APPDATA%\…`). The token is a file of its own that only the user may read.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const SETTINGS_FILE: &str = "settings.json";
const TOKEN_FILE: &str = "token";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Base URL of the TeamCraft server, e.g. `https://plm.example.org`.
    pub server_url: Option<String>,
    /// Skin id (see `shared/css/skins.css`).
    pub skin: Option<String>,
    /// Root of the local workspace (check-out folder).
    pub workspace_dir: Option<PathBuf>,
    /// Zoom factor of the user interface (Ctrl + mouse wheel), 1.0 = 100 %.
    pub zoom: Option<f64>,
    /// Local ACP agents the user configured (Claude Code adapter, Gemini CLI,
    /// …); none by default – nothing is downloaded or started on its own.
    #[serde(default)]
    pub agents: Vec<crate::acp::AgentProfile>,
    /// Name of the agent picked last.
    pub agent: Option<String>,
    /// The model picked last per agent (agent name → the agent's model id);
    /// applied at the next start if the agent still offers it.
    #[serde(default)]
    pub agent_models: std::collections::BTreeMap<String, String>,
    /// Not stored in settings.json.
    #[serde(skip)]
    pub token: Option<String>,
}

impl Settings {
    /// Reads the settings; missing or broken files give the defaults.
    pub fn load(dir: &Path) -> Settings {
        let mut settings: Settings = fs::read_to_string(dir.join(SETTINGS_FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        settings.token = fs::read_to_string(dir.join(TOKEN_FILE))
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        settings
    }

    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{SETTINGS_FILE}.tmp"));
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(tmp, dir.join(SETTINGS_FILE))?;
        Ok(())
    }

    /// Stores (or with `None` deletes) the token, readable by the user only.
    pub fn save_token(&mut self, dir: &Path, token: Option<String>) -> anyhow::Result<()> {
        let path = dir.join(TOKEN_FILE);
        match &token {
            Some(token) => {
                fs::create_dir_all(dir)?;
                let mut options = fs::OpenOptions::new();
                options.write(true).create(true).truncate(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(&path)?;
                file.write_all(token.as_bytes())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
                }
            }
            None => {
                if path.exists() {
                    fs::remove_file(&path)?;
                }
            }
        }
        self.token = token;
        Ok(())
    }
}

/// Normalises a server address typed by the user: `plm.example.org` →
/// `https://plm.example.org`, no trailing slash, no query.
pub fn normalize_server_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim().trim_end_matches('/');
    if raw.is_empty() {
        return Err("Enter the address of the TeamCraft server.".into());
    }
    let with_scheme = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let url = url::Url::parse(&with_scheme)
        .map_err(|e| format!("'{raw}' is not a valid address: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(format!("'{raw}' is not an http(s) address."));
    }
    let mut out = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
    if let Some(port) = url.port() {
        out.push_str(&format!(":{port}"));
    }
    let path = url.path().trim_end_matches('/');
    out.push_str(path);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls() {
        assert_eq!(
            normalize_server_url("plm.example.org").unwrap(),
            "https://plm.example.org"
        );
        assert_eq!(
            normalize_server_url(" http://localhost:8080/ ").unwrap(),
            "http://localhost:8080"
        );
        assert_eq!(
            normalize_server_url("https://plm.example.org/teamcraft/?x=1").unwrap(),
            "https://plm.example.org/teamcraft"
        );
        assert!(normalize_server_url("").is_err());
        assert!(normalize_server_url("ftp://x").is_err());
    }

    #[test]
    fn settings_and_token_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Settings::load(dir.path());
        assert!(s.server_url.is_none() && s.token.is_none());
        s.server_url = Some("https://plm.example.org".into());
        s.skin = Some("teamcraft-day".into());
        s.save(dir.path()).unwrap();
        s.save_token(dir.path(), Some("tc_pat_secret".into()))
            .unwrap();
        let json = fs::read_to_string(dir.path().join(SETTINGS_FILE)).unwrap();
        assert!(
            !json.contains("tc_pat_secret"),
            "the token is not in settings.json"
        );
        let loaded = Settings::load(dir.path());
        assert_eq!(
            loaded.server_url.as_deref(),
            Some("https://plm.example.org")
        );
        assert_eq!(loaded.token.as_deref(), Some("tc_pat_secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(TOKEN_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        s.save_token(dir.path(), None).unwrap();
        assert!(Settings::load(dir.path()).token.is_none());
    }
}
