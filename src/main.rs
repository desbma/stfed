//! Syncthing Folder Event Daemon

use std::collections::{
    hash_map::{Entry, HashMap},
    HashSet,
};
use std::io;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
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

    // Build hook map for fast matching
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

    // Setup running hooks state
    let running_hooks: Arc<Mutex<HashSet<hook::FolderHookId>>> =
        Arc::new(Mutex::new(HashSet::new()));
    let running_hooks_reaper = Arc::clone(&running_hooks);

    // Create reaper thread and channel
    let (reaper_tx, reaper_rx) = mpsc::channel();
    thread::Builder::new()
        .name("reaper".to_string())
        .spawn(move || -> anyhow::Result<()> { hook::reaper(reaper_rx, &running_hooks_reaper) })?;

    loop {
        // Setup client
        let client_res = syncthing::SyncthingClient::new(&cfg);
        match client_res {
            Ok(client) => {
                // Event loop
                for event in client.iter_events() {
                    // Handle special events
                    let event = match event {
                        Err(ref err) => {
                            if let Some(err) = err.downcast_ref::<syncthing::ServerGone>() {
                                log::warn!(
                                    "Syncthing server is gone, will restart main loop. {:?}",
                                    err
                                );
                                break;
                            } else if let Some(err) =
                                err.downcast_ref::<syncthing::ServerConfigChanged>()
                            {
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
                                if hook
                                    .filter
                                    .as_ref()
                                    .map(|g| g.is_match(&path))
                                    .unwrap_or(true)
                                {
                                    hook::run(
                                        hook,
                                        Some(&path),
                                        &folder,
                                        &reaper_tx,
                                        &running_hooks,
                                    )?;
                                }
                            }
                        }
                        syncthing::SyncthingEvent::FolderDownSyncDone { folder } => {
                            for hook in hooks_map
                                .get(&(config::FolderEvent::FolderDownSyncDone, &folder))
                                .unwrap_or(&vec![])
                            {
                                hook::run(hook, None, &folder, &reaper_tx, &running_hooks)?;
                            }
                        }
                        syncthing::SyncthingEvent::FileConflict { path, folder } => {
                            for hook in hooks_map
                                .get(&(config::FolderEvent::FileConflict, &folder))
                                .unwrap_or(&vec![])
                            {
                                hook::run(hook, Some(&path), &folder, &reaper_tx, &running_hooks)?;
                            }
                        }
                    }
                }
            }
            Err(ref err) => match err.root_cause().downcast_ref::<io::Error>() {
                Some(err2) if err2.kind() == io::ErrorKind::ConnectionRefused => {
                    log::warn!(
                        "Syncthing·server·connection failed,·will·restart·main·loop.·{:?}",
                        err
                    );
                }
                _ => {
                    client_res?;
                }
            },
        }

        /// Delay to wait for before trying to reconnect to Synthing server
        const RECONNECT_DELAY: Duration = Duration::from_secs(5);
        log::info!("Will reconnect in {:?}", RECONNECT_DELAY);
        thread::sleep(RECONNECT_DELAY);
    }
}
