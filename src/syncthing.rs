//! Syncthing related code

use std::{
    collections::{
        hash_map::{Entry, HashMap},
        VecDeque,
    },
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
    /// Start time of the server, changing each time it restarts
    start_time: String,
}

/// Position in the event stream of a server instance
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
        log::debug!("GET {:?}", url.to_string());
        let json_str = session
            .get(url.as_ref())
            .timeout(HTTP_TIMEOUT)
            .set(HEADER_API_KEY, api_key)
            .call()?
            .into_string()?;
        log::trace!("{}", json_str);
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
                    let folder_path = self
                        .client
                        .folder_map
                        .get(&folder)
                        .expect("Unknown folder id");
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
                _ => continue,
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
        iter,
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

    /// Start time of the server the tests connect to
    const SERVER_START_TIME: &str = "2026-07-11T12:00:00Z";

    /// Start time of the server instance a previous connection was made to
    const PREVIOUS_SERVER_START_TIME: &str = "2026-07-11T11:00:00Z";

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
            self.push_events(&[(event_type, data)]);
        }

        /// Emit events, buffering them all before waking up any pending long polling request
        fn push_events(&self, events: &[(&str, serde_json::Value)]) {
            let (state, state_changed) = &*self.state;
            let mut state = state.lock().expect("Server state poisoned");
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
            let mut state = state.lock().expect("Server state poisoned");
            while state.event_requests.len() < count {
                let (new_state, wait) = state_changed
                    .wait_timeout(state, EVENT_DELAY)
                    .expect("Server state poisoned");
                assert!(!wait.timed_out(), "Missing events requests");
                state = new_state;
            }
        }

        /// Query string of each events request received so far
        fn event_requests(&self) -> Vec<String> {
            let (state, _state_changed) = &*self.state;
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
            "/rest/system/status" => json!({
                "myID": "TESTDEV-ICEID",
                "startTime": SERVER_START_TIME,
            })
            .to_string(),
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

        let (state, state_changed) = state;
        let mut state = state.lock().expect("Server state poisoned");
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
            state = state_changed
                .wait_timeout(state, remaining)
                .expect("Server state poisoned")
                .0;
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

    /// Client connected to a server, with the folder of the tests already set up
    fn connect(server: &TestSyncthingServer) -> Client {
        let cfg = config::Config {
            url: server.url(),
            api_key: "apikey".to_owned(),
        };
        Client::new(&cfg).expect("Failed to build client")
    }

    /// Consume the events of a file sync, and return the path of each synced file
    fn recv_synced_files(
        events: &mpsc::Receiver<anyhow::Result<Event>>,
        count: usize,
    ) -> Vec<PathBuf> {
        iter::repeat_with(|| {
            let event = events
                .recv_timeout(EVENT_DELAY)
                .expect("No event received")
                .expect("Event stream error");
            let Event::FileDownSyncDone { path, .. } = event else {
                panic!("Unexpected event: {event:?}");
            };
            path
        })
        .take(count)
        .collect()
    }

    /// Events that occurred before we connected must not trigger hooks
    #[test]
    fn no_historical_event_replay_on_startup() {
        let server = TestSyncthingServer::start(&[("fid1", "/data/folder")]);
        server.push_event("ItemFinished", item_finished("old.txt", "fid1"));

        let events = stream_events(connect(&server), None);

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

    /// Events buffered by the server between two polls must all be delivered
    #[test]
    fn no_event_loss_on_burst() {
        let server = TestSyncthingServer::start(&[("fid1", "/data/folder")]);

        let events = stream_events(connect(&server), None);

        // Wait for the request priming the cursor and the first polling one, so that the
        // events are not mistaken for events that occurred before we connected
        server.wait_event_requests(2);
        let items = ["1.txt", "2.txt", "3.txt"];
        server.push_events(&items.map(|item| ("ItemFinished", item_finished(item, "fid1"))));

        assert_eq!(
            recv_synced_files(&events, items.len()),
            items.map(PathBuf::from)
        );
    }

    /// Events that occurred while disconnected must be processed when the connection is back
    #[test]
    fn resume_event_stream_after_reconnection() {
        let server = TestSyncthingServer::start(&[("fid1", "/data/folder")]);
        let items = ["1.txt", "2.txt", "3.txt"];
        server.push_events(&items.map(|item| ("ItemFinished", item_finished(item, "fid1"))));

        // The previous connection consumed the first event before it was lost
        let events = stream_events(connect(&server), Some(cursor(SERVER_START_TIME, 1)));

        assert_eq!(
            recv_synced_files(&events, 2),
            ["2.txt", "3.txt"].map(PathBuf::from)
        );
    }

    /// An item the sync did not update as a local file must not be reported as synced down
    #[test]
    fn ignore_item_finished_of_non_updated_file() {
        let server = TestSyncthingServer::start(&[("fid1", "/data/folder")]);

        let events = stream_events(connect(&server), None);

        server.wait_event_requests(2);
        server.push_events(&[
            (
                "ItemFinished",
                item_finished_data(
                    "failed.txt",
                    "fid1",
                    Some("no space left"),
                    "file",
                    "update",
                ),
            ),
            (
                "ItemFinished",
                item_finished_data("deleted.txt", "fid1", None, "file", "delete"),
            ),
            (
                "ItemFinished",
                item_finished_data("chmod.txt", "fid1", None, "file", "metadata"),
            ),
            (
                "ItemFinished",
                item_finished_data("subdir", "fid1", None, "dir", "update"),
            ),
            ("ItemFinished", item_finished("ok.txt", "fid1")),
        ]);

        assert_eq!(recv_synced_files(&events, 1), [PathBuf::from("ok.txt")]);
        assert!(
            events.recv_timeout(NO_EVENT_DELAY).is_err(),
            "Unexpected event"
        );
    }

    /// A server that restarted numbers its events from scratch, its whole buffer must be processed
    #[test]
    fn restart_event_stream_after_server_restart() {
        let server = TestSyncthingServer::start(&[("fid1", "/data/folder")]);
        let items = ["1.txt", "2.txt"];
        server.push_events(&items.map(|item| ("ItemFinished", item_finished(item, "fid1"))));

        // The previous connection was to another server instance, whose ids are unrelated to
        // those of this one, even when they are within the range of its event buffer
        let events = stream_events(
            connect(&server),
            Some(cursor(PREVIOUS_SERVER_START_TIME, 1)),
        );

        assert_eq!(
            recv_synced_files(&events, items.len()),
            items.map(PathBuf::from)
        );
    }
}
