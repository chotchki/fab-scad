//! W.3.27.1: the desktop hotchkiss.io publish credential — resolved from the ENV first, then a config
//! file, so a double-clicked `.app` (which inherits none of your shell env) can still publish once the key
//! is saved. The file is `$XDG_CONFIG_HOME/fab-scad/credentials.toml` (else `$HOME/.config/…`), written
//! owner-only (0600) because it holds a secret. Both the CLI (`fab publish`) and the GUI Publish button
//! resolve through here; the GUI Settings screen writes it. NOT wasm — the web publishes via the site
//! session cookie, no API key.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// hotchkiss.io's default base — the URL when nothing overrides it.
pub const DEFAULT_URL: &str = "https://hotchkiss.io";

/// The on-disk credential file. Every field optional, so a hand-edited or partial file still parses.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Eq, Debug)]
pub struct Credentials {
    pub hio_api_key: Option<String>,
    pub hio_url: Option<String>,
}

impl Credentials {
    /// Blank strings become `None` — the Settings screen hands us `String`s, and an empty field means
    /// "unset", not "the empty key". Keeps a cleared field from serializing as `hio_api_key = ""`.
    fn normalized(mut self) -> Self {
        self.hio_api_key = self.hio_api_key.filter(|s| !s.trim().is_empty());
        self.hio_url = self.hio_url.filter(|s| !s.trim().is_empty());
        self
    }
}

/// Where the resolved key came from — the Settings screen shows this so "why can't I publish?" is legible.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeySource {
    /// `$HIO_API_KEY` (a terminal / CI launch).
    Env,
    /// The saved `credentials.toml` (the double-clicked-app path).
    File,
    /// Nowhere — publish can't proceed.
    Unset,
}

/// The resolved publish config: the key (if any), the base URL (always — defaults to hotchkiss.io), and
/// where the key came from.
pub struct Resolved {
    pub api_key: Option<String>,
    pub url: String,
    pub key_source: KeySource,
}

/// `$XDG_CONFIG_HOME/fab-scad/credentials.toml`, else `$HOME/.config/fab-scad/credentials.toml`. `None`
/// only when NEITHER env var is set (a headless box with no home — publish just stays env-only there).
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("fab-scad").join("credentials.toml"))
}

/// Read the config file, or an empty default if it's absent/unreadable/malformed — a missing file is the
/// common case (nobody's saved a key yet), not an error.
pub fn load_file() -> Credentials {
    let Some(path) = config_path() else {
        return Credentials::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Credentials::default();
    };
    toml::from_str(&text).unwrap_or_default()
}

/// Resolve the publish key + URL. The ENV WINS over the file (a terminal / CI launch with `$HIO_API_KEY`
/// set overrides a saved file), then the file, then unset. The URL follows the same precedence, falling
/// back to [`DEFAULT_URL`]. Env and file are resolved independently — a file key with an env URL is fine.
pub fn resolve() -> Resolved {
    let file = load_file();
    let env_key = std::env::var("HIO_API_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let (api_key, key_source) = match env_key {
        Some(k) => (Some(k), KeySource::Env),
        None => match file.hio_api_key.filter(|s| !s.trim().is_empty()) {
            Some(k) => (Some(k), KeySource::File),
            None => (None, KeySource::Unset),
        },
    };
    let url = std::env::var("HIO_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or(file.hio_url.filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    Resolved {
        api_key,
        url,
        key_source,
    }
}

