//! Persistent settings of the desktop app (server, skin, workspace folder) and
//! the personal access token.
//!
//! The settings live in the per-user configuration directory of the OS
//! (`~/.config/io.github.rudalabs.teamcraft`, `~/Library/Application Support/…`,
//! `%APPDATA%\…`). The token goes into the OS keychain (Windows Credential
//! Manager, macOS Keychain, the Secret Service on Linux). Only where no keychain
//! is available (a Linux session without a Secret Service, e.g. a headless or
//! minimal desktop) it falls back to a file of its own that only the user may
//! read – and the app shows a warning. A token found in that file is moved into
//! the keychain as soon as one is available (the file of older app versions).

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const SETTINGS_FILE: &str = "settings.json";
const TOKEN_FILE: &str = "token";

/// Keychain entry of the token: service = the app identifier.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
const KEYCHAIN_SERVICE: &str = "io.github.rudalabs.teamcraft";
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
const KEYCHAIN_ACCOUNT: &str = "api-token";

/// A secure credential store for the one token of the app.
pub trait Keychain {
    /// The stored token; `Ok(None)` when there is none, `Err` when the store
    /// cannot be reached.
    fn get(&self) -> Result<Option<String>, String>;
    fn set(&self, token: &str) -> Result<(), String>;
    /// Removes the token; no token stored is not an error.
    fn delete(&self) -> Result<(), String>;
}

/// The keychain of the operating system (`keyring` crate).
pub struct OsKeychain;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
impl OsKeychain {
    fn entry() -> Result<keyring::Entry, String> {
        keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).map_err(|e| e.to_string())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
impl Keychain for OsKeychain {
    fn get(&self) -> Result<Option<String>, String> {
        match Self::entry()?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, token: &str) -> Result<(), String> {
        Self::entry()?
            .set_password(token)
            .map_err(|e| e.to_string())
    }

    fn delete(&self) -> Result<(), String> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Other systems: no keychain support compiled in – the file fallback.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl Keychain for OsKeychain {
    fn get(&self) -> Result<Option<String>, String> {
        Err("no keychain support on this system".into())
    }
    fn set(&self, _token: &str) -> Result<(), String> {
        Err("no keychain support on this system".into())
    }
    fn delete(&self) -> Result<(), String> {
        Err("no keychain support on this system".into())
    }
}

/// Where the token is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenStorage {
    /// The OS keychain.
    Keychain,
    /// The fallback file (no keychain available) – the UI warns about it.
    File,
}

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
    /// The Agent view's backend the user picked, per server and user
    /// (`"<server_url> <username>"` → `"company"` or `"local"`). Only a
    /// preference: the server's policy decides what is available.
    #[serde(default)]
    pub agent_backends: std::collections::BTreeMap<String, String>,
    /// Look for a newer release on GitHub (at most once a day); `None` = on.
    pub update_check: Option<bool>,
    /// When the release check last ran (seconds since the Unix epoch).
    pub update_checked_at: Option<u64>,
    /// The latest release version the last check found (without the `v`).
    pub update_latest: Option<String>,
    /// The release version whose notice the user closed.
    pub update_dismissed: Option<String>,
    /// Not stored in settings.json.
    #[serde(skip)]
    pub token: Option<String>,
    /// Where the token is kept (`None` while there is no token).
    #[serde(skip)]
    pub token_storage: Option<TokenStorage>,
}

/// Stores the token in the keychain and reads it back: only a keychain that
/// returns what it got counts as working.
fn keychain_store(keychain: &dyn Keychain, token: &str) -> Result<(), String> {
    keychain.set(token)?;
    match keychain.get()? {
        Some(stored) if stored == token => Ok(()),
        _ => Err("the keychain did not return the stored token".into()),
    }
}

fn read_token_file(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join(TOKEN_FILE))
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

