//! Syncthing related code

use std::{
    collections::{
        VecDeque,
        hash_map::{Entry, HashMap},
    },
    io,
    path::PathBuf,
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
    /// Start time of the server, changing each time it restarts
    start_time: String,
}

/// Position in the event stream of a server instance
#[cfg_attr(test, derive(Debug, Eq, PartialEq))]
pub(crate) struct Cursor {
    /// Start time of the server instance the event ids refer to
    server_start_time: String,
    /// Id of the last consumed event
    last_id: u64,
}

/// API timeout for long event requests
const REST_TIMEOUT_EVENT_STREAM: Duration = Duration::from_secs(60 * 60);
/// HTTP timeout for normal requests
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
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
        let session = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_connect(Some(HTTP_TIMEOUT))
                .user_agent(format!(
                    "{}/{}",
                    env!("CARGO_PKG_NAME"),
                    env!("CARGO_PKG_VERSION")
                ))
                .build(),
        );

        // Get system config to build folder map
        let base_url = cfg.url.clone();
        let system_config: syncthing_rest::SystemConfig = serde_json::from_str(&Self::get(
            &session,
            &base_url.join("rest/system/config")?,
            &cfg.api_key,
        )?)?;
        let system_status: syncthing_rest::SystemStatus = serde_json::from_str(&Self::get(
            &session,
            &base_url.join("rest/system/status")?,
            &cfg.api_key,
        )?)?;

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
            start_time: system_status.start_time,
        })
    }

    /// Send a request to an endpoint, and return the response body
    fn get(session: &ureq::Agent, url: &url::Url, api_key: &str) -> anyhow::Result<String> {
        log::debug!("GET {url:?}", url = url.to_string());
        let json_str = session
            .get(url.as_ref())
            .config()
            .timeout_global(Some(HTTP_TIMEOUT))
            .build()
            .header(HEADER_API_KEY, api_key)
            .call()?
            .into_body()
            .read_to_string()?;
        log::trace!("{json_str}");
        Ok(json_str)
    }

    /// Iterator over infinite stream of events, resuming from `cursor` if set
    pub(crate) fn iter_events(&self, cursor: Option<&Cursor>) -> FolderEventIterator<'_> {
        FolderEventIterator::new(self, self.resume_id(cursor))
    }

    /// Event id to resume the stream from, `None` to start after the events already buffered
    fn resume_id(&self, cursor: Option<&Cursor>) -> Option<u64> {
        let cursor = cursor?;
        if cursor.server_start_time == self.start_time {
            Some(cursor.last_id)
        } else {
            // The server numbers the events of a subscription from scratch when it restarts, so
            // the ids of a previous instance are meaningless: process its whole event buffer
            Some(0)
        }
    }

    /// Send an events request, and return the events it yielded, if any
    fn events(
        &self,
        since: u64,
        limit: Option<usize>,
        timeout: Duration,
    ) -> anyhow::Result<Vec<syncthing_rest::Event>> {
        // See https://docs.syncthing.net/dev/events.html
        let mut url = self.base_url.join("rest/events")?;
        let mut query = url.query_pairs_mut();
        query
            .append_pair("since", &since.to_string())
            .append_pair("events", &EVENT_TYPES.join(","))
            .append_pair("timeout", &timeout.as_secs().to_string());
        if let Some(limit) = limit {
            query.append_pair("limit", &limit.to_string());
        }
        drop(query);
        log::debug!("GET {url:?}", url = url.to_string());
        let response = self
            .session
            .get(url.as_ref())
            .config()
            .timeout_global(Some(timeout + HTTP_TIMEOUT))
            .build()
            .header(HEADER_API_KEY, &self.api_key)
            .call()
            .and_then(|r| r.into_body().read_to_string());
        let json_str = match response {
            // ureq sends an unexpected EOF error when the socket closes, either while waiting
            // for the response of a long polling request, or while reading its body
            Err(ureq::Error::Io(err)) if err.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(ServerGone { inner: err }.into());
            }
            Err(err) => return Err(err.into()),
            Ok(json_str) => json_str,
        };
        log::trace!("{json_str}");
        Ok(serde_json::from_str(&json_str)?)
    }

    /// Get the id of the most recent event the server has buffered, if any
    fn latest_event_id(&self) -> anyhow::Result<Option<u64>> {
        // A null timeout returns the buffered events immediately, instead of waiting for a
        // new one when there is none, which would swallow it
        Ok(self
            .events(0, Some(1), Duration::ZERO)?
            .last()
            .map(|evt| evt.id))
    }
}

