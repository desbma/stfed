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
pub(crate) struct ServerGone {
    /// Inner error
    #[from]
    inner: io::Error,
}

/// Error when server config changed
#[derive(thiserror::Error, Debug)]
pub(crate) enum ServerConfigChanged {
    /// Server initiated config changed notification via event
    #[error("Server sent ConfigSaved event")]
    ConfigSaved,
}

/// Syncthing client used to interact with the Syncthing REST API
pub(crate) struct Client {
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
/// Event types to subscribe to
// TODO subscribe to ItemFinished/FolderSummary only if needed
// Notes:
// DownloadProgress is not emitted for small downloads
// FolderCompletion is for remote device progress
const EVENT_TYPES: &[&str] = &[
    "ItemFinished",
    "FolderSummary",
    "LocalChangeDetected",
    "ConfigSaved",
];

impl Client {
    /// Constructor
    pub(crate) fn new(cfg: &config::Config) -> anyhow::Result<Client> {
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
    pub(crate) fn iter_events(&self) -> FolderEventIterator<'_> {
        FolderEventIterator::new(self)
    }

    /// Send an events request, and return the events it yielded, if any
    fn events(&self, since: u64, timeout: Duration) -> anyhow::Result<Vec<syncthing_rest::Event>> {
        // See https://docs.syncthing.net/dev/events.html
        let mut url = self.base_url.join("rest/events")?;
        url.query_pairs_mut()
            .append_pair("since", &since.to_string())
            .append_pair("limit", "1")
            .append_pair("events", &EVENT_TYPES.join(","))
            .append_pair("timeout", &timeout.as_secs().to_string());
        log::debug!("GET {:?}", url.to_string());
        let response = self
            .session
            .get(url.as_ref())
            .timeout(timeout + HTTP_TIMEOUT)
            .set(HEADER_API_KEY, &self.api_key)
            .call()?
            .into_string();
        let json_str = match response {
            // ureq sends InvalidInput error when socket closes unexpectedly
            Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                return Err(ServerGone { inner: err }.into());
            }
            Err(err) => return Err(err.into()),
            Ok(json_str) => json_str,
        };
        log::trace!("{}", json_str);
        Ok(serde_json::from_str(&json_str)?)
    }

    /// Get the id of the most recent event the server has buffered, if any
    fn latest_event_id(&self) -> anyhow::Result<Option<u64>> {
        // A null timeout returns the buffered events immediately, instead of waiting for a
        // new one when there is none, which would swallow it
        Ok(self.events(0, Duration::ZERO)?.last().map(|evt| evt.id))
    }

    /// Get a single event, no filtering is done at this level
    fn event(&self, since: u64) -> anyhow::Result<syncthing_rest::Event> {
        loop {
            let mut events = self.events(since, REST_TIMEOUT_EVENT_STREAM)?;
            assert!(events.len() <= 1);
            if let Some(event) = events.pop() {
                return Ok(event);
            }
        }
    }
}

/// Iterator of Syncthing events
pub(crate) struct FolderEventIterator<'a> {
    /// API client
    client: &'a Client,
    /// Last event id, `None` until the cursor has been primed
    last_id: Option<u64>,
    /// Last state change for folder to avoid duplicates
    folder_state_change_time: HashMap<String, String>,
}

impl<'a> FolderEventIterator<'a> {
    /// Constructor
    fn new(client: &'a Client) -> Self {
        Self {
            client,
            last_id: None,
            folder_state_change_time: HashMap::new(),
        }
    }
}

