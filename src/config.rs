//! Local configuration

use std::{fs, path::PathBuf};

use anyhow::Context;
use serde::de::Deserialize;

/// Local configuration
#[derive(Debug, serde::Deserialize)]
pub struct Config {
    /// Syncthing base URL
    pub url: url::Url,
    /// Syncthing API key
    pub api_key: String,
}

/// Root local Syncthing configuration
#[derive(serde::Deserialize)]
struct SyncthingXmlConfig {
    /// "GUI" configuration part, whatever that means
    gui: SyncthingXmlConfigGui,
}

/// GUI local Syncthing configuration
#[derive(serde::Deserialize)]
struct SyncthingXmlConfigGui {
    /// Listening address
    address: String,
    /// API key
    apikey: String,
}

impl Config {
    /// Try to generate a valid default configuration from the local Syncthing configuration
    fn default_from_syncthing_config() -> anyhow::Result<Self> {
        // Read Syncthing config to get address & API key
        let xdg_dirs = xdg::BaseDirectories::with_prefix("syncthing")
            .context("Unable fo find Synthing config directory")?;
        let st_config_filepath = xdg_dirs
            .find_state_file("config.xml")
            .or_else(|| xdg_dirs.find_config_file("config.xml"))
            .context("Unable fo find Synthing config file")?;
        log::debug!("Found Syncthing config in {:?}", st_config_filepath);
        let st_config_xml = fs::read_to_string(st_config_filepath)?;
        let st_config: SyncthingXmlConfig = quick_xml::de::from_str(&st_config_xml)?;

        Ok(Self {
            url: url::Url::parse(&format!("http://{}", st_config.gui.address))?,
            api_key: st_config.gui.apikey,
        })
    }
}

impl Default for Config {
    fn default() -> Self {
        #[expect(clippy::unwrap_used)]
        Self::default_from_syncthing_config()
            .context(format!(
                "Unable to guess {} configuration field values from Synthing config, \
                 please write a config file",
                env!("CARGO_PKG_NAME")
            ))
            .unwrap()
    }
}

/// Folder hooks configurations
#[expect(clippy::module_name_repetitions)]
#[derive(Debug, serde::Deserialize)]
pub struct FolderConfig {
    /// Hooks array
    pub hooks: Vec<FolderHook>,
}

/// Configuration for a folder hook
#[derive(Clone, Debug, serde::Deserialize)]
pub struct FolderHook {
    /// Absolute path of the folder
    pub folder: PathBuf,
    /// Event to hook
    pub event: FolderEvent,
    /// Event filter
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_glob")]
    pub filter: Option<globset::GlobMatcher>,
    /// Command
    #[serde(deserialize_with = "deserialize_command")]
    pub command: Vec<String>,
    /// Allow concurrent runs for the same hook
    pub allow_concurrent: Option<bool>,
}

/// Deserialize filter into a glob matcher to validate glob expression
fn deserialize_glob<'de, D>(deserializer: D) -> Result<Option<globset::GlobMatcher>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    opt.map(|s| {
        globset::GlobBuilder::new(&s)
            .literal_separator(true)
            .build()
            .map(|g| g.compile_matcher())
            .map_err(serde::de::Error::custom)
    })
    .transpose()
}

/// Deserialize command string into a vec directly usable by `std::Command`
fn deserialize_command<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    shlex::split(&s).ok_or_else(|| serde::de::Error::custom(format!("Invalid command: {s:?}")))
}

/// Folder event kind
#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderEvent {
    /// A whole folder has been synced down
    FolderDownSyncDone,
    /// A file has been synced down
    FileDownSyncDone,
    /// A conflict has occured locally
    FileConflict,
    /// A conflict has occured remotely
    RemoteFileConflict,
}

/// Parse local configuration
pub fn parse() -> anyhow::Result<(Config, FolderConfig)> {
    let binary_name = env!("CARGO_PKG_NAME");
    let xdg_dirs = xdg::BaseDirectories::with_prefix(binary_name)?;
    let config_filepath = xdg_dirs.find_config_file("config.toml");

    let config = if let Some(config_filepath) = config_filepath {
        log::debug!("Config filepath: {:?}", config_filepath);

        let toml_data = fs::read_to_string(config_filepath)?;
        log::trace!("Config data: {:?}", toml_data);

        toml::from_str(&toml_data)?
    } else {
        log::warn!("Unable to find config file, using default config");
        Config::default()
    };

    log::trace!("Config: {:?}", config);

    let hooks_filepath = xdg_dirs
        .find_config_file("hooks.toml")
        .ok_or_else(|| anyhow::anyhow!("Unable to find hooks file"))?;
    log::debug!("Hooks filepath: {:?}", hooks_filepath);

    let toml_data = fs::read_to_string(hooks_filepath)?;
    log::trace!("Hooks data: {:?}", toml_data);
    let hooks = toml::from_str(&toml_data)?;

    log::trace!("Hooks: {:?}", hooks);

    Ok((config, hooks))
}