/// Iterator of Syncthing events
pub(crate) struct FolderEventIterator<'a> {
    /// API client
    client: &'a Client,
    /// Id of the last consumed event, `None` until the cursor has been primed
    last_id: Option<u64>,
    /// Events received from the server, not yet consumed
    pending: VecDeque<syncthing_rest::Event>,
    /// Last state change for folder to avoid duplicates
    folder_state_change_time: HashMap<String, String>,
}

impl<'a> FolderEventIterator<'a> {
    /// Constructor
    fn new(client: &'a Client, resume_id: Option<u64>) -> Self {
        Self {
            client,
            last_id: resume_id,
            pending: VecDeque::new(),
            folder_state_change_time: HashMap::new(),
        }
    }

    /// Position reached in the event stream, to resume it after a reconnection
    pub(crate) fn cursor(&self) -> Option<Cursor> {
        self.last_id.map(|last_id| Cursor {
            server_start_time: self.client.start_time.clone(),
            last_id,
        })
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
            let Some(new_evt) = self.pending.pop_front() else {
                // Fetch all the events that occurred since the last one, otherwise a burst of
                // events would be truncated to its most recent one, and the others lost
                match self.client.events(last_id, None, REST_TIMEOUT_EVENT_STREAM) {
                    Ok(events) => self.pending.extend(events),
                    Err(err) => return Some(Err(err)),
                }
                continue;
            };

            // Update last id
            self.last_id = Some(new_evt.id);

            return match new_evt.data {
                // The server emits this event for each item the sync processed, whatever the
                // outcome: a failed sync left no usable file, and a deletion or a metadata
                // change synced no content
                syncthing_rest::EventData::ItemFinished(syncthing_rest::ItemFinishedEvent {
                    item,
                    folder,
                    error: None,
                    item_type,
                    action: syncthing_rest::ItemAction::Update,
                }) if item_type == "file" => {
                    // The folder may have been removed from the server config while its
                    // events were still buffered
                    let Some(folder_path) = self.client.folder_map.get(&folder) else {
                        log::warn!("Ignoring event for unknown folder id {folder:?}");
                        continue;
                    };
                    Some(Ok(Event::FileDownSyncDone {
                        path: PathBuf::from(item),
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
                    let Some(folder_path) = self.client.folder_map.get(&evt_data.folder) else {
                        log::warn!(
                            "Ignoring event for unknown folder id {folder:?}",
                            folder = evt_data.folder
                        );
                        continue;
                    };
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
                        let Some(folder_path) = self.client.folder_map.get(&evt_data.folder) else {
                            log::warn!(
                                "Ignoring event for unknown folder id {folder:?}",
                                folder = evt_data.folder
                            );
                            continue;
                        };
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
                _ => continue,
            };
        }
    }
}

/// Syncthing event, see `config::FolderEvent` for meaning of each event
#[derive(Debug)]
#[cfg_attr(test, derive(Eq, PartialEq))]
pub(crate) enum Event {
    /// See `config::FolderEvent::FileDownSyncDone`
    FileDownSyncDone {
        /// Path of the file, relative to the folder
        path: PathBuf,
        /// Local path of the folder
        folder: PathBuf,
    },
    /// See `config::FolderEvent::FolderDownSyncDone`
    FolderDownSyncDone {
        /// Local path of the folder
        folder: PathBuf,
    },
    /// See `config::FolderEvent::FileConflict`
    FileConflict {
        /// Path of the conflict file, relative to the folder
        path: PathBuf,
        /// Local path of the folder
        folder: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use std::{
        iter,
        net::{Shutdown, TcpListener, TcpStream},
        sync::{Arc, Condvar, Mutex, mpsc},
        thread,
        time::Instant,
    };

    use serde_json::json;

    use super::*;

    /// Delay to wait for before considering no event was received
    const NO_EVENT_DELAY: Duration = Duration::from_millis(500);

    /// Delay to wait for an event that is expected to be received
    const EVENT_DELAY: Duration = Duration::from_secs(5);

    /// Start time of the server the tests connect to
    const SERVER_START_TIME: &str = "2026-07-11T12:00:00Z";

    /// Start time of the server instance a previous connection was made to
    const PREVIOUS_SERVER_START_TIME: &str = "2026-07-11T11:00:00Z";

    /// Id of the folder the tests sync
    const FOLDER_ID: &str = "fid1";

    /// Local path of the folder the tests sync
    const FOLDER_PATH: &str = "/data/folder";

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
        /// Server state, with a condition variable signaled when it changes
        state: Arc<(Mutex<State>, Condvar)>,
        /// Kept alive to serve requests for as long as the server is used
        _server: Arc<tiny_http::Server>,
    }

    impl TestSyncthingServer {
        /// Start a server exposing the given folders, as `(id, path)` pairs
        fn start(folders: &[(&str, &str)]) -> Self {
            let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
            let addr = server.server_addr().to_ip().unwrap();
            let url = url::Url::parse(&format!("http://{addr}/")).unwrap();
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
            self.push_events(&[(event_type, data)]);
        }

        /// Emit events, buffering them all before waking up any pending long polling request
        fn push_events(&self, events: &[(&str, serde_json::Value)]) {
            let (state, state_changed) = &*self.state;
            let mut state = state.lock().unwrap();
            for (event_type, data) in events {
                state.events.push(BufferedEvent {
                    event_type: (*event_type).to_owned(),
                    data: data.clone(),
                });
            }
            drop(state);
            state_changed.notify_all();
        }

        /// Wait until the server has received `count` events requests
        fn wait_event_requests(&self, count: usize) {
            let (state, state_changed) = &*self.state;
            let mut state = state.lock().unwrap();
            while state.event_requests.len() < count {
                let (new_state, wait) = state_changed.wait_timeout(state, EVENT_DELAY).unwrap();
                assert!(!wait.timed_out(), "Missing events requests");
                state = new_state;
            }
        }

        /// Query string of each events request received so far
        fn event_requests(&self) -> Vec<String> {
            let (state, _state_changed) = &*self.state;
            state.lock().unwrap().event_requests.clone()
        }
    }

    /// TCP relay in front of a server, able to close the connections it forwards
    struct VanishingRelay {
        /// Relay base URL
        url: url::Url,
        /// Client side of each forwarded connection
        connections: Arc<Mutex<Vec<TcpStream>>>,
    }

    impl VanishingRelay {
        /// Start a relay forwarding to the server at `target`
        fn start(target: &url::Url) -> Self {
            let target = format!("{}:{}", target.host_str().unwrap(), target.port().unwrap());
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let url = url::Url::parse(&format!("http://{addr}/")).unwrap();
            let connections: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));

            let connections_thread = Arc::clone(&connections);
            thread::spawn(move || {
                for client in listener.incoming() {
                    let client = client.unwrap();
                    let server = TcpStream::connect(&target).unwrap();
                    let clone = |s: &TcpStream| s.try_clone().unwrap();
                    connections_thread.lock().unwrap().push(clone(&client));
                    for (mut from, mut to) in [(clone(&client), clone(&server)), (server, client)] {
                        thread::spawn(move || io::copy(&mut from, &mut to));
                    }
                }
            });

            Self { url, connections }
        }

        /// Relay base URL
        fn url(&self) -> url::Url {
            self.url.clone()
        }

        /// Close the forwarded connections, as a server going away does
        fn vanish(&self) {
            for connection in self.connections.lock().unwrap().drain(..) {
                connection.shutdown(Shutdown::Both).unwrap();
            }
        }
    }

    /// Serve a single request
    fn serve(request: tiny_http::Request, state: &(Mutex<State>, Condvar), system_config: &str) {
        let url = url::Url::parse("http://localhost/")
            .unwrap()
            .join(request.url())
            .unwrap();
        let body = match url.path() {
            "/rest/system/config" => system_config.to_owned(),
            "/rest/system/status" => json!({
                "myID": "TESTDEV-ICEID",
                "startTime": SERVER_START_TIME,
            })
            .to_string(),
            "/rest/events" => events(state, &url),
            path => panic!("Unexpected request path {path:?}"),
        };
        let content_type =
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        request
            .respond(tiny_http::Response::from_string(body).with_header(content_type))
            .unwrap();
    }

    /// Serve an events request, long polling until an event is available or the timeout expires
    fn events(state: &(Mutex<State>, Condvar), url: &url::Url) -> String {
        let (mut since, mut limit, mut types, mut timeout) =
            (0, None, None, Duration::from_secs(60));
        for (key, val) in url.query_pairs() {
            match key.as_ref() {
                "since" => since = val.parse().unwrap(),
                "limit" => limit = Some(val.parse().unwrap()),
                "events" => types = Some(val.split(',').map(str::to_owned).collect::<Vec<_>>()),
                "timeout" => {
                    timeout = Duration::from_secs(val.parse().unwrap());
                }
                key => panic!("Unexpected query parameter {key:?}"),
            }
        }

        let (state, state_changed) = state;
        let mut state = state.lock().unwrap();
        state
            .event_requests
            .push(url.query().unwrap_or_default().to_owned());
        state_changed.notify_all();

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
            state = state_changed.wait_timeout(state, remaining).unwrap().0;
        }
    }

