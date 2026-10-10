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
        let value: serde_json::Value = serde_json::from_slice(&config_data)
            .map_err(|error| format!("cannot parse config {}: {error}", config_path.display()))?;
        if value.get("logging").is_some() {
            return Err(format!("config {} contains obsolete logging settings; remove logging: job history is now stored in jobs.sqlite3", config_path.display()).into());
        }
        let mut config = serde_json::from_value::<Config>(value)
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

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDirectory;

    #[test]
    fn configuration_rejects_obsolete_logging_settings() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let mut config: serde_json::Value =
            serde_json::from_str(include_str!("../working-directory-example/config.json"))?;
        std::fs::write(
            directory.path().join("config.json"),
            serde_json::to_vec(&config)?,
        )?;
        Config::load(directory.path())?;
        config["logging"] = serde_json::json!({"enabled": false});
        std::fs::write(
            directory.path().join("config.json"),
            serde_json::to_vec(&config)?,
        )?;
        let error = Config::load(directory.path())
            .err()
            .ok_or("accepted obsolete logging settings")?;
        assert!(error.to_string().contains("remove logging"));
        assert!(error.to_string().contains("jobs.sqlite3"));
        Ok(())
    }
}
