//! Local configuration

use std::{
    fs, io,
    ops::Deref,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use serde::de::Deserialize as _;
use simple_expand_tilde::expand_tilde;

/// Local configuration
#[derive(Debug, serde::Deserialize)]
pub(crate) struct Config {
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
        let xdg_dirs = xdg::BaseDirectories::with_prefix("syncthing");
        let st_config_filepath = xdg_dirs
            .find_state_file("config.xml")
            .or_else(|| xdg_dirs.find_config_file("config.xml"))
            .context("Unable fo find Synthing config file")?;
        log::debug!("Found Syncthing config in {st_config_filepath:?}");
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
            .with_context(|| {
                format!(
                    "Unable to guess {} configuration field values from Synthing config, \
                 please write a config file",
                    env!("CARGO_PKG_NAME")
                )
            })
            .unwrap()
    }
}

/// Folder hooks configurations
#[derive(Debug, serde::Deserialize)]
#[serde(from = "RawFolderConfig")]
pub(crate) struct FolderConfig {
    /// Hooks array
    pub hooks: Vec<FolderHook>,
}

/// Folder hooks configurations as deserialized, before hook indexes are assigned
#[derive(serde::Deserialize)]
struct RawFolderConfig {
    /// Hooks array
    hooks: Vec<FolderHook>,
}

impl From<RawFolderConfig> for FolderConfig {
    fn from(raw: RawFolderConfig) -> Self {
        let mut hooks = raw.hooks;
        // Assign hook indexes during deserialization so that every parsing path gets
        // unique ones, they identify each hook at runtime
        for (index, hook) in hooks.iter_mut().enumerate() {
            hook.index = index;
        }
        Self { hooks }
    }
}

/// Path string with ~ replaced, and canonicalized
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct NormalizedPath(PathBuf);

impl<'de> serde::Deserialize<'de> for NormalizedPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let pb = PathBuf::deserialize(deserializer)?;
        pb.as_path().try_into().map_err(serde::de::Error::custom)
    }
}

impl TryFrom<&Path> for NormalizedPath {
    type Error = io::Error;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        let path = expand_tilde(path)
            .ok_or_else(|| io::Error::other("User not found"))?
            .canonicalize()?;
        Ok(Self(path))
    }
}

impl Deref for NormalizedPath {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.0.as_path()
    }
}

/// Configuration for a folder hook
#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct FolderHook {
    /// Absolute path of the folder
    pub folder: NormalizedPath,
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
    /// Unique hook index, assigned when parsing, used to identify the hook at runtime
    #[serde(skip)]
    pub index: usize,
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
pub(crate) enum FolderEvent {
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
pub(crate) fn parse() -> anyhow::Result<(Config, FolderConfig)> {
    let binary_name = env!("CARGO_PKG_NAME");
    let xdg_dirs = xdg::BaseDirectories::with_prefix(binary_name);
    let config_filepath = xdg_dirs.find_config_file("config.toml");

    let config = if let Some(config_filepath) = config_filepath {
        log::debug!("Config filepath: {config_filepath:?}");

        let toml_data = fs::read_to_string(config_filepath)?;
        log::trace!("Config data: {toml_data:?}");

        toml::from_str(&toml_data)?
    } else {
        log::warn!("Unable to find config file, using default config");
        Config::default()
    };

    log::trace!("Config: {config:?}");

    let hooks_filepath = xdg_dirs
        .find_config_file("hooks.toml")
        .ok_or_else(|| anyhow::anyhow!("Unable to find hooks file"))?;
    log::debug!("Hooks filepath: {hooks_filepath:?}");

    let toml_data = fs::read_to_string(hooks_filepath)?;
    log::trace!("Hooks data: {toml_data:?}");
    let hooks = toml::from_str(&toml_data)?;

    log::trace!("Hooks: {hooks:?}");

    Ok((config, hooks))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    /// A hooks document, exercising all supported keys
    #[test]
    fn parse_hooks_document() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().canonicalize().unwrap();
        let toml_data = format!(
            r#"
            [[hooks]]
            folder = "{folder}"
            event = "file_down_sync_done"
            filter = "*.pdf"
            command = "notify-send 'stfed event' body"

            [[hooks]]
            folder = "{folder}"
            event = "remote_file_conflict"
            command = "true"
            allow_concurrent = true
            "#,
            folder = folder.to_str().unwrap()
        );

        let hooks: FolderConfig = toml::from_str(&toml_data).unwrap();

        assert_eq!(hooks.hooks.len(), 2);
        assert_eq!(hooks.hooks[0].folder, NormalizedPath(folder.clone()));
        assert_eq!(hooks.hooks[0].event, FolderEvent::FileDownSyncDone);
        assert_eq!(
            hooks.hooks[0].command,
            ["notify-send", "stfed event", "body"]
        );
        assert_eq!(hooks.hooks[0].allow_concurrent, None);
        assert!(hooks.hooks[0].filter.is_some());
        assert_eq!(hooks.hooks[1].folder, NormalizedPath(folder));
        assert_eq!(hooks.hooks[1].event, FolderEvent::RemoteFileConflict);
        assert_eq!(hooks.hooks[1].command, ["true"]);
        assert_eq!(hooks.hooks[1].allow_concurrent, Some(true));
        assert!(hooks.hooks[1].filter.is_none());
    }