    /// Data payload of an `ItemFinished` event
    fn item_finished_data(
        item: &str,
        folder: &str,
        error: Option<&str>,
        item_type: &str,
        action: &str,
    ) -> serde_json::Value {
        json!({
            "item": item,
            "folder": folder,
            "error": error,
            "type": item_type,
            "action": action,
        })
    }

    /// Data payload of an `ItemFinished` event, for a file successfully updated by a sync
    fn item_finished(item: &str, folder: &str) -> serde_json::Value {
        item_finished_data(item, folder, None, "file", "update")
    }

    /// Data payload of a `FolderSummary` event
    fn folder_summary(
        folder: &str,
        need_total_items: u64,
        state_changed: &str,
    ) -> serde_json::Value {
        json!({
            "folder": folder,
            "summary": {
                "globalBytes": 1024,
                "globalDeleted": 0,
                "globalDirectories": 1,
                "globalFiles": 3,
                "globalSymlinks": 0,
                "globalTotalItems": 4,
                "ignorePatterns": false,
                "inSyncBytes": 1024,
                "inSyncFiles": 3,
                "localBytes": 1024,
                "localDeleted": 0,
                "localDirectories": 1,
                "localFiles": 3,
                "localSymlinks": 0,
                "localTotalItems": 4,
                "needBytes": 0,
                "needDeletes": 0,
                "needDirectories": 0,
                "needFiles": 0,
                "needSymlinks": 0,
                "needTotalItems": need_total_items,
                "pullErrors": 0,
                "sequence": 100,
                "state": "idle",
                "stateChanged": state_changed,
                "version": 100,
            },
        })
    }

