//! ClickHouse connection settings: environment first, then the `.env` file (the same rules as
//! `sql/apply.sh`). The password never comes from argv and is never printed (`Debug` redacts it).
//!
//! Keys: `CLICKHOUSE_URL` (default `http://127.0.0.1:${CLICKHOUSE_HTTP_PORT:-18123}/`),
//! `CLICKHOUSE_USER` (default `hood`), `CLICKHOUSE_PASSWORD` (required).
//! `.env` format: plain `KEY=value` lines, the last one wins, CRLF and one pair of surrounding
//! quotes are stripped; the file is parsed, never executed.
//!
//! Only a loopback URL is accepted: ClickHouse on a server is a separate decision of Michael's
//! (task 032, "Что НЕ делать").

use std::fmt;
use std::path::Path;

use anyhow::{bail, Context, Result};
use hood_core::redact::redact_url;

/// Default HTTP port of the local ClickHouse (`docker-compose.yml`, `.env.example`).
pub const DEFAULT_HTTP_PORT: u16 = 18123;

/// A secret that does not show up in `Debug` output or error messages.
#[derive(Clone)]
pub struct Password(String);

impl Password {
    /// The secret itself, for the request header only.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

/// Where and as whom to connect.
#[derive(Debug, Clone)]
pub struct ChConfig {
    /// Base URL, e.g. `http://127.0.0.1:18123/`.
    pub url: String,
    pub user: String,
    pub password: Password,
}

impl ChConfig {
    /// Settings from the process environment and `env_file` (missing file = no values from it).
    ///
    /// # Errors
    /// Unreadable `env_file`, no password, a non-loopback or malformed URL.
    pub fn load(env_file: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(env_file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("read {}", env_file.display())),
        };
        Self::resolve(|k| std::env::var(k).ok().filter(|v| !v.is_empty()), &text)
            .with_context(|| format!("ClickHouse settings (environment, then {})", env_file.display()))
    }

    /// Pure part of [`Self::load`]: `env` is the process environment, `env_text` the `.env` file.
    ///
    /// # Errors
    /// See [`Self::load`].
    pub fn resolve(env: impl Fn(&str) -> Option<String>, env_text: &str) -> Result<Self> {
        let get = |k: &str| env(k).or_else(|| env_file_value(env_text, k)).filter(|v| !v.is_empty());
        let Some(password) = get("CLICKHOUSE_PASSWORD") else {
            bail!("CLICKHOUSE_PASSWORD is not set");
        };
        let url = match get("CLICKHOUSE_URL") {
            Some(u) => u,
            None => {
                let port = match get("CLICKHOUSE_HTTP_PORT") {
                    Some(p) => p.parse::<u16>().with_context(|| format!("CLICKHOUSE_HTTP_PORT {p:?}"))?,
                    None => DEFAULT_HTTP_PORT,
                };
                format!("http://127.0.0.1:{port}/")
            }
        };
        check_loopback(&url)?;
        Ok(Self {
            url,
            user: get("CLICKHOUSE_USER").unwrap_or_else(|| "hood".to_owned()),
            password: Password(password),
        })
    }
}

/// Value of `key` in a `.env` text: the last `key=value` line wins; trailing `\r` and one pair of
/// surrounding quotes are stripped. `None` if there is no such line.
pub(crate) fn env_file_value(text: &str, key: &str) -> Option<String> {
    let mut val = None;
    for line in text.split('\n') {
        if let Some(v) = line.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
            val = Some(v);
        }
    }
    let v = val?.strip_suffix('\r').unwrap_or(val?);
    let unq = |q: char| v.strip_prefix(q).and_then(|x| x.strip_suffix(q));
    Some(unq('"').or_else(|| unq('\'')).unwrap_or(v).to_owned())
}

/// `http://` URL whose host is 127.0.0.1, localhost or [::1].
fn check_loopback(url: &str) -> Result<()> {
    let Some(rest) = url.strip_prefix("http://") else {
        bail!("CLICKHOUSE_URL {}: only http:// to a loopback address is supported", redact_url(url));
    };
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        bail!("CLICKHOUSE_URL must not carry credentials (use CLICKHOUSE_USER / CLICKHOUSE_PASSWORD)");
    }
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        bail!("CLICKHOUSE_URL host {host:?} is not loopback: the loader only talks to a local ClickHouse");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn env_file_rules_match_apply_sh() {
        let t = "CLICKHOUSE_PASSWORD=a\nCLICKHOUSE_PASSWORD=\"b c\"\r\nOTHER=x\nCLICKHOUSE_PASSWORD_X=no\n";
        assert_eq!(env_file_value(t, "CLICKHOUSE_PASSWORD").as_deref(), Some("b c"));
        assert_eq!(env_file_value("K='v'\n", "K").as_deref(), Some("v"));
        assert_eq!(env_file_value("K=v # c\n", "K").as_deref(), Some("v # c"));
        assert_eq!(env_file_value("export K=v\n", "K"), None);
        assert_eq!(env_file_value("", "K"), None);
    }

    #[test]
    fn environment_wins_and_defaults_apply() {
        let c = ChConfig::resolve(none, "CLICKHOUSE_PASSWORD=p\n").unwrap();
        assert_eq!((c.url.as_str(), c.user.as_str(), c.password.expose()), ("http://127.0.0.1:18123/", "hood", "p"));
        let env = |k: &str| (k == "CLICKHOUSE_PASSWORD").then(|| "q".to_owned());
        let c = ChConfig::resolve(env, "CLICKHOUSE_PASSWORD=p\nCLICKHOUSE_HTTP_PORT=28000\n").unwrap();
        assert_eq!((c.url.as_str(), c.password.expose()), ("http://127.0.0.1:28000/", "q"));
        assert!(!format!("{c:?}").contains('q'), "password must not be in Debug");
        assert!(ChConfig::resolve(none, "").is_err());
    }

    #[test]
    fn only_loopback_urls() {
        for ok in ["http://127.0.0.1:1/", "http://localhost:8123", "http://[::1]:8123/"] {
            assert!(check_loopback(ok).is_ok(), "{ok}");
        }
        for bad in ["https://127.0.0.1/", "http://10.0.0.1:8123/", "http://u:p@127.0.0.1/", "http://127.0.0.1.evil/"] {
            assert!(check_loopback(bad).is_err(), "{bad}");
        }
    }
}
