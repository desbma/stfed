//! Syncthing types, taken from <https://github.com/JayceFayne/syncthing-rs/blob/e5981950d59c210a380c0665a4e7a4b44f7ce37f/src/rest/events.rs>, with some additional fixes

#![allow(
    dead_code,
    clippy::enum_glob_use,
    clippy::missing_docs_in_private_items
)]

use std::{collections::HashMap, convert::TryFrom};

use serde::{Deserialize, Serialize};

//
// Events
//

type FileName = String;
type DeviceID = String;
type FolderName = String;
type Folder = HashMap<FileName, File>;

#[derive(Debug, Deserialize)]
#[serde(rename_all(deserialize = "camelCase"))]
pub(crate) struct File {
    pub total: u64,
    pub pulling: u64,
    pub copied_from_origin: u64,
    pub reused: u64,
    pub copied_from_elsewhere: u64,
    pub pulled: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ConfigSavedEvent {
    pub version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all(deserialize = "camelCase"))]
pub(crate) struct DeviceConnectedEvent {
    pub addr: String,
    #[serde(rename = "id")]
    pub device_id: DeviceID,
    pub device_name: String,
    pub client_name: String,
    pub client_version: String,
    #[serde(rename = "type")]
    pub client_type: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeviceDisconnectedEvent {
    #[serde(rename = "id")]
    pub device_id: DeviceID,
    pub error: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeviceDiscoveredEvent {
    #[serde(rename = "device")]
    pub device_id: DeviceID,
    pub addrs: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DevicePausedEvent {
    #[serde(rename = "device")]
    pub device_id: DeviceID,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeviceRejectedEvent {
    #[serde(rename = "device")]
    pub device_id: DeviceID,
    pub name: String,
    pub address: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeviceResumedEvent {
    #[serde(rename = "device")]
    pub device_id: DeviceID,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all(deserialize = "camelCase"))]
pub(crate) struct FolderCompletionEvent {
    #[serde(rename = "device")]
    pub device_id: DeviceID,
    #[serde(rename = "folder")]
    pub folder_id: String,
    pub completion: f64,
    pub global_bytes: u64,
    pub need_bytes: u64,
    pub need_deletes: u64,
    pub need_items: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FolderErrorsEvent {
    pub folder: String,
    pub errors: Vec<FolderError>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FolderError {
    pub error: String,
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FolderRejectedEvent {
    #[serde(rename = "device")]
    pub device_id: DeviceID,
    #[serde(rename = "folder")]
    pub folder_id: String,
    #[serde(rename = "folderLabel")]
    pub folder_label: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FolderScanProgressEvent {
    pub total: u64,
    pub rate: u64,
    pub current: u64,
    #[serde(rename = "folder")]
    pub folder_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FolderSummaryEvent {
    pub folder: String,
    pub summary: FolderSummaryData,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all(deserialize = "camelCase"))]
pub(crate) struct FolderSummaryData {
    pub global_bytes: u64,
    pub global_deleted: u64,
    pub global_directories: u64,
    pub global_files: u64,
    pub global_symlinks: u64,
    pub global_total_items: u64,
    pub ignore_patterns: bool,
    pub in_sync_bytes: u64,
    pub in_sync_files: u64,
    pub invalid: Option<String>,
    pub local_bytes: u64,
    pub local_deleted: u64,
    pub local_directories: u64,
    pub local_files: u64,
    pub local_symlinks: u64,
    pub local_total_items: u64,
    pub need_bytes: u64,
    pub need_deletes: u64,
    pub need_directories: u64,
    pub need_files: u64,
    pub need_symlinks: u64,
    pub need_total_items: u64,
    pub pull_errors: u64,
    pub sequence: u64,
    pub state: String,
    pub state_changed: String,
    pub version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all(deserialize = "lowercase"))]
pub(crate) enum ItemAction {
    Update,
    Metadata,
    Delete,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ItemFinishedEvent {
    pub item: String,
    pub folder: String,
    pub error: Option<String>,
    #[serde(rename = "type")]
    pub item_type: String,
    pub action: ItemAction,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ItemStartedEvent {
    pub item: String,
    pub folder: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub action: ItemAction,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListenAddressesChangedEvent {}

#[derive(Debug, Deserialize)]
pub(crate) struct LocalChangeDetectedEvent {
    pub action: String,
    pub folder: String,
    pub label: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LocalIndexUpdatedEvent {
    #[serde(rename = "folder")]
    pub folder_id: String,
    pub items: u64,
    pub version: u64,
    pub filenames: Vec<FileName>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LoginAttemptEvent {
    pub username: String,
    pub success: bool,
}
#[derive(Debug, Deserialize)]
pub(crate) struct RemoteChangeDetectedEvent {
    pub action: String,
    #[serde(rename = "folderID")]
    pub folder_id: String,
    pub label: String,
    pub path: String,
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(rename = "modifiedBy")]
    pub modified_by: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteDownloadProgressEvent {
    #[serde(rename = "device")]
    pub device_id: DeviceID,
    pub folder: String,
    pub state: HashMap<FileName, u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteIndexUpdatedEvent {
    #[serde(rename = "device")]
    pub device_id: DeviceID,
    #[serde(rename = "folder")]
    pub folder_id: String,
    pub items: u64,
    pub version: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StartingEvent {
    #[serde(rename = "myID")]
    pub device_id: DeviceID,
    pub home: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all(deserialize = "kebab-case"))]
pub(crate) enum FolderState {
    Idle,
    Scanning,
    ScanWaiting,
    SyncPreparing,
    Syncing,
    Error,
    Unknown,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StateChangedEvent {
    #[serde(rename = "folder")]
    pub folder_id: String,
    pub duration: Option<f64>,
    pub from: FolderState,
    pub to: FolderState,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) enum EventData {
    ConfigSaved(ConfigSavedEvent),
    DeviceConnected(DeviceConnectedEvent),
    DeviceDisconnected(DeviceDisconnectedEvent),
    DeviceDiscovered(DeviceDiscoveredEvent),
    DevicePaused(DevicePausedEvent),
    DeviceRejected(DeviceRejectedEvent),
    DeviceResumed(DeviceResumedEvent),
    DownloadProgress(HashMap<FolderName, Folder>),
    FolderCompletion(FolderCompletionEvent),
    FolderErrors(FolderErrorsEvent),
    FolderRejected(FolderRejectedEvent),
    FolderScanProgress(FolderScanProgressEvent),
    FolderSummary(Box<FolderSummaryEvent>),
    ItemFinished(ItemFinishedEvent),
    ItemStarted(ItemStartedEvent),
    ListenAddressesChanged(ListenAddressesChangedEvent),
    LocalChangeDetected(LocalChangeDetectedEvent),
    LocalIndexUpdated(LocalIndexUpdatedEvent),
    LoginAttempt(LoginAttemptEvent),
    RemoteChangeDetected(RemoteChangeDetectedEvent),
    RemoteDownloadProgress(RemoteDownloadProgressEvent),
    RemoteIndexUpdated(RemoteIndexUpdatedEvent),
    Starting(StartingEvent),
    StartupComplete,
    StateChanged(StateChangedEvent),
}

#[derive(Debug, Deserialize)]
pub(super) struct RawEvent {
    pub id: u64,
    #[serde(rename = "globalID")]
    pub global_id: u64,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub time: String,
    pub data: Box<serde_json::value::RawValue>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) enum EventType {
    ConfigSaved,
    DeviceConnected,
    DeviceDisconnected,
    DeviceDiscovered,
    DevicePaused,
    DeviceRejected,
    DeviceResumed,
    DownloadProgress,
    FolderCompletion,
    FolderErrors,
    FolderRejected,
    FolderScanProgress,
    FolderSummary,
    ItemFinished,
    ItemStarted,
    ListenAddressesChanged,
    LocalChangeDetected,
    LocalIndexUpdated,
    LoginAttempt,
    RemoteChangeDetected,
    RemoteDownloadProgress,
    RemoteIndexUpdated,
    Starting,
    StartupComplete,
    StateChanged,
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "RawEvent")]
pub(crate) struct Event {
    pub id: u64,
    pub global_id: u64,
    pub time: String,
    pub data: EventData,
}

impl TryFrom<RawEvent> for Event {
    type Error = serde_json::Error;

    fn try_from(raw_event: RawEvent) -> Result<Self, Self::Error> {
        use EventData::*;
        let RawEvent {
            id,
            global_id,
            event_type,
            time,
            data,
        } = raw_event;
        let data = data.get();
        Ok(Event {
            id,
            global_id,
            time,
            data: match event_type {
                EventType::ConfigSaved => ConfigSaved(serde_json::from_str(data)?),
                EventType::DeviceConnected => DeviceConnected(serde_json::from_str(data)?),
                EventType::DeviceDisconnected => DeviceDisconnected(serde_json::from_str(data)?),
                EventType::DeviceDiscovered => DeviceDiscovered(serde_json::from_str(data)?),
                EventType::DevicePaused => DevicePaused(serde_json::from_str(data)?),
                EventType::DeviceRejected => DeviceRejected(serde_json::from_str(data)?),
                EventType::DeviceResumed => DeviceResumed(serde_json::from_str(data)?),
                EventType::DownloadProgress => DownloadProgress(serde_json::from_str(data)?),
                EventType::FolderCompletion => FolderCompletion(serde_json::from_str(data)?),
                EventType::FolderErrors => FolderErrors(serde_json::from_str(data)?),
                EventType::FolderRejected => FolderRejected(serde_json::from_str(data)?),
                EventType::FolderScanProgress => FolderScanProgress(serde_json::from_str(data)?),
                EventType::FolderSummary => FolderSummary(serde_json::from_str(data)?),
                EventType::ItemFinished => ItemFinished(serde_json::from_str(data)?),
                EventType::ItemStarted => ItemStarted(serde_json::from_str(data)?),
                EventType::ListenAddressesChanged => {
                    ListenAddressesChanged(serde_json::from_str(data)?)
                }
                EventType::LocalChangeDetected => LocalChangeDetected(serde_json::from_str(data)?),
                EventType::LocalIndexUpdated => LocalIndexUpdated(serde_json::from_str(data)?),
                EventType::LoginAttempt => LoginAttempt(serde_json::from_str(data)?),
                EventType::RemoteChangeDetected => {
                    RemoteChangeDetected(serde_json::from_str(data)?)
                }
                EventType::RemoteDownloadProgress => {
                    RemoteDownloadProgress(serde_json::from_str(data)?)
                }
                EventType::RemoteIndexUpdated => RemoteIndexUpdated(serde_json::from_str(data)?),
                EventType::Starting => Starting(serde_json::from_str(data)?),
                EventType::StartupComplete => StartupComplete,
                EventType::StateChanged => StateChanged(serde_json::from_str(data)?),
            },
        })
    }
}

//
// /rest/system/config response
//

#[derive(serde::Deserialize)]
pub(crate) struct SystemConfig {
    pub folders: Vec<SystemConfigFolder>,
}

#[derive(serde::Deserialize)]
pub(crate) struct SystemConfigFolder {
    pub path: String,
    pub id: String,
}

//
// /rest/system/status response
//

#[derive(serde::Deserialize)]
pub(crate) struct SystemStatus {
    #[serde(rename = "myID")]
    pub my_id: String,
}