    /// Data payload of a `LocalChangeDetected` event
    fn local_change(path: &str, folder: &str, item_type: &str, action: &str) -> serde_json::Value {
        json!({
            "action": action,
            "folder": folder,
            "label": "Folder",
            "path": path,
            "type": item_type,
        })
    }

    /// Stream events in a background thread, to be able to assert on what is received or not
    fn stream_events(
        client: Client,
        cursor: Option<Cursor>,
    ) -> mpsc::Receiver<anyhow::Result<Event>> {
        let (event_tx, event_rx) = mpsc::channel();
        thread::spawn(move || {
            for event in client.iter_events(cursor.as_ref()) {
                if event_tx.send(event).is_err() {
                    break;
                }
            }
        });
        event_rx
    }

    /// Position left in the event stream by a connection to the server instance started at
    /// `server_start_time`
    fn cursor(server_start_time: &str, last_id: u64) -> Cursor {
        Cursor {
            server_start_time: server_start_time.to_owned(),
            last_id,
        }
    }

    /// Client connected to the server reachable at `url`
    fn connect(url: url::Url) -> Client {
        let cfg = config::Config {
            url,
            api_key: "apikey".to_owned(),
        };
        Client::new(&cfg).unwrap()
    }

    /// Consume `count` events of the stream
    fn recv_events(events: &mpsc::Receiver<anyhow::Result<Event>>, count: usize) -> Vec<Event> {
        iter::repeat_with(|| events.recv_timeout(EVENT_DELAY).unwrap().unwrap())
            .take(count)
            .collect()
    }

