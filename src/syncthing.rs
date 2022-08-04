//! Syncthing related code

use std::collections::hash_map::{Entry, HashMap};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crate::config;
use crate::syncthing_rest;

/// Error when server vanished
#[derive(thiserror::Error, Debug)]
#[error(transparent)]
pub struct ServerGone {
    /// Inner error
    #[from]
    inner: io::Error,
}

/// Error when server config changed
#[derive(thiserror::Error, Debug)]
pub enum ServerConfigChanged {
    /// Server initiated config changed notification via event
    #[error("Server sent ConfigSaved event")]
    ConfigSaved,
}

/// Syncthing client used to interact with the Syncthing REST API
pub struct SyncthingClient {
    /// Syncthing URL
    base_url: url::Url,
    /// API key
    api_key: String,
    /// HTTP session
    session: ureq::Agent,
    /// Folder id to path
    folder_map: HashMap<String, PathBuf>,
}

/// HTTP timeout for long event requests
const EVENT_STREAM_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// HTTP timeout for other requests
const REST_TIMEOUT: Duration = Duration::from_secs(10);
/// Header key value for Synthing API key
const HEADER_API_KEY: &str = "X-API-Key";

impl SyncthingClient {
    /// Constructor
    pub fn new(cfg: &config::Config) -> anyhow::Result<SyncthingClient> {
        // Build session
        let session = ureq::AgentBuilder::new()
            .timeout_connect(REST_TIMEOUT)
            .timeout_read(EVENT_STREAM_TIMEOUT)
            .timeout_write(REST_TIMEOUT)
            .user_agent(&format!(
                "{}/{}",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION")
            ))
            .build();

        // Get system config to build folder map
        let base_url = cfg.url.to_owned();
        let url = base_url.join("rest/system/config")?;
        log::debug!("GET {:?}", url);
        let json_str = session
            .get(url.as_ref())
            .timeout(REST_TIMEOUT)
            .set(HEADER_API_KEY, &cfg.api_key)
            .call()?
            .into_string()?;
        log::trace!("{}", json_str);
        let system_config: syncthing_rest::SystemConfig = serde_json::from_str(&json_str)?;

        // Build folder map
        let folder_map = system_config
            .folders
            .into_iter()
            .map(|f| (f.id, PathBuf::from(f.path)))
            .collect();

        Ok(Self {
            base_url,
            session,
            api_key: cfg.api_key.to_owned(),
            folder_map,
        })
    }

    /// Iterator over infinite stream of events
    pub fn iter_events(&self) -> SyncthingFolderEventIterator {
        SyncthingFolderEventIterator::new(self)
    }

    /// Get a single event, no filtering is done at this level
    fn event(&self, since: u64, evt_types: &[&str]) -> anyhow::Result<syncthing_rest::Event> {
        // See https://docs.syncthing.net/dev/events.html
        let mut url = self.base_url.to_owned();
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Invalid URL {}", self.base_url))?
            .push("rest")
            .push("events");
        url.query_pairs_mut()
            .append_pair("since", &since.to_string())
            .append_pair("limit", "1")
            .append_pair("events", &evt_types.join(","));
        url.query_pairs_mut()
            .append_pair("timeout", &EVENT_STREAM_TIMEOUT.as_secs().to_string());
        loop {
            log::debug!("GET {:?}", url.to_string());
            let response = self
                .session
                .get(url.as_ref())
                .set(HEADER_API_KEY, &self.api_key)
                .call()?
                .into_string();
            let json_str = match response {
                // ureq sends InvalidInput error when socket closes unexpectedly
                Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                    return Err(ServerGone { inner: err }.into());
                }
                Err(_) => response?,
                Ok(resp) => resp,
            };
            log::trace!("{}", json_str);
            let mut events: Vec<syncthing_rest::Event> = serde_json::from_str(&json_str)?;
            assert!(events.len() <= 1);
            if let Some(event) = events.pop() {
                return Ok(event);
            }
        }
    }
}

