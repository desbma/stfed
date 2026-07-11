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
}

/// API timeout for long event requests
const REST_TIMEOUT_EVENT_STREAM: Duration = Duration::from_secs(60 * 60);
/// API timeout for the cursor priming event request, which must not long poll
const REST_TIMEOUT_EVENT_PRIME: Duration = Duration::from_secs(1);
/// HTTP timeout for normal requests
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// HTTP timeout for long event requests
static HTTP_TIMEOUT_EVENT_STREAM: LazyLock<Duration> =
    LazyLock::new(|| REST_TIMEOUT_EVENT_STREAM + HTTP_TIMEOUT);
/// Header key value for Synthing API key
const HEADER_API_KEY: &str = "X-API-Key";
/// Event types we subscribe to.
/// The server numbers event ids per subscription (one subscription per distinct filter),
/// so every events request must use this exact filter for `since` values to be comparable.
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

    /// Iterator over infinite stream of events.
    /// `resume_cursor` is the cursor of a previous connection if any, so that events
    /// that occurred while disconnected are not lost.
    pub(crate) fn iter_events(&self, resume_cursor: Option<u64>) -> FolderEventIterator<'_> {
        FolderEventIterator::new(self, resume_cursor)
    }

    /// Send a request to the events endpoint and return the raw JSON response.
    /// See <https://docs.syncthing.net/rest/events-get.html>
    fn events_request(
        &self,
        since: u64,
        limit: Option<usize>,
        timeout: Duration,
    ) -> anyhow::Result<String> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("Invalid URL {}", self.base_url))?
            .push("rest")
            .push("events");
        url.query_pairs_mut()
            .append_pair("since", &since.to_string())
            .append_pair("events", &EVENT_TYPES.join(","))
            .append_pair("timeout", &timeout.as_secs().to_string());
        if let Some(limit) = limit {
            url.query_pairs_mut()
                .append_pair("limit", &limit.to_string());
        }
        log::debug!("GET {:?}", url.to_string());
        let response = self
            .session
            .get(url.as_ref())
            .timeout(timeout + HTTP_TIMEOUT)
            .set(HEADER_API_KEY, &self.api_key)
            .call()?
            .into_string();
        match response {
            // ureq sends InvalidInput error when socket closes unexpectedly
            Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                Err(ServerGone { inner: err }.into())
            }
            Err(err) => Err(err.into()),
            Ok(json_str) => {
                log::trace!("{}", json_str);
                Ok(json_str)
            }
        }
    }

    /// Get the id of the most recent event already buffered for our subscription, if any.
    /// Used to prime the event cursor, so that historical events are never replayed.
    fn latest_event_id(&self) -> anyhow::Result<Option<u64>> {
        // since=0&limit=1 returns the most recent historical event; short timeout because
        // an empty buffer means there is nothing to skip, no point long polling
        let json_str = self.events_request(0, Some(1), REST_TIMEOUT_EVENT_PRIME)?;
        // Parse minimally: only the id matters here
        let events: Vec<serde_json::Value> = serde_json::from_str(&json_str)?;
        Ok(events
            .last()
            .and_then(|e| e.get("id"))
            .and_then(serde_json::Value::as_u64))
    }

    /// Get the next batch of events, in chronological order.
    /// No filtering is done at this level.
    fn events(&self, since: u64) -> anyhow::Result<Vec<syncthing_rest::Event>> {
        loop {
            let json_str = self.events_request(since, None, REST_TIMEOUT_EVENT_STREAM)?;
            let events: Vec<syncthing_rest::Event> = serde_json::from_str(&json_str)?;
            if !events.is_empty() {
                return Ok(events);
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
    /// Cursor of a previous connection to resume from, if any
    resume_cursor: Option<u64>,
    /// Events fetched but not yet processed
    pending: VecDeque<syncthing_rest::Event>,
    /// Last state change for folder to avoid duplicates
    folder_state_change_time: HashMap<String, String>,
}

impl<'a> FolderEventIterator<'a> {
    /// Constructor
    fn new(client: &'a Client, resume_cursor: Option<u64>) -> Self {
        Self {
            client,
            last_id: None,
            resume_cursor,
            pending: VecDeque::new(),
            folder_state_change_time: HashMap::new(),
        }
    }

    /// Current event cursor, to pass back to `iter_events` when reconnecting
    pub(crate) fn cursor(&self) -> Option<u64> {
        self.last_id.or(self.resume_cursor)
    }

    /// Compute the cursor position to start polling from
    fn prime_cursor(&self) -> anyhow::Result<u64> {
        let latest = self.client.latest_event_id()?;
        Ok(match (self.resume_cursor, latest) {
            // Normal reconnection: resume where we left off, catching up on events
            // that occurred while we were disconnected
            (Some(cursor), Some(latest)) if cursor <= latest => cursor,
            // Our cursor is ahead of the server's counter: the server restarted and
            // event ids were reset, everything buffered is new
            (Some(_), _) => 0,
            // First connection: start after the most recent historical event, hooks
            // must not fire for things that happened before startup
            (None, latest) => latest.unwrap_or(0),
        })
    }
}

impl Iterator for FolderEventIterator<'_> {
    type Item = anyhow::Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // Prime the cursor on the first call
            let last_id = if let Some(id) = self.last_id {
                id
            } else {
                let id = match self.prime_cursor() {
                    Ok(id) => id,
                    Err(err) => return Some(Err(err)),
                };
                self.last_id = Some(id);
                id
            };

            // Take the next fetched event, or fetch a new batch
            let Some(new_evt) = self.pending.pop_front() else {
                match self.client.events(last_id) {
                    Ok(events) => {
                        self.pending.extend(events);
                        continue;
                    }
                    Err(err) => return Some(Err(err)),
                }
            };

            // Update last id
            self.last_id = Some(new_evt.id);

            return match new_evt.data {
                syncthing_rest::EventData::ItemFinished(evt_data) => {
                    // The event is also emitted for failed syncs, deletions and
                    // metadata changes: only a successful content update means a
                    // file was synced down
                    if evt_data.error.is_some()
                        || !matches!(evt_data.action, syncthing_rest::ItemAction::Update)
                    {
                        continue;
                    }
                    let Some(folder_path) = self.client.folder_map.get(&evt_data.folder) else {
                        log::warn!("Ignoring event for unknown folder id {:?}", evt_data.folder);
                        continue;
                    };
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
                    let Some(folder_path) = self.client.folder_map.get(&evt_data.folder) else {
                        log::warn!("Ignoring event for unknown folder id {:?}", evt_data.folder);
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
                                "Ignoring event for unknown folder id {:?}",
                                evt_data.folder
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
                other => {
                    log::warn!("Ignoring unexpected event: {:?}", other);
                    continue;
                }
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
        collections::VecDeque,
        io::{BufRead as _, BufReader, Write as _},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };

    use super::*;

    /// Canned `/rest/system/config` response with a single folder
    const SYSTEM_CONFIG: &str = r#"{"folders": [{"id": "fid1", "path": "/data/folder"}]}"#;

    /// Minimal HTTP server serving canned JSON responses in order, recording request URLs
    struct MockServer {
        /// Server base URL
        url: url::Url,
        /// Request paths + query strings, in order of reception
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl MockServer {
        /// Start a server that will serve the given JSON response bodies, in order
        fn start(responses: &[&str]) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url =
                url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let server_requests = Arc::clone(&requests);
            let mut pending: VecDeque<String> = responses.iter().map(|r| (*r).to_owned()).collect();
            thread::spawn(move || {
                while !pending.is_empty() {
                    let Ok((stream, _addr)) = listener.accept() else {
                        break;
                    };
                    // Read request line + headers, GET requests have no body
                    let mut reader = BufReader::new(stream);
                    let mut request_line = String::new();
                    reader.read_line(&mut request_line).unwrap();
                    let target = request_line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    loop {
                        let mut header_line = String::new();
                        reader.read_line(&mut header_line).unwrap();
                        if header_line.trim_end().is_empty() {
                            break;
                        }
                    }
                    server_requests.lock().unwrap().push(target);
                    let body = pending.pop_front().unwrap();
                    let mut writer = reader.into_inner();
                    write!(
                        writer,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .unwrap();
                }
            });
            Self { url, requests }
        }

        /// Requests received so far (path + query string)
        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    /// Create a client connected to the mock server, consuming its first canned response
    fn test_client(server: &MockServer) -> Client {
        let cfg = config::Config {
            url: server.url.clone(),
            api_key: "apikey".to_owned(),
        };
        Client::new(&cfg).unwrap()
    }

    /// Build a single raw `ItemFinished` event JSON object
    fn item_finished_event(
        id: u64,
        item: &str,
        folder: &str,
        error: Option<&str>,
        action: &str,
    ) -> String {
        let error_json = error.map_or_else(|| "null".to_owned(), |e| format!("\"{e}\""));
        format!(
            r#"{{"id": {id}, "globalID": {id}, "type": "ItemFinished", "time": "2026-01-01T00:00:00Z", "data": {{"item": "{item}", "folder": "{folder}", "error": {error_json}, "type": "file", "action": "{action}"}}}}"#
        )
    }

    /// Wrap raw event JSON objects into a `/rest/events` response body
    fn events_response(events: &[String]) -> String {
        format!("[{}]", events.join(","))
    }

    /// The most recent historical event must not be replayed when the event stream starts:
    /// the cursor is primed from the latest event id of our subscription
    #[test]
    fn no_historical_event_replay_on_startup() {
        let historical = events_response(&[item_finished_event(
            42,
            "oldfile.txt",
            "fid1",
            None,
            "update",
        )]);
        let new_event = events_response(&[item_finished_event(
            43,
            "newfile.txt",
            "fid1",
            None,
            "update",
        )]);
        let server = MockServer::start(&[SYSTEM_CONFIG, &historical, &new_event]);
        let client = test_client(&server);

        let event = client.iter_events(None).next().unwrap().unwrap();

        match event {
            Event::FileDownSyncDone { path, folder } => {
                assert_eq!(path, PathBuf::from("newfile.txt"));
                assert_eq!(folder, PathBuf::from("/data/folder"));
            }
            other => panic!("Unexpected event: {other:?}"),
        }
        let requests = server.requests();
        assert_eq!(requests.len(), 3);
        // Priming request skips over history, and uses the same event type filter as
        // regular polling (event ids are numbered per subscription)
        assert!(requests[1].contains("since=0") && requests[1].contains("limit=1"));
        assert!(requests[1].contains("events=ItemFinished"));
        // Regular polling starts after the latest historical event
        assert!(requests[2].contains("since=42"));
        assert!(requests[2].contains("events=ItemFinished"));
    }

    /// Reconnecting with a resume cursor must fetch the events that occurred while
    /// disconnected, instead of priming past them
    #[test]
    fn resume_cursor_catches_up_missed_events() {
        // Priming probe sees the newest buffered event (id 44) but the cursor (42) is
        // still valid, so polling must resume from it and deliver both missed events
        let latest = events_response(&[item_finished_event(
            44,
            "missed2.txt",
            "fid1",
            None,
            "update",
        )]);
        let missed = events_response(&[
            item_finished_event(43, "missed1.txt", "fid1", None, "update"),
            item_finished_event(44, "missed2.txt", "fid1", None, "update"),
        ]);
        let server = MockServer::start(&[SYSTEM_CONFIG, &latest, &missed]);
        let client = test_client(&server);
        let mut events = client.iter_events(Some(42));

        match events.next().unwrap().unwrap() {
            Event::FileDownSyncDone { path, .. } => {
                assert_eq!(path, PathBuf::from("missed1.txt"));
            }
            other => panic!("Unexpected event: {other:?}"),
        }
        match events.next().unwrap().unwrap() {
            Event::FileDownSyncDone { path, .. } => {
                assert_eq!(path, PathBuf::from("missed2.txt"));
            }
            other => panic!("Unexpected event: {other:?}"),
        }
        assert_eq!(events.cursor(), Some(44));
        let requests = server.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests[2].contains("since=42"));
    }

    /// A resume cursor ahead of the server's event counter means the server restarted
    /// and event ids were reset: everything buffered is new and must be processed
    #[test]
    fn resume_cursor_detects_server_restart() {
        let latest = events_response(&[item_finished_event(
            2,
            "after-restart2.txt",
            "fid1",
            None,
            "update",
        )]);
        let buffered = events_response(&[
            item_finished_event(1, "after-restart1.txt", "fid1", None, "update"),
            item_finished_event(2, "after-restart2.txt", "fid1", None, "update"),
        ]);
        let server = MockServer::start(&[SYSTEM_CONFIG, &latest, &buffered]);
        let client = test_client(&server);
        let mut events = client.iter_events(Some(500));

        match events.next().unwrap().unwrap() {
            Event::FileDownSyncDone { path, .. } => {
                assert_eq!(path, PathBuf::from("after-restart1.txt"));
            }
            other => panic!("Unexpected event: {other:?}"),
        }
        let requests = server.requests();
        assert!(requests[2].contains("since=0"));
    }

    /// `ItemFinished` events for failed syncs, deletions or metadata changes must not
    /// trigger `FileDownSyncDone`: only a successful content update does
    #[test]
    fn item_finished_only_fires_for_successful_updates() {
        let historical = "[]";
        let failed = events_response(&[item_finished_event(
            1,
            "failed.txt",
            "fid1",
            Some("generic error"),
            "update",
        )]);
        let deleted = events_response(&[item_finished_event(
            2,
            "deleted.txt",
            "fid1",
            None,
            "delete",
        )]);
        let metadata = events_response(&[item_finished_event(
            3,
            "metadata.txt",
            "fid1",
            None,
            "metadata",
        )]);
        let updated = events_response(&[item_finished_event(
            4,
            "updated.txt",
            "fid1",
            None,
            "update",
        )]);
        let server = MockServer::start(&[
            SYSTEM_CONFIG,
            historical,
            &failed,
            &deleted,
            &metadata,
            &updated,
        ]);
        let client = test_client(&server);

        let event = client.iter_events(None).next().unwrap().unwrap();

        match event {
            Event::FileDownSyncDone { path, folder } => {
                assert_eq!(path, PathBuf::from("updated.txt"));
                assert_eq!(folder, PathBuf::from("/data/folder"));
            }
            other => panic!("Unexpected event: {other:?}"),
        }
    }

    /// An event referencing a folder id unknown to us must be skipped, not panic
    #[test]
    fn unknown_folder_id_is_skipped() {
        let unknown = events_response(&[item_finished_event(
            43,
            "elsewhere.txt",
            "unknownfid",
            None,
            "update",
        )]);
        let known = events_response(&[item_finished_event(44, "here.txt", "fid1", None, "update")]);
        let server = MockServer::start(&[SYSTEM_CONFIG, "[]", &unknown, &known]);
        let client = test_client(&server);

        let event = client.iter_events(None).next().unwrap().unwrap();

        match event {
            Event::FileDownSyncDone { path, .. } => {
                assert_eq!(path, PathBuf::from("here.txt"));
            }
            other => panic!("Unexpected event: {other:?}"),
        }
    }

    /// All events of a batched response must be processed, in order, without re-fetching
    /// (the server sends every buffered event since the cursor in one response)
    #[test]
    fn batched_events_are_all_processed() {
        let batch = events_response(&[
            item_finished_event(43, "first.txt", "fid1", None, "update"),
            item_finished_event(44, "second.txt", "fid1", None, "update"),
        ]);
        let server = MockServer::start(&[SYSTEM_CONFIG, "[]", &batch]);
        let client = test_client(&server);
        let mut events = client.iter_events(None);

        match events.next().unwrap().unwrap() {
            Event::FileDownSyncDone { path, .. } => {
                assert_eq!(path, PathBuf::from("first.txt"));
            }
            other => panic!("Unexpected event: {other:?}"),
        }
        match events.next().unwrap().unwrap() {
            Event::FileDownSyncDone { path, .. } => {
                assert_eq!(path, PathBuf::from("second.txt"));
            }
            other => panic!("Unexpected event: {other:?}"),
        }
        // Both events came from a single request
        assert_eq!(server.requests().len(), 3);
        assert_eq!(events.cursor(), Some(44));
    }

    /// An event type we can parse but do not handle must be skipped, not panic
    #[test]
    fn unhandled_event_type_is_skipped() {
        let started = r#"[{"id": 43, "globalID": 43, "type": "ItemStarted", "time": "2026-01-01T00:00:00Z", "data": {"item": "started.txt", "folder": "fid1", "type": "file", "action": "update"}}]"#;
        let finished =
            events_response(&[item_finished_event(44, "done.txt", "fid1", None, "update")]);
        let server = MockServer::start(&[SYSTEM_CONFIG, "[]", started, &finished]);
        let client = test_client(&server);

        let event = client.iter_events(None).next().unwrap().unwrap();

        match event {
            Event::FileDownSyncDone { path, .. } => {
                assert_eq!(path, PathBuf::from("done.txt"));
            }
            other => panic!("Unexpected event: {other:?}"),
        }
    }
}