    /// Event of a file successfully synced down in the folder of the tests
    fn file_down_sync_done(item: &str) -> Event {
        Event::FileDownSyncDone {
            path: PathBuf::from(item),
            folder: PathBuf::from(FOLDER_PATH),
        }
    }

    /// Event of the folder of the tests fully synced down
    fn folder_down_sync_done() -> Event {
        Event::FolderDownSyncDone {
            folder: PathBuf::from(FOLDER_PATH),
        }
    }

    /// Events that occurred before we connected must not trigger hooks
    #[test]
    fn no_historical_event_replay_on_startup() {
        let server = TestSyncthingServer::start(&[(FOLDER_ID, FOLDER_PATH)]);
        server.push_event("ItemFinished", item_finished("old.txt", FOLDER_ID));

        let events = stream_events(connect(server.url()), None);

        assert!(events.recv_timeout(NO_EVENT_DELAY).is_err());

        // Events occurring while connected are still delivered
        server.push_event("ItemFinished", item_finished("new.txt", FOLDER_ID));
        assert_eq!(recv_events(&events, 1), [file_down_sync_done("new.txt")]);

        // The event ids of a subscription are only comparable with those of the same
        // subscription, so the request priming the cursor must use the polled filter
        assert!(
            server
                .event_requests()
                .iter()
                .all(|r| r.contains("events=ItemFinished%2CFolderSummary"))
        );
    }

    /// Events buffered by the server between two polls must all be delivered
    #[test]
    fn no_event_loss_on_burst() {
        let server = TestSyncthingServer::start(&[(FOLDER_ID, FOLDER_PATH)]);

        let events = stream_events(connect(server.url()), None);

        // Wait for the request priming the cursor and the first polling one, so that the
        // events are not mistaken for events that occurred before we connected
        server.wait_event_requests(2);
        let items = ["1.txt", "2.txt", "3.txt"];
        server.push_events(&items.map(|item| ("ItemFinished", item_finished(item, FOLDER_ID))));

        assert_eq!(
            recv_events(&events, items.len()),
            items.map(file_down_sync_done)
        );
    }

    /// Events that occurred while disconnected must be processed when the connection is back
    #[test]
    fn resume_event_stream_after_reconnection() {
        let server = TestSyncthingServer::start(&[(FOLDER_ID, FOLDER_PATH)]);
        let items = ["1.txt", "2.txt", "3.txt"];
        server.push_events(&items.map(|item| ("ItemFinished", item_finished(item, FOLDER_ID))));

        // The previous connection consumed the first event before it was lost
        let events = stream_events(connect(server.url()), Some(cursor(SERVER_START_TIME, 1)));

        assert_eq!(
            recv_events(&events, 2),
            ["2.txt", "3.txt"].map(file_down_sync_done)
        );
    }