/// Iterator of Syncthing events
pub struct SyncthingFolderEventIterator<'a> {
    /// API client
    client: &'a SyncthingClient,
    /// Last event id
    last_id: u64,
    /// Last state change for folder to avoid duplicates
    folder_state_change_time: HashMap<String, String>,
}

impl<'a> SyncthingFolderEventIterator<'a> {
    /// Constructor
    fn new(client: &'a SyncthingClient) -> SyncthingFolderEventIterator {
        Self {
            client,
            last_id: 0,
            folder_state_change_time: HashMap::new(),
        }
    }
}

impl Iterator for SyncthingFolderEventIterator<'_> {
    type Item = anyhow::Result<SyncthingEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // TODO subscribe to ItemFinished/FolderSummary only if needed
            // Notes:
            // DownloadProgress is not emitted for small downloads
            // FolderCompletion is for remote device progress
            let new_evt_res = self.client.event(
                self.last_id,
                &[
                    "ItemFinished",
                    "FolderSummary",
                    "LocalChangeDetected",
                    "ConfigSaved",
                ],
            );
            return match new_evt_res {
                Ok(new_evt) => {
                    // Update last id
                    self.last_id = new_evt.id;

                    match new_evt.data {
                        syncthing_rest::EventData::ItemFinished(evt_data) => {
                            let folder_path = self
                                .client
                                .folder_map
                                .get(&evt_data.folder)
                                .expect("Unknown folder id");
                            Some(Ok(SyncthingEvent::FileDownSyncDone {
                                path: PathBuf::from(evt_data.item),
                                folder: folder_path.to_owned(),
                            }))
                        }
                        syncthing_rest::EventData::FolderSummary(evt_data) => {
                            if evt_data.summary.need_total_items > 0 {
                                // Not complete
                                continue;
                            }
                            let changed = evt_data.summary.state_changed;
                            match self.folder_state_change_time.entry(evt_data.folder.clone()) {
                                Entry::Occupied(mut e) => {
                                    if e.get() == &changed {
                                        // Duplicate event
                                        continue;
                                    }
                                    e.insert(changed);
                                }
                                Entry::Vacant(e) => {
                                    e.insert(changed);
                                }
                            }
                            let folder_path = self
                                .client
                                .folder_map
                                .get(&evt_data.folder)
                                .expect("Unknown folder id");
                            Some(Ok(SyncthingEvent::FolderDownSyncDone {
                                folder: folder_path.to_owned(),
                            }))
                        }
                        syncthing_rest::EventData::LocalChangeDetected(evt_data) => {
                            // see https://github.com/syncthing/syncthing/issues/6121#issuecomment-549077477
                            if (evt_data.item_type == "file")
                                && (evt_data.action == "modified")
                                && (evt_data.path.contains(".sync-conflict-"))
                            {
                                let folder_path = self
                                    .client
                                    .folder_map
                                    .get(&evt_data.folder)
                                    .expect("Unknown folder id");
                                Some(Ok(SyncthingEvent::FileConflict {
                                    path: PathBuf::from(evt_data.path),
                                    folder: folder_path.to_owned(),
                                }))
                            } else {
                                continue;
                            }
                        }
                        syncthing_rest::EventData::ConfigSaved(_) => {
                            Some(Err(ServerConfigChanged::ConfigSaved.into()))
                        }
                        _ => unimplemented!(),
                    }
                }

                // Propagate error
                Err(e) => Some(Err(e)),
            };
        }
    }
}

/// Syncthing event, see config::FolderEvent for meaning of each event
#[allow(clippy::missing_docs_in_private_items)]
#[derive(Debug)]
pub enum SyncthingEvent {
    FileDownSyncDone { path: PathBuf, folder: PathBuf },
    FolderDownSyncDone { folder: PathBuf },
    FileConflict { path: PathBuf, folder: PathBuf },
}
