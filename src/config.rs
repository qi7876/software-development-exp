//! Startup configuration, read once before accepting requests.

use std::{net::SocketAddr, path::PathBuf};

use serde::Deserialize;
use thiserror::Error;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) listen: SocketAddr,
    pub(crate) secret_key: String,
}

#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    #[error("cannot read configuration {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    // Do not include serde's source: malformed values can contain the secret.
    #[error(
        "invalid configuration {path} at line {line}, column {column}: expected JSON with listen (IP:port) and secret_key"
    )]
    Parse {
        path: PathBuf,
        line: usize,
        column: usize,
    },
    #[error(
        "secret_key must be non-empty and contain only visible ASCII characters without spaces"
    )]
    SecretKey,
}

impl Config {
    pub(crate) fn load(path: PathBuf) -> Result<Self, ConfigError> {
        let bytes = std::fs::read(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        let config: Self = serde_json::from_slice(&bytes).map_err(|source| ConfigError::Parse {
            path,
            line: source.line(),
            column: source.column(),
        })?;
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.secret_key.is_empty()
            || !self.secret_key.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(ConfigError::SecretKey);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_keys_that_cannot_be_used_as_bearer_tokens() {
        for secret_key in ["", " ", "has space", "line\nbreak", "中文"] {
            let config = Config {
                listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
                secret_key: secret_key.to_owned(),
            };
            assert!(config.validate().is_err());
        }
    }

    #[test]
    fn rejects_missing_and_unknown_configuration_fields() {
        for json in [
            r#"{"listen":"127.0.0.1:8080"}"#,
            r#"{"secret_key":"secret"}"#,
            r#"{"listen":"127.0.0.1:8080","secret_key":"secret","typo":true}"#,
            r#"{"listen":"not-an-address","secret_key":"secret"}"#,
        ] {
            assert!(serde_json::from_str::<Config>(json).is_err());
        }
    }
}
