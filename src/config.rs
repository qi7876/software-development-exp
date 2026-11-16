use std::{
    error::Error,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(serde::Deserialize, Debug)]
pub(crate) struct Config {
    pub(crate) listen: SocketAddr,
    pub(crate) secret_key: String,
    #[serde(default)]
    pub(crate) web_ui: WebUIConfig,
    #[serde(default)]
    pub(crate) encryption: EncryptionConfig,
    #[serde(default)]
    pub(crate) scheduled_backups: Option<crate::schedule::ScheduleConfig>,
}

#[derive(serde::Deserialize, Debug, Default)]
pub(crate) struct WebUIConfig {
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) path: Option<PathBuf>,
}

#[derive(serde::Deserialize, Debug, Default)]
pub(crate) struct EncryptionConfig {
    #[serde(default)]
    pub(crate) keys: std::collections::BTreeMap<String, std::path::PathBuf>,
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
            let path = config
                .web_ui
                .path
                .as_mut()
                .filter(|path| !path.as_os_str().is_empty())
                .ok_or("web_ui.path must not be empty when web_ui.enabled is true")?;
            *path = working_directory.join(&*path);
            for name in ["index.html", "app.js", "styles.css", "favicon.svg"] {
                let asset = path.join(name);
                let file = std::fs::File::open(&asset).map_err(|error| {
                    format!("cannot open web UI asset {}: {error}", asset.display())
                })?;
                if !file.metadata()?.is_file() {
                    return Err(format!("web UI asset {} must be a file", asset.display()).into());
                }
            }
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDirectory;

    #[test]
    fn web_console_loads_external_paths_only_when_enabled() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let assets = directory.path().join("console");
        std::fs::create_dir(&assets)?;
        for name in ["index.html", "app.js", "styles.css", "favicon.svg"] {
            std::fs::write(assets.join(name), "test")?;
        }
        for web_ui in [
            serde_json::json!({}),
            serde_json::json!({"enabled": false, "path": "missing"}),
            serde_json::json!({"enabled": true, "path": "console"}),
            serde_json::json!({"enabled": true, "path": assets}),
        ] {
            let config = serde_json::json!({
                "listen": "127.0.0.1:0", "secret_key": "test-key", "web_ui": web_ui
            });
            std::fs::write(
                directory.path().join("config.json"),
                serde_json::to_vec(&config)?,
            )?;
            let loaded = Config::load(directory.path())?;
            if loaded.web_ui.enabled {
                assert_eq!(loaded.web_ui.path.as_ref(), Some(&assets));
            }
        }
        for web_ui in [
            serde_json::json!({"enabled": true}),
            serde_json::json!({"enabled": true, "path": ""}),
            serde_json::json!({"enabled": true, "path": "missing"}),
            serde_json::json!({"enabled": true, "path": "console/index.html"}),
        ] {
            let config = serde_json::json!({
                "listen": "127.0.0.1:0", "secret_key": "test-key", "web_ui": web_ui
            });
            std::fs::write(
                directory.path().join("config.json"),
                serde_json::to_vec(&config)?,
            )?;
            assert!(Config::load(directory.path()).is_err());
        }
        Ok(())
    }

    #[test]
    fn configuration_rejects_obsolete_logging_settings() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let mut config: serde_json::Value =
            serde_json::from_str(include_str!("../working-directory-example/config.json"))?;
        config["web_ui"] = serde_json::json!({"enabled": false});
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
