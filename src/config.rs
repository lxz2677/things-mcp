use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, ensure};
use serde::Deserialize;

pub const DEFAULT_BASE_URL: &str = "https://cloud.culturedcode.com";
pub const DEFAULT_DB_FILE: &str = "things.sqlite";
const CREDENTIALS_FILE: &str = ".things-mcp/credentials.json";

/// Runtime configuration.
#[derive(Clone, Debug)]
pub struct Config {
    pub email: Option<String>,
    pub password: Option<String>,
    pub base_url: String,
    pub db_path: PathBuf,
}

impl Config {
    pub fn load(
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
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let (email, password) = resolve_credentials((email, password), &base_url, home.as_deref())?;

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
            .context("missing Things account email: set THINGS_EMAIL (or pass --email) or use ~/.things-mcp/credentials.json")
    }

    pub fn password(&self) -> Result<&str> {
        self.password
            .as_deref()
            .context("missing Things Cloud password: set THINGS_PASSWORD or use ~/.things-mcp/credentials.json")
    }
}

#[derive(Deserialize)]
struct FileCredentials {
    email: String,
    password: String,
}

fn resolve_credentials(
    explicit: (Option<String>, Option<String>),
    base_url: &str,
    home: Option<&Path>,
) -> Result<(Option<String>, Option<String>)> {
    // Keep credentials from one source together. Alternate endpoints use explicit credentials.
    if explicit.0.is_some()
        || explicit.1.is_some()
        || base_url.trim_end_matches('/') != DEFAULT_BASE_URL
    {
        return Ok(explicit);
    }
    let Some(home) = home.filter(|path| path.is_absolute()) else {
        return Ok(explicit);
    };
    let path = home.join(CREDENTIALS_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(explicit),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let saved: FileCredentials = serde_json::from_slice(&bytes).map_err(|error| {
        anyhow!(
            "invalid {} at line {}, column {}: expected email and password strings",
            path.display(),
            error.line(),
            error.column()
        )
    })?;
    let email = saved.email.trim();
    ensure!(
        !email.is_empty() && !saved.password.trim().is_empty(),
        "{} must contain a non-empty email and password",
        path.display()
    );
    Ok((Some(email.to_owned()), Some(saved.password)))
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
    if let Some(home) = &home {
        return home
            .join("Library/Application Support/things-mcp")
            .join(DEFAULT_DB_FILE);
    }

    if let Some(xdg) = env_non_empty("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("things-mcp").join(DEFAULT_DB_FILE);
    }
    home.map(|path| path.join(".local/share/things-mcp").join(DEFAULT_DB_FILE))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DB_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_credentials(home: &Path, contents: &str) {
        let directory = home.join(".things-mcp");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("credentials.json"), contents).unwrap();
    }

    #[test]
    fn loads_saved_credentials_when_no_explicit_credentials_are_set() {
        let directory = tempfile::tempdir().unwrap();
        let home = Some(directory.path());
        assert_eq!(
            resolve_credentials((None, None), DEFAULT_BASE_URL, home).unwrap(),
            (None, None)
        );

        write_credentials(
            directory.path(),
            r#"{"email":" user@example.com ","password":"  secret  "}"#,
        );
        assert_eq!(
            resolve_credentials((None, None), DEFAULT_BASE_URL, home).unwrap(),
            (Some("user@example.com".into()), Some("  secret  ".into()))
        );
    }

    #[test]
    fn skips_the_file_for_explicit_credentials_or_an_alternate_endpoint() {
        let directory = tempfile::tempdir().unwrap();
        write_credentials(directory.path(), "invalid JSON");
        let home = Some(directory.path());
        let explicit = (
            Some("script@example.com".into()),
            Some("script-secret".into()),
        );
        assert_eq!(
            resolve_credentials(explicit.clone(), DEFAULT_BASE_URL, home).unwrap(),
            explicit
        );
        let partial = (Some("other@example.com".into()), None);
        assert_eq!(
            resolve_credentials(partial.clone(), DEFAULT_BASE_URL, home).unwrap(),
            partial
        );
        assert_eq!(
            resolve_credentials((None, None), "http://127.0.0.1:9", home).unwrap(),
            (None, None)
        );
    }

    #[test]
    fn reports_malformed_credentials_without_disclosing_file_contents() {
        let directory = tempfile::tempdir().unwrap();
        write_credentials(directory.path(), r#""private-text""#);
        let error = resolve_credentials((None, None), DEFAULT_BASE_URL, Some(directory.path()))
            .unwrap_err()
            .to_string();
        assert!(error.contains(CREDENTIALS_FILE));
        assert!(!error.contains("private-text"));
    }
}
