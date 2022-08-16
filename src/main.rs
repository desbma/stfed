//! Syncthing Folder Event Daemon

use std::collections::hash_map::{Entry, HashMap};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Context;

mod config;
mod hook;
mod syncthing;
mod syncthing_rest;

fn main() -> anyhow::Result<()> {
    // Init logger
    simple_logger::SimpleLogger::new()
        .init()
        .context("Failed to init logger")?;

    // Parse config
    let (cfg, hooks) = config::parse_config().context("Failed to read local config")?;
    let mut hooks_map: HashMap<(config::FolderEvent, &Path), Vec<config::FolderHook>> =
        HashMap::new();
    for hook in hooks.hooks.iter() {
        match hooks_map.entry((hook.event.clone(), &hook.folder)) {
            Entry::Occupied(mut e) => {
                e.get_mut().push(hook.clone());
            }
            Entry::Vacant(e) => {
                e.insert(vec![hook.clone()]);
            }
        }
    }

    // Create reaper thread and channel
    let (reaper_tx, reaper_rx) = mpsc::channel();
    thread::Builder::new()
        .name("reaper".to_string())
        .spawn(move || -> anyhow::Result<()> { hook::reaper(reaper_rx) })?;

    loop {
        // Setup client
        let client = syncthing::SyncthingClient::new(&cfg)?;

        // Handle events
        for event in client.iter_events() {
            let event = match event {
                Err(ref err) => {
                    if let Some(err) = err.downcast_ref::<syncthing::ServerGone>() {
                        log::warn!(
                            "Syncthing server is gone, will restart main loop. {:?}",
                            err
                        );
                        break;
                    } else if let Some(err) = err.downcast_ref::<syncthing::ServerConfigChanged>() {
                        log::warn!(
                            "Syncthing server configuration changed, will restart main loop. {:?}",
                            err
                        );
                        break;
                    } else {
                        event?;
                    }
                    unreachable!();
                }
                Ok(event) => event,
            };
            log::info!("New event: {:?}", event);

            // Dispatch event
            match event {
                syncthing::SyncthingEvent::FileDownSyncDone { path, folder } => {
                    for hook in hooks_map
                        .get(&(config::FolderEvent::FileDownSyncDone, &folder))
                        .unwrap_or(&vec![])
                    {
                        // TODO match file from filter
                        hook::run(hook, Some(&path), &folder, &reaper_tx)?;
                    }
                }
                syncthing::SyncthingEvent::FolderDownSyncDone { folder } => {
                    for hook in hooks_map
                        .get(&(config::FolderEvent::FolderDownSyncDone, &folder))
                        .unwrap_or(&vec![])
                    {
                        hook::run(hook, None, &folder, &reaper_tx)?;
                    }
                }
                syncthing::SyncthingEvent::FileConflict { path, folder } => {
                    for hook in hooks_map
                        .get(&(config::FolderEvent::FileConflict, &folder))
                        .unwrap_or(&vec![])
                    {
                        hook::run(hook, Some(&path), &folder, &reaper_tx)?;
                    }
                }
            }
        }

        /// Delay to wait for before trying to reconnect to Synthing server
        const RECONNECT_DELAY: Duration = Duration::from_secs(5);
        log::info!("Will reconnect in {:?}", RECONNECT_DELAY);
        thread::sleep(RECONNECT_DELAY);
    }
}