    /// Filter glob wildcards must not cross directory separators
    #[test]
    fn filter_glob_does_not_cross_directories() {
        let dir = tempfile::tempdir().unwrap();
        let toml_data = format!(
            r#"
            [[hooks]]
            folder = "{folder}"
            event = "file_down_sync_done"
            filter = "*.pdf"
            command = "true"
            "#,
            folder = dir.path().to_str().unwrap()
        );

        let hooks: FolderConfig = toml::from_str(&toml_data).unwrap();

        let filter = hooks.hooks[0].filter.as_ref().unwrap();
        assert!(filter.is_match("report.pdf"));
        assert!(!filter.is_match("sub/report.pdf"));
    }

    /// An unparseable command string must be rejected when parsing hooks
    #[test]
    fn reject_invalid_command() {
        let dir = tempfile::tempdir().unwrap();
        let toml_data = format!(
            r#"
            [[hooks]]
            folder = "{folder}"
            event = "file_down_sync_done"
            command = "notify-send 'unbalanced"
            "#,
            folder = dir.path().to_str().unwrap()
        );

        assert!(toml::from_str::<FolderConfig>(&toml_data).is_err());
    }

    /// An invalid filter glob must be rejected when parsing hooks
    #[test]
    fn reject_invalid_filter_glob() {
        let dir = tempfile::tempdir().unwrap();
        let toml_data = format!(
            r#"
            [[hooks]]
            folder = "{folder}"
            event = "file_down_sync_done"
            filter = "[oops"
            command = "true"
            "#,
            folder = dir.path().to_str().unwrap()
        );

        assert!(toml::from_str::<FolderConfig>(&toml_data).is_err());
    }

    /// Normalization must resolve symlinks and relative components
    #[test]
    fn normalized_path_canonicalizes() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        symlink(&real, &link).unwrap();
        let real = real.canonicalize().unwrap();

        assert_eq!(
            NormalizedPath::try_from(link.as_path()).unwrap(),
            NormalizedPath(real.clone())
        );
        assert_eq!(
            NormalizedPath::try_from(dir.path().join("real/../real").as_path()).unwrap(),
            NormalizedPath(real)
        );
    }

    /// A path that does not exist must be rejected
    #[test]
    fn normalized_path_rejects_missing_path() {
        let dir = tempfile::tempdir().unwrap();

        assert!(NormalizedPath::try_from(dir.path().join("missing").as_path()).is_err());
    }

    /// Address and API key must be extracted from a local Syncthing configuration
    #[test]
    fn parse_syncthing_xml_config() {
        let xml = r#"
            <configuration version="37">
                <folder id="fid1" label="Folder" path="/data/folder" type="sendreceive">
                    <device id="AAAAAAA-AAAAAAA" introducedBy=""></device>
                </folder>
                <gui enabled="true" tls="false" debugging="false">
                    <address>127.0.0.1:8384</address>
                    <apikey>abcdefgh123456</apikey>
                    <theme>default</theme>
                </gui>
                <options>
                    <listenAddress>default</listenAddress>
                </options>
            </configuration>
            "#;

        let config: SyncthingXmlConfig = quick_xml::de::from_str(xml).unwrap();

        assert_eq!(config.gui.address, "127.0.0.1:8384");
        assert_eq!(config.gui.apikey, "abcdefgh123456");
    }
}