impl Iterator for FolderEventIterator<'_> {
    type Item = anyhow::Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let Some(last_id) = self.last_id else {
                // Start after the events the server has already buffered, otherwise polling
                // would first return the most recent of them, and trigger hooks for
                // something that happened before we connected
                match self.client.latest_event_id() {
                    Ok(latest_id) => self.last_id = Some(latest_id.unwrap_or(0)),
                    Err(err) => return Some(Err(err)),
                }
                continue;
            };
            let new_evt_res = self.client.event(last_id);
            return match new_evt_res {
                Ok(new_evt) => {
                    // Update last id
                    self.last_id = Some(new_evt.id);

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
pub(crate) enum Event {
    FileDownSyncDone { path: PathBuf, folder: PathBuf },
    FolderDownSyncDone { folder: PathBuf },
    FileConflict { path: PathBuf, folder: PathBuf },
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{mpsc, Arc, Condvar, Mutex},
        thread,
        time::Instant,
    };

    use serde_json::json;

    use super::*;

    /// Delay to wait for before considering no event was received
    const NO_EVENT_DELAY: Duration = Duration::from_millis(500);

    /// Delay to wait for an event that is expected to be received
    const EVENT_DELAY: Duration = Duration::from_secs(5);

    /// Event buffered by the server
    struct BufferedEvent {
        /// Event type, ie. `ItemFinished`
        event_type: String,
        /// Event type specific payload
        data: serde_json::Value,
    }

    /// Server state, shared with the request handling threads
    #[derive(Default)]
    struct State {
        /// Emitted events, in chronological order
        events: Vec<BufferedEvent>,
        /// Query string of each events request received, in order of reception
        event_requests: Vec<String>,
    }

    impl State {
        /// Events of the subscription selected by `types`, that occurred after `since`
        fn subscription_events(
            &self,
            since: u64,
            limit: Option<usize>,
            types: Option<&[String]>,
        ) -> Vec<serde_json::Value> {
            let mut events = Vec::new();
            // Event ids are numbered per subscription, ie. per distinct event type filter,
            // while globalID is shared by all subscriptions
            let mut subscription_id = 0;
            for (idx, event) in self.events.iter().enumerate() {
                if types.is_some_and(|types| !types.contains(&event.event_type)) {
                    continue;
                }
                subscription_id += 1;
                if subscription_id <= since {
                    continue;
                }
                events.push(json!({
                    "id": subscription_id,
                    "globalID": idx + 1,
                    "type": event.event_type,
                    "time": "2026-01-01T00:00:00Z",
                    "data": event.data,
                }));
            }
            if let Some(limit) = limit {
                // Only the last events are returned
                let over_limit = events.len().saturating_sub(limit);
                events.drain(..over_limit);
            }
            events
        }
    }

    /// Syncthing server, listening on a random port of the loopback interface
    struct TestSyncthingServer {
        /// Server base URL
        url: url::Url,
        /// Event buffer, with a condition variable signaled when an event is emitted
        state: Arc<(Mutex<State>, Condvar)>,
        /// Kept alive to serve requests for as long as the server is used
        _server: Arc<tiny_http::Server>,
    }

    impl TestSyncthingServer {
        /// Start a server exposing the given folders, as `(id, path)` pairs
        fn start(folders: &[(&str, &str)]) -> Self {
            let server = Arc::new(
                tiny_http::Server::http("127.0.0.1:0").expect("Failed to start Syncthing server"),
            );
            let addr = server
                .server_addr()
                .to_ip()
                .expect("Syncthing server has no IP address");
            let url = url::Url::parse(&format!("http://{addr}/")).expect("Invalid server URL");
            let state = Arc::new((Mutex::new(State::default()), Condvar::new()));

            let system_config = json!({
                "folders": folders
                    .iter()
                    .map(|(id, path)| json!({"id": id, "path": path}))
                    .collect::<Vec<_>>(),
            })
            .to_string();

            let server_thread = Arc::clone(&server);
            let state_thread = Arc::clone(&state);
            thread::spawn(move || {
                while let Ok(request) = server_thread.recv() {
                    let request_state = Arc::clone(&state_thread);
                    let request_system_config = system_config.clone();
                    // Serve each request in its own thread, so a long polling events request
                    // does not delay the following ones
                    thread::spawn(move || serve(request, &request_state, &request_system_config));
                }
            });

            Self {
                url,
                state,
                _server: server,
            }
        }

        /// Server base URL
        fn url(&self) -> url::Url {
            self.url.clone()
        }

        /// Emit an event, waking up any pending long polling request
        fn push_event(&self, event_type: &str, data: serde_json::Value) {
            let (state, new_event) = &*self.state;
            state
                .lock()
                .expect("Server state poisoned")
                .events
                .push(BufferedEvent {
                    event_type: event_type.to_owned(),
                    data,
                });
            new_event.notify_all();
        }

        /// Query string of each events request received so far
        fn event_requests(&self) -> Vec<String> {
            let (state, _new_event) = &*self.state;
            state
                .lock()
                .expect("Server state poisoned")
                .event_requests
                .clone()
        }
    }

    /// Serve a single request
    fn serve(request: tiny_http::Request, state: &(Mutex<State>, Condvar), system_config: &str) {
        let url = url::Url::parse("http://localhost/")
            .expect("Invalid base URL")
            .join(request.url())
            .expect("Invalid request URL");
        let body = match url.path() {
            "/rest/system/config" => system_config.to_owned(),
            "/rest/events" => events(state, &url),
            path => panic!("Unexpected request path {path:?}"),
        };
        let content_type =
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                .expect("Invalid header");
        request
            .respond(tiny_http::Response::from_string(body).with_header(content_type))
            .expect("Failed to send response");
    }

    /// Serve an events request, long polling until an event is available or the timeout expires
    fn events(state: &(Mutex<State>, Condvar), url: &url::Url) -> String {
        let (mut since, mut limit, mut types, mut timeout) =
            (0, None, None, Duration::from_secs(60));
        for (key, val) in url.query_pairs() {
            match key.as_ref() {
                "since" => since = val.parse().expect("Invalid since parameter"),
                "limit" => limit = Some(val.parse().expect("Invalid limit parameter")),
                "events" => types = Some(val.split(',').map(str::to_owned).collect::<Vec<_>>()),
                "timeout" => {
                    timeout = Duration::from_secs(val.parse().expect("Invalid timeout parameter"));
                }
                key => panic!("Unexpected query parameter {key:?}"),
            }
        }

        let (state, new_event) = state;
        let mut state = state.lock().expect("Server state poisoned");
        state
            .event_requests
            .push(url.query().unwrap_or_default().to_owned());

        let deadline = Instant::now() + timeout;
        loop {
            let events = state.subscription_events(since, limit, types.as_deref());
            if !events.is_empty() {
                return serde_json::Value::Array(events).to_string();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return "[]".to_owned();
            }
            state = new_event
                .wait_timeout(state, remaining)
                .expect("Server state poisoned")
                .0;
        }
    }

    /// Data payload of an `ItemFinished` event, for a file successfully updated by a sync
    fn item_finished(item: &str, folder: &str) -> serde_json::Value {
        json!({
            "item": item,
            "folder": folder,
            "error": null,
            "type": "file",
            "action": "update",
        })
    }

    /// Stream events in a background thread, to be able to assert on what is received or not
    fn stream_events(client: Client) -> mpsc::Receiver<anyhow::Result<Event>> {
        let (event_tx, event_rx) = mpsc::channel();
        thread::spawn(move || {
            for event in client.iter_events() {
                if event_tx.send(event).is_err() {
                    break;
                }
            }
        });
        event_rx
    }

    /// Events that occurred before we connected must not trigger hooks
    #[test]
    fn no_historical_event_replay_on_startup() {
        let server = TestSyncthingServer::start(&[("fid1", "/data/folder")]);
        server.push_event("ItemFinished", item_finished("old.txt", "fid1"));

        let cfg = config::Config {
            url: server.url(),
            api_key: "apikey".to_owned(),
        };
        let events = stream_events(Client::new(&cfg).expect("Failed to build client"));

        assert!(
            events.recv_timeout(NO_EVENT_DELAY).is_err(),
            "Historical event was replayed"
        );

        // Events occurring while connected are still delivered
        server.push_event("ItemFinished", item_finished("new.txt", "fid1"));
        let event = events
            .recv_timeout(EVENT_DELAY)
            .expect("No event received")
            .expect("Event stream error");
        let Event::FileDownSyncDone { path, folder } = event else {
            panic!("Unexpected event: {event:?}");
        };
        assert_eq!(path, PathBuf::from("new.txt"));
        assert_eq!(folder, PathBuf::from("/data/folder"));

        // The event ids of a subscription are only comparable with those of the same
        // subscription, so the request priming the cursor must use the polled filter
        let requests = server.event_requests();
        assert!(
            requests
                .iter()
                .all(|r| r.contains("events=ItemFinished%2CFolderSummary")),
            "Requests do not all poll the same subscription: {requests:?}"
        );
    }
}