    /// An item the sync did not update as a local file must not be reported as synced down
    #[test]
    fn ignore_item_finished_of_non_updated_file() {
        let server = TestSyncthingServer::start(&[(FOLDER_ID, FOLDER_PATH)]);

        let events = stream_events(connect(server.url()), None);

        server.wait_event_requests(2);
        server.push_events(&[
            (
                "ItemFinished",
                item_finished_data(
                    "failed.txt",
                    FOLDER_ID,
                    Some("no space left"),
                    "file",
                    "update",
                ),
            ),
            (
                "ItemFinished",
                item_finished_data("deleted.txt", FOLDER_ID, None, "file", "delete"),
            ),
            (
                "ItemFinished",
                item_finished_data("chmod.txt", FOLDER_ID, None, "file", "metadata"),
            ),
            (
                "ItemFinished",
                item_finished_data("subdir", FOLDER_ID, None, "dir", "update"),
            ),
            ("ItemFinished", item_finished("ok.txt", FOLDER_ID)),
        ]);

        assert_eq!(recv_events(&events, 1), [file_down_sync_done("ok.txt")]);
        assert!(events.recv_timeout(NO_EVENT_DELAY).is_err());
    }

    /// A server closing the connection must be reported as gone, so the main loop reconnects
    #[test]
    fn server_gone_on_connection_close() {
        let server = TestSyncthingServer::start(&[(FOLDER_ID, FOLDER_PATH)]);
        let relay = VanishingRelay::start(&server.url());

        let events = stream_events(connect(relay.url()), None);

        // Wait for the request priming the cursor and the first polling one, so the connection
        // is closed while a long polling request is pending
        server.wait_event_requests(2);
        relay.vanish();

        let err = events.recv_timeout(EVENT_DELAY).unwrap().unwrap_err();
        assert!(err.downcast_ref::<ServerGone>().is_some());
    }

    /// A summary of a folder that still needs items must not be reported as synced down
    #[test]
    fn ignore_incomplete_folder_summary() {
        let server = TestSyncthingServer::start(&[(FOLDER_ID, FOLDER_PATH)]);

        let events = stream_events(connect(server.url()), None);

        server.wait_event_requests(2);
        server.push_event(
            "FolderSummary",
            folder_summary(FOLDER_ID, 2, "2026-01-01T00:00:01Z"),
        );
        assert!(events.recv_timeout(NO_EVENT_DELAY).is_err());

        server.push_event(
            "FolderSummary",
            folder_summary(FOLDER_ID, 0, "2026-01-01T00:00:02Z"),
        );
        assert_eq!(recv_events(&events, 1), [folder_down_sync_done()]);
    }

    /// A summary re-sent for the same state change must be reported only once
    #[test]
    fn ignore_duplicate_folder_summary() {
        let server = TestSyncthingServer::start(&[(FOLDER_ID, FOLDER_PATH)]);

        let events = stream_events(connect(server.url()), None);

        server.wait_event_requests(2);
        server.push_event(
            "FolderSummary",
            folder_summary(FOLDER_ID, 0, "2026-01-01T00:00:01Z"),
        );
        assert_eq!(recv_events(&events, 1), [folder_down_sync_done()]);

        server.push_event(
            "FolderSummary",
            folder_summary(FOLDER_ID, 0, "2026-01-01T00:00:01Z"),
        );
        assert!(events.recv_timeout(NO_EVENT_DELAY).is_err());

        server.push_event(
            "FolderSummary",
            folder_summary(FOLDER_ID, 0, "2026-01-01T00:00:02Z"),
        );
        assert_eq!(recv_events(&events, 1), [folder_down_sync_done()]);
    }

