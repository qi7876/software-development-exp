use std::{
    error::Error,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(serde::Deserialize, Debug)]
pub(crate) struct Config {
    pub(crate) listen: SocketAddr,
    pub(crate) secret_key: String,
    pub(crate) web_ui: WebUIConfig,
    #[serde(default)]
    pub(crate) logging: LoggingConfig,
}

#[derive(serde::Deserialize, Debug)]
#[serde(default)]
pub(crate) struct LoggingConfig {
    pub(crate) enabled: bool,
    pub(crate) file: PathBuf,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            file: PathBuf::from("jobs.jsonl"),
        }
    }
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct WebUIConfig {
    #[serde(default)]
    pub(crate) enabled: bool,
    pub(crate) archive_url: url::Url,
    pub(crate) directory: PathBuf,
}

impl Config {
    pub(crate) fn load(working_directory: &Path) -> Result<Self, Box<dyn Error>> {
        let config_path = working_directory.join("config.json");
        let config_data = std::fs::read(&config_path)
            .map_err(|error| format!("cannot read config {}: {error}", config_path.display()))?;
        let mut config = serde_json::from_slice::<Config>(&config_data)
            .map_err(|error| format!("cannot parse config {}: {error}", config_path.display()))?;

        if config.secret_key.is_empty() {
            return Err("secret_key must not be empty".into());
        }

        if config.web_ui.enabled {
            if !matches!(config.web_ui.archive_url.scheme(), "http" | "https") {
                return Err("web_ui.archive_url must use http or https".into());
            }

            if config.web_ui.directory.as_os_str().is_empty() {
                return Err("web_ui.directory must not be empty".into());
            }
        }

        if config.web_ui.directory.is_relative() {
            config.web_ui.directory = working_directory.join(&config.web_ui.directory);
        }

        if config.logging.enabled && config.logging.file.as_os_str().is_empty() {
            return Err("logging.file must not be empty when logging is enabled".into());
        }
        if config.logging.file.is_relative() {
            config.logging.file = working_directory.join(&config.logging.file);
        }

        Ok(config)
    }
}