/// Persist the credential file (creating the dir), owner-only (0600) — it holds a secret. Empty fields
/// are dropped ([`Credentials::normalized`]), so clearing the key in Settings removes it from disk.
/// Returns the path written so the caller can show it.
pub fn save_file(creds: &Credentials) -> Result<PathBuf> {
    let path =
        config_path().context("no $HOME or $XDG_CONFIG_HOME — can't locate a config directory")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let text =
        toml::to_string_pretty(&creds.clone().normalized()).context("serialize credentials")?;

    // On unix, tighten the fd to 0600 BEFORE the key bytes land — never a world-readable window.
    // `.mode(0o600)` only takes effect when open(2) CREATES the file; a pre-existing (e.g. hand-edited
    // 0644) file keeps its mode through open. `.truncate` empties it AT open, so chmod'ing the still-empty
    // fd before `write_all` guarantees the secret is only ever written into an owner-only file.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("open {} for write", path.display()))?;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", path.display()))?;
        f.write_all(text.as_bytes())
            .with_context(|| format!("write {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A brace-scoped env + HOME sandbox so these tests don't touch a real `credentials.toml` or race on
    /// the process-global env. Serialized via a mutex — env vars are process-wide.
    struct Sandbox {
        _lock: std::sync::MutexGuard<'static, ()>,
        _dir: tempfile::TempDir,
        prev: Vec<(&'static str, Option<String>)>,
    }
    impl Sandbox {
        fn new() -> Self {
            static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = tempfile::tempdir().unwrap();
            let prev: Vec<(&str, Option<String>)> =
                ["HOME", "XDG_CONFIG_HOME", "HIO_API_KEY", "HIO_URL"]
                    .iter()
                    .map(|k| (*k, std::env::var(k).ok()))
                    .collect();
            // SAFETY: single-threaded within the locked section; restored on Drop.
            unsafe {
                std::env::set_var("HOME", dir.path());
                std::env::remove_var("XDG_CONFIG_HOME");
                std::env::remove_var("HIO_API_KEY");
                std::env::remove_var("HIO_URL");
            }
            Sandbox {
                _lock,
                _dir: dir,
                prev,
            }
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            for (k, v) in &self.prev {
                // SAFETY: still inside the locked section (guard drops after this).
                unsafe {
                    match v {
                        Some(val) => std::env::set_var(k, val),
                        None => std::env::remove_var(k),
                    }
                }
            }
        }
    }

    #[test]
    fn missing_file_is_unset_not_an_error() {
        let _s = Sandbox::new();
        let r = resolve();
        assert!(r.api_key.is_none());
        assert_eq!(r.key_source, KeySource::Unset);
        assert_eq!(r.url, DEFAULT_URL, "no key, no url ⇒ the default base");
    }

    #[test]
    fn save_then_resolve_round_trips_from_file() {
        let _s = Sandbox::new();
        let path = save_file(&Credentials {
            hio_api_key: Some("hio_secret".into()),
            hio_url: Some("https://staging.example".into()),
        })
        .unwrap();
        assert!(path.ends_with("fab-scad/credentials.toml"));
        let r = resolve();
        assert_eq!(r.api_key.as_deref(), Some("hio_secret"));
        assert_eq!(r.key_source, KeySource::File);
        assert_eq!(r.url, "https://staging.example");
    }

    #[test]
    fn env_key_wins_over_the_file() {
        let _s = Sandbox::new();
        save_file(&Credentials {
            hio_api_key: Some("from_file".into()),
            hio_url: None,
        })
        .unwrap();
        // SAFETY: inside the Sandbox's locked section.
        unsafe { std::env::set_var("HIO_API_KEY", "from_env") };
        let r = resolve();
        assert_eq!(r.api_key.as_deref(), Some("from_env"));
        assert_eq!(r.key_source, KeySource::Env, "env beats the saved file");
    }

    #[test]
    fn blank_fields_normalize_to_absent() {
        let _s = Sandbox::new();
        save_file(&Credentials {
            hio_api_key: Some("   ".into()), // whitespace-only ⇒ dropped
            hio_url: Some(String::new()),
        })
        .unwrap();
        let r = resolve();
        assert!(
            r.api_key.is_none(),
            "a blank key saves as absent, not empty"
        );
        assert_eq!(r.key_source, KeySource::Unset);
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let _s = Sandbox::new();
        let path = save_file(&Credentials {
            hio_api_key: Some("hio_secret".into()),
            hio_url: None,
        })
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a secret on disk must be owner-only");
    }

    #[test]
    fn whitespace_only_env_key_is_ignored() {
        // A fat-fingered `export HIO_API_KEY="   "` must NOT mask a good saved file key (read/write both
        // treat whitespace-only as unset). Regression for the resolve()/normalized() `.trim()` mismatch.
        let _s = Sandbox::new();
        save_file(&Credentials {
            hio_api_key: Some("from_file".into()),
            hio_url: None,
        })
        .unwrap();
        // SAFETY: inside the Sandbox's locked section.
        unsafe { std::env::set_var("HIO_API_KEY", "   ") };
        let r = resolve();
        assert_eq!(r.api_key.as_deref(), Some("from_file"));
        assert_eq!(
            r.key_source,
            KeySource::File,
            "a whitespace-only env var is not a key"
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_tightens_a_preexisting_loose_file() {
        // A pre-existing 0644 file (hand-created under the default umask, or a perm-dropping restore) must
        // end up 0600 AND the secret must never touch a world-readable file — save_file chmods the fd
        // BEFORE writing. Regression for the ".mode() only applies on create" window.
        use std::os::unix::fs::PermissionsExt;
        let _s = Sandbox::new();
        let path = config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "hio_api_key = \"stale\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        save_file(&Credentials {
            hio_api_key: Some("hio_fresh".into()),
            hio_url: None,
        })
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "a pre-existing loose file is tightened to 0600"
        );
        assert_eq!(resolve().api_key.as_deref(), Some("hio_fresh"));
    }
}
