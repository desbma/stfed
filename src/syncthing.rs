//! Syncthing related code

use std::{
    collections::hash_map::{Entry, HashMap},
    io,
    path::PathBuf,
    sync::LazyLock,
    time::Duration,
};

use crate::{config, syncthing_rest};

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
pub struct Client {
    /// Syncthing URL
    base_url: url::Url,
    /// API key
    api_key: String,
    /// HTTP session
    session: ureq::Agent,
    /// Folder id to path
    folder_map: HashMap<String, PathBuf>,
}

/// API timeout for long event requests
const REST_TIMEOUT_EVENT_STREAM: Duration = Duration::from_secs(60 * 60);
/// HTTP timeout for normal requests
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// HTTP timeout for long event requests
static HTTP_TIMEOUT_EVENT_STREAM: LazyLock<Duration> =
    LazyLock::new(|| REST_TIMEOUT_EVENT_STREAM + HTTP_TIMEOUT);
/// Header key value for Synthing API key
const HEADER_API_KEY: &str = "X-API-Key";

impl Client {
    /// Constructor
    pub fn new(cfg: &config::Config) -> anyhow::Result<Client> {
        // Build session
        let session = ureq::AgentBuilder::new()
            .timeout_connect(HTTP_TIMEOUT)
            .timeout_read(*HTTP_TIMEOUT_EVENT_STREAM)
            .timeout_write(HTTP_TIMEOUT)
            .user_agent(&format!(
                "{}/{}",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION")
            ))
            .build();

        // Get system config to build folder map
        let base_url = cfg.url.clone();
        let url = base_url.join("rest/system/config")?;
        log::debug!("GET {:?}", url);
        let json_str = session
            .get(url.as_ref())
            .timeout(HTTP_TIMEOUT)
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
            api_key: cfg.api_key.clone(),
            folder_map,
        })
    }

    /// Iterator over infinite stream of events
    pub fn iter_events(&self) -> FolderEventIterator {
        FolderEventIterator::new(self)
    }

    /// Get a single event, no filtering is done at this level
    fn event(&self, since: u64, evt_types: &[&str]) -> anyhow::Result<syncthing_rest::Event> {
        // See https://docs.syncthing.net/dev/events.html
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("Invalid URL {}", self.base_url))?
            .push("rest")
            .push("events");
        url.query_pairs_mut()
            .append_pair("since", &since.to_string())
            .append_pair("limit", "1")
            .append_pair("events", &evt_types.join(","));
        url.query_pairs_mut()
            .append_pair("timeout", &REST_TIMEOUT_EVENT_STREAM.as_secs().to_string());
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
pub struct FolderEventIterator<'a> {
    /// API client
    client: &'a Client,
    /// Last event id
    last_id: u64,
    /// Last state change for folder to avoid duplicates
    folder_state_change_time: HashMap<String, String>,
}

impl<'a> FolderEventIterator<'a> {
    /// Constructor
    fn new(client: &'a Client) -> Self {
        Self {
            client,
            last_id: 0,
            folder_state_change_time: HashMap::new(),
        }
    }
}

impl Iterator for FolderEventIterator<'_> {
    type Item = anyhow::Result<Event>;

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
                            Some(Ok(Event::FileDownSyncDone {
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
                            Some(Ok(Event::FolderDownSyncDone {
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
                                Some(Ok(Event::FileConflict {
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

/// Syncthing event, see `config::FolderEvent` for meaning of each event
#[expect(clippy::missing_docs_in_private_items)]
#[derive(Debug)]
pub enum Event {
    FileDownSyncDone { path: PathBuf, folder: PathBuf },
    FolderDownSyncDone { folder: PathBuf },
    FileConflict { path: PathBuf, folder: PathBuf },
}
