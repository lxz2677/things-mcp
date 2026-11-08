use std::path::PathBuf;

use anyhow::{Context, Result};

pub const DEFAULT_BASE_URL: &str = "https://cloud.culturedcode.com";
pub const DEFAULT_DB_FILE: &str = "things.sqlite";

/// Runtime configuration.
///
/// Credentials are never stored on disk: the password only ever comes from
/// the environment (`THINGS_PASSWORD`, or `THINGS_TOKEN` as an alias).
#[derive(Clone, Debug)]
pub struct Config {
    pub email: Option<String>,
    pub password: Option<String>,
    pub base_url: String,
    pub db_path: PathBuf,
}

impl Config {
    pub fn from_env(
        email_override: Option<String>,
        base_url_override: Option<String>,
        db_override: Option<PathBuf>,
    ) -> Result<Self> {
        let email = email_override
            .or_else(|| env_non_empty("THINGS_EMAIL"))
            .or_else(|| env_non_empty("THINGS_USERNAME"))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let password = env_non_empty("THINGS_PASSWORD").or_else(|| env_non_empty("THINGS_TOKEN"));

        let base_url = base_url_override
            .or_else(|| env_non_empty("THINGS_BASE_URL"))
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        let db_path = db_override
            .or_else(|| env_non_empty("THINGS_DB").map(PathBuf::from))
            .unwrap_or_else(default_db_path);

        Ok(Self {
            email,
            password,
            base_url,
            db_path,
        })
    }

    /// Credentials are only required for commands that talk to Things Cloud;
    /// local read commands work without them.
    pub fn email(&self) -> Result<&str> {
        self.email
            .as_deref()
            .context("missing Things account email: set THINGS_EMAIL (or pass --email)")
    }

    pub fn password(&self) -> Result<&str> {
        self.password
            .as_deref()
            .context("missing Things Cloud password: set THINGS_PASSWORD")
    }
}

pub fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Default database location: per-user application data, so that a stdio MCP
/// server launched from any working directory finds the same replica.
pub fn default_db_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = &home {
            return home
                .join("Library")
                .join("Application Support")
                .join("things-mcp")
                .join(DEFAULT_DB_FILE);
        }
    }

    if let Some(xdg) = env_non_empty("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("things-mcp").join(DEFAULT_DB_FILE);
    }
    if let Some(home) = home {
        return home
            .join(".local")
            .join("share")
            .join("things-mcp")
            .join(DEFAULT_DB_FILE);
    }
    PathBuf::from(DEFAULT_DB_FILE)
}