    /// Only the modification of a conflict file must be reported as a local conflict
    #[test]
    fn file_conflict_on_conflict_file_modification() {
        let server = TestSyncthingServer::start(&[(FOLDER_ID, FOLDER_PATH)]);

        let events = stream_events(connect(server.url()), None);

        server.wait_event_requests(2);
        let conflict_path = "doc.sync-conflict-20260101-000000-AAAAAAA.txt";
        server.push_events(&[
            (
                "LocalChangeDetected",
                local_change("doc.txt", FOLDER_ID, "file", "modified"),
            ),
            (
                "LocalChangeDetected",
                local_change(conflict_path, FOLDER_ID, "dir", "modified"),
            ),
            (
                "LocalChangeDetected",
                local_change(conflict_path, FOLDER_ID, "file", "deleted"),
            ),
            (
                "LocalChangeDetected",
                local_change(conflict_path, FOLDER_ID, "file", "modified"),
            ),
        ]);

        assert_eq!(
            recv_events(&events, 1),
            [Event::FileConflict {
                path: PathBuf::from(conflict_path),
                folder: PathBuf::from(FOLDER_PATH),
            }]
        );
        assert!(events.recv_timeout(NO_EVENT_DELAY).is_err());
    }

    /// An event referencing a folder absent from the server config, as when a folder is
    /// removed while its events are still buffered, must be skipped instead of crashing
    #[test]
    fn ignore_event_of_unknown_folder() {
        let server = TestSyncthingServer::start(&[(FOLDER_ID, FOLDER_PATH)]);

        let events = stream_events(connect(server.url()), None);

        server.wait_event_requests(2);
        server.push_events(&[
            ("ItemFinished", item_finished("gone.txt", "removedfid")),
            (
                "FolderSummary",
                folder_summary("removedfid", 0, "2026-01-01T00:00:01Z"),
            ),
            (
                "LocalChangeDetected",
                local_change(
                    "doc.sync-conflict-20260101-000000-AAAAAAA.txt",
                    "removedfid",
                    "file",
                    "modified",
                ),
            ),
            ("ItemFinished", item_finished("kept.txt", FOLDER_ID)),
        ]);

        assert_eq!(recv_events(&events, 1), [file_down_sync_done("kept.txt")]);
    }

    /// A server config change must interrupt the stream, so the folder map is rebuilt
    #[test]
    fn server_config_changed_on_config_saved() {
        let server = TestSyncthingServer::start(&[(FOLDER_ID, FOLDER_PATH)]);

        let events = stream_events(connect(server.url()), None);

        server.wait_event_requests(2);
        server.push_event("ConfigSaved", json!({"version": 2}));

        let err = events.recv_timeout(EVENT_DELAY).unwrap().unwrap_err();
        assert!(err.downcast_ref::<ServerConfigChanged>().is_some());
    }

    /// The cursor must be unset until primed, then track the last consumed event
    #[test]
    fn cursor_tracks_stream_position() {
        let server = TestSyncthingServer::start(&[(FOLDER_ID, FOLDER_PATH)]);
        server.push_event("ItemFinished", item_finished("1.txt", FOLDER_ID));

        let client = connect(server.url());
        assert!(client.iter_events(None).cursor().is_none());

        let mut events = client.iter_events(Some(&cursor(SERVER_START_TIME, 0)));
        assert_eq!(
            events.next().unwrap().unwrap(),
            file_down_sync_done("1.txt")
        );
        assert_eq!(events.cursor(), Some(cursor(SERVER_START_TIME, 1)));
    }

    /// A server that restarted numbers its events from scratch, its whole buffer must be processed
    #[test]
    fn restart_event_stream_after_server_restart() {
        let server = TestSyncthingServer::start(&[(FOLDER_ID, FOLDER_PATH)]);
        let items = ["1.txt", "2.txt"];
        server.push_events(&items.map(|item| ("ItemFinished", item_finished(item, FOLDER_ID))));

        // The previous connection was to another server instance, whose ids are unrelated to
        // those of this one, even when they are within the range of its event buffer
        let events = stream_events(
            connect(server.url()),
            Some(cursor(PREVIOUS_SERVER_START_TIME, 1)),
        );

        assert_eq!(
            recv_events(&events, items.len()),
            items.map(file_down_sync_done)
        );
    }
}