fn remove_token_file(dir: &Path) -> std::io::Result<()> {
    match fs::remove_file(dir.join(TOKEN_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Writes the fallback file, readable by the user only.
fn write_token_file(dir: &Path, token: &str) -> anyhow::Result<()> {
    let path = dir.join(TOKEN_FILE);
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
    Ok(())
}

/// Most remembered Agent view backends (servers × users).
pub const MAX_AGENT_BACKENDS: usize = 32;

impl Settings {
    /// Key of [`Settings::agent_backends`]: the configured server and the
    /// signed-in user; `None` without either.
    fn agent_backend_key(&self, username: &str) -> Option<String> {
        let server = self.server_url.as_deref()?;
        (!username.is_empty()).then(|| format!("{server} {username}"))
    }

    /// The backend this user picked last on this server, if any.
    pub fn stored_agent_backend(&self, username: &str) -> Option<crate::acp::AgentBackend> {
        let key = self.agent_backend_key(username)?;
        self.agent_backends
            .get(&key)
            .and_then(|b| crate::acp::AgentBackend::parse(b.as_str()))
    }

    /// Remembers the user's pick (bounded: when the map is full, the first
    /// keys in sort order give way).
    pub fn remember_agent_backend(&mut self, username: &str, backend: crate::acp::AgentBackend) {
        let Some(key) = self.agent_backend_key(username) else {
            return;
        };
        while self.agent_backends.len() >= MAX_AGENT_BACKENDS
            && !self.agent_backends.contains_key(&key)
        {
            self.agent_backends.pop_first();
        }
        self.agent_backends
            .insert(key, backend.as_str().to_string());
    }

    /// Reads the settings and the token (OS keychain); missing or broken
    /// files give the defaults.
    pub fn load(dir: &Path) -> Settings {
        Self::load_with(dir, &OsKeychain)
    }

    pub fn load_with(dir: &Path, keychain: &dyn Keychain) -> Settings {
        let mut settings: Settings = fs::read_to_string(dir.join(SETTINGS_FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if let Some(token) = read_token_file(dir) {
            // A token file: the fallback, or left by an older app version –
            // moved into the keychain as soon as there is one.
            let moved = keychain_store(keychain, &token).is_ok() && remove_token_file(dir).is_ok();
            settings.token_storage = Some(if moved {
                TokenStorage::Keychain
            } else {
                TokenStorage::File
            });
            settings.token = Some(token);
        } else {
            // an unreachable keychain means: not signed in
            settings.token = keychain
                .get()
                .ok()
                .flatten()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty());
            settings.token_storage = settings.token.as_ref().map(|_| TokenStorage::Keychain);
        }
        settings
    }

    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{SETTINGS_FILE}.tmp"));
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(tmp, dir.join(SETTINGS_FILE))?;
        Ok(())
    }

    /// Stores (or with `None` deletes) the token: in the OS keychain, or in
    /// the fallback file when there is none.
    pub fn save_token(&mut self, dir: &Path, token: Option<String>) -> anyhow::Result<()> {
        self.save_token_with(dir, &OsKeychain, token)
    }

    pub fn save_token_with(
        &mut self,
        dir: &Path,
        keychain: &dyn Keychain,
        token: Option<String>,
    ) -> anyhow::Result<()> {
        match &token {
            Some(token) => {
                if keychain_store(keychain, token).is_ok() {
                    // an older token in the fallback file must not stay behind
                    remove_token_file(dir)?;
                    self.token_storage = Some(TokenStorage::Keychain);
                } else {
                    write_token_file(dir, token)?;
                    self.token_storage = Some(TokenStorage::File);
                }
            }
            None => {
                remove_token_file(dir)?;
                if let Err(e) = keychain.delete() {
                    // An unreachable keychain holds nothing to forget; one
                    // that still returns the token would sign in again.
                    if matches!(keychain.get(), Ok(Some(_))) {
                        anyhow::bail!("the token could not be removed from the keychain: {e}");
                    }
                }
                self.token_storage = None;
            }
        }
        self.token = token;
        Ok(())
    }
}

/// True for `http://` addresses of other computers: the token and all data
/// would cross the network unencrypted. `localhost` and loopback addresses
/// are fine (development, a server on the same machine).
pub fn insecure_http(server_url: &str) -> bool {
    let Ok(url) = url::Url::parse(server_url) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            !(name == "localhost" || name.ends_with(".localhost"))
        }
        Some(url::Host::Ipv4(ip)) => !ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => !ip.is_loopback(),
        None => false,
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
    use std::cell::RefCell;

    /// An in-memory keychain; `working: false` behaves like a system without
    /// a Secret Service. Tests never touch the real OS keychain.
    struct FakeKeychain {
        working: bool,
        stored: RefCell<Option<String>>,
    }

    impl FakeKeychain {
        fn new(working: bool) -> Self {
            FakeKeychain {
                working,
                stored: RefCell::new(None),
            }
        }
        fn check(&self) -> Result<(), String> {
            if self.working {
                Ok(())
            } else {
                Err("no Secret Service".into())
            }
        }
    }

    impl Keychain for FakeKeychain {
        fn get(&self) -> Result<Option<String>, String> {
            self.check()?;
            Ok(self.stored.borrow().clone())
        }
        fn set(&self, token: &str) -> Result<(), String> {
            self.check()?;
            *self.stored.borrow_mut() = Some(token.to_string());
            Ok(())
        }
        fn delete(&self) -> Result<(), String> {
            self.check()?;
            *self.stored.borrow_mut() = None;
            Ok(())
        }
    }

    /// A keychain that accepts and returns the token but cannot delete it.
    struct StickyKeychain(RefCell<Option<String>>);

    impl Keychain for StickyKeychain {
        fn get(&self) -> Result<Option<String>, String> {
            Ok(self.0.borrow().clone())
        }
        fn set(&self, token: &str) -> Result<(), String> {
            *self.0.borrow_mut() = Some(token.to_string());
            Ok(())
        }
        fn delete(&self) -> Result<(), String> {
            Err("access denied".into())
        }
    }

    /// A keychain that accepts writes but returns nothing (a broken store).
    struct ForgetfulKeychain;

    impl Keychain for ForgetfulKeychain {
        fn get(&self) -> Result<Option<String>, String> {
            Ok(None)
        }
        fn set(&self, _token: &str) -> Result<(), String> {
            Ok(())
        }
        fn delete(&self) -> Result<(), String> {
            Ok(())
        }
    }

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
    fn insecure_http_spares_https_and_loopback() {
        for url in [
            "http://plm.example.org",
            "http://10.0.0.5:8080",
            "http://[2001:db8::1]:8080",
            "http://localhost.example.org",
        ] {
            assert!(insecure_http(url), "{url}");
        }
        for url in [
            "https://plm.example.org",
            "http://localhost:8080",
            "http://LOCALHOST",
            "http://app.localhost",
            "http://127.0.0.1:8080",
            "http://127.1.2.3",
            "http://[::1]:8080",
            "not a url",
        ] {
            assert!(!insecure_http(url), "{url}");
        }
    }

    #[test]
    fn settings_and_keychain_token_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = FakeKeychain::new(true);
        let mut s = Settings::load_with(dir.path(), &keychain);
        assert!(s.server_url.is_none() && s.token.is_none() && s.token_storage.is_none());
        s.server_url = Some("https://plm.example.org".into());
        s.skin = Some("teamcraft-day".into());
        s.save(dir.path()).unwrap();
        s.save_token_with(dir.path(), &keychain, Some("tc_pat_secret".into()))
            .unwrap();
        assert_eq!(s.token_storage, Some(TokenStorage::Keychain));
        let json = fs::read_to_string(dir.path().join(SETTINGS_FILE)).unwrap();
        assert!(
            !json.contains("tc_pat_secret"),
            "the token is not in settings.json"
        );
        assert!(!dir.path().join(TOKEN_FILE).exists(), "no token file");
        assert_eq!(keychain.stored.borrow().as_deref(), Some("tc_pat_secret"));
        let loaded = Settings::load_with(dir.path(), &keychain);
        assert_eq!(
            loaded.server_url.as_deref(),
            Some("https://plm.example.org")
        );
        assert_eq!(loaded.token.as_deref(), Some("tc_pat_secret"));
        assert_eq!(loaded.token_storage, Some(TokenStorage::Keychain));
        s.save_token_with(dir.path(), &keychain, None).unwrap();
        assert!(keychain.stored.borrow().is_none());
        let loaded = Settings::load_with(dir.path(), &keychain);
        assert!(loaded.token.is_none() && loaded.token_storage.is_none());
    }

    #[test]
    fn the_agent_backend_is_kept_per_server_and_user() {
        use crate::acp::AgentBackend;
        let dir = tempfile::tempdir().unwrap();
        let keychain = FakeKeychain::new(true);
        let mut s = Settings::load_with(dir.path(), &keychain);
        // no server yet: nothing to key it by
        s.remember_agent_backend("lina", AgentBackend::Company);
        assert!(s.agent_backends.is_empty());
        s.server_url = Some("https://plm.example.org".into());
        s.remember_agent_backend("lina", AgentBackend::Company);
        s.remember_agent_backend("", AgentBackend::Local);
        s.save(dir.path()).unwrap();
        let mut loaded = Settings::load_with(dir.path(), &keychain);
        assert_eq!(
            loaded.stored_agent_backend("lina"),
            Some(AgentBackend::Company)
        );
        assert_eq!(loaded.stored_agent_backend("tom"), None);
        assert_eq!(loaded.agent_backends.len(), 1);
        loaded.server_url = Some("https://other.example.org".into());
        assert_eq!(loaded.stored_agent_backend("lina"), None);
        // a value an older or newer app wrote is ignored, not an error
        loaded
            .agent_backends
            .insert("https://other.example.org lina".into(), "off".into());
        assert_eq!(loaded.stored_agent_backend("lina"), None);
        // bounded
        for n in 0..(MAX_AGENT_BACKENDS + 5) {
            loaded.remember_agent_backend(&format!("user{n}"), AgentBackend::Local);
        }
        assert_eq!(loaded.agent_backends.len(), MAX_AGENT_BACKENDS);
        assert_eq!(
            loaded.stored_agent_backend(&format!("user{}", MAX_AGENT_BACKENDS + 4)),
            Some(AgentBackend::Local)
        );
    }

    #[test]
    fn without_a_keychain_the_token_falls_back_to_a_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let none = FakeKeychain::new(false);
        let mut s = Settings::load_with(dir.path(), &none);
        assert!(s.token.is_none());
        s.save_token_with(dir.path(), &none, Some("tc_pat_secret".into()))
            .unwrap();
        assert_eq!(s.token_storage, Some(TokenStorage::File));
        let loaded = Settings::load_with(dir.path(), &none);
        assert_eq!(loaded.token.as_deref(), Some("tc_pat_secret"));
        assert_eq!(loaded.token_storage, Some(TokenStorage::File));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(TOKEN_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // signing out works without a keychain as well
        s.save_token_with(dir.path(), &none, None).unwrap();
        assert!(!dir.path().join(TOKEN_FILE).exists());
        assert!(Settings::load_with(dir.path(), &none).token.is_none());
    }

    #[test]
    fn a_keychain_that_loses_the_token_counts_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Settings::default();
        s.save_token_with(dir.path(), &ForgetfulKeychain, Some("tc_pat_secret".into()))
            .unwrap();
        assert_eq!(s.token_storage, Some(TokenStorage::File));
        assert_eq!(
            read_token_file(dir.path()).as_deref(),
            Some("tc_pat_secret")
        );
    }

    #[test]
    fn a_token_file_moves_into_the_keychain() {
        let dir = tempfile::tempdir().unwrap();
        // the file an older app version (or a session without keychain) wrote
        write_token_file(dir.path(), "tc_pat_old\n").unwrap();
        let keychain = FakeKeychain::new(true);
        let loaded = Settings::load_with(dir.path(), &keychain);
        assert_eq!(loaded.token.as_deref(), Some("tc_pat_old"));
        assert_eq!(loaded.token_storage, Some(TokenStorage::Keychain));
        assert_eq!(keychain.stored.borrow().as_deref(), Some("tc_pat_old"));
        assert!(!dir.path().join(TOKEN_FILE).exists(), "the file is gone");
        // and it is found there at the next start
        let again = Settings::load_with(dir.path(), &keychain);
        assert_eq!(again.token.as_deref(), Some("tc_pat_old"));
    }

    #[test]
    fn a_token_file_stays_while_there_is_no_keychain() {
        let dir = tempfile::tempdir().unwrap();
        write_token_file(dir.path(), "tc_pat_old").unwrap();
        let loaded = Settings::load_with(dir.path(), &FakeKeychain::new(false));
        assert_eq!(loaded.token.as_deref(), Some("tc_pat_old"));
        assert_eq!(loaded.token_storage, Some(TokenStorage::File));
        assert!(dir.path().join(TOKEN_FILE).exists(), "nothing is lost");
    }

    #[test]
    fn storing_in_the_keychain_removes_an_old_token_file() {
        let dir = tempfile::tempdir().unwrap();
        write_token_file(dir.path(), "tc_pat_old").unwrap();
        let keychain = FakeKeychain::new(true);
        let mut s = Settings::default();
        s.save_token_with(dir.path(), &keychain, Some("tc_pat_new".into()))
            .unwrap();
        assert!(!dir.path().join(TOKEN_FILE).exists());
        assert_eq!(
            Settings::load_with(dir.path(), &keychain).token.as_deref(),
            Some("tc_pat_new")
        );
    }

    #[test]
    fn sign_out_fails_when_the_keychain_keeps_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = StickyKeychain(RefCell::new(None));
        let mut s = Settings::default();
        s.save_token_with(dir.path(), &keychain, Some("tc_pat_secret".into()))
            .unwrap();
        assert!(s.save_token_with(dir.path(), &keychain, None).is_err());
    }
}
