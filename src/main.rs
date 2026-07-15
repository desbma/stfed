//! Syncthing Folder Event Daemon

use std::{
    collections::hash_map::{Entry, HashMap},
    io,
    rc::Rc,
    sync::{LazyLock, mpsc},
    thread,
    time::Duration,
};

use anyhow::Context as _;
use config::NormalizedPath;

mod config;
mod hook;
mod syncthing;
mod syncthing_rest;

/// Delay to wait for before trying to reconnect to Synthing server
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Glob matcher for a conflict file
static CONFLICT_MATCHER: LazyLock<globset::GlobMatcher> = LazyLock::new(|| {
    #[expect(clippy::unwrap_used)]
    globset::Glob::new("*.sync-conflict-*")
        .unwrap()
        .compile_matcher()
});

#[expect(clippy::too_many_lines)]
fn main() -> anyhow::Result<()> {
    // Init logger
    simple_logger::SimpleLogger::new()
        .env()
        .init()
        .context("Failed to init logger")?;

    // Parse config
    let (cfg, hooks) = config::parse().context("Failed to read local config")?;

    // Build hook map for fast matching
    let mut hooks_map: HashMap<
        (config::FolderEvent, Rc<NormalizedPath>),
        Vec<&config::FolderHook>,
    > = HashMap::new();
    for hook in &hooks.hooks {
        match hooks_map.entry((hook.event.clone(), Rc::new(hook.folder.clone()))) {
            Entry::Occupied(mut e) => {
                e.get_mut().push(hook);
            }
            Entry::Vacant(e) => {
                e.insert(vec![hook]);
            }
        }
    }

    // Setup running hooks state
    let mut running_hooks = HashMap::new();

    // Create reaper thread and channel
    let (reaper_tx, reaper_rx) = mpsc::channel();
    thread::Builder::new()
        .name("reaper".to_owned())
        .spawn(move || -> anyhow::Result<()> { hook::reaper(&reaper_rx) })?;

    // Position reached in the event stream, to resume it where it stopped when the connection
    // is lost
    let mut cursor = None;

    loop {
        // Setup client
        let client_res = syncthing::Client::new(&cfg);
        match client_res {
            Ok(client) => {
                // Event loop
                let mut events = client.iter_events(cursor.as_ref());
                for event in &mut events {
                    // Handle special events
                    let event = match &event {
                        Err(err) => {
                            if let Some(err) = err.downcast_ref::<syncthing::ServerGone>() {
                                log::warn!(
                                    "Syncthing server is gone, will restart main loop. {err:?}"
                                );
                                break;
                            } else if let Some(err) =
                                err.downcast_ref::<syncthing::ServerConfigChanged>()
                            {
                                log::warn!(
                                    "Syncthing server configuration changed, will restart main loop. {err:?}"
                                );
                                break;
                            }
                            event?;
                            unreachable!();
                        }
                        Ok(event) => event,
                    };
                    log::info!("New event: {event:?}");

                    // Dispatch event
                    match event {
                        syncthing::Event::FileDownSyncDone { path, folder } => {
                            let folder: Rc<NormalizedPath> = Rc::new(folder.as_path().try_into()?);
                            for hook in hooks_map
                                .get(&(config::FolderEvent::FileDownSyncDone, Rc::clone(&folder)))
                                .unwrap_or(&vec![])
                            {
                                if hook.filter.as_ref().is_none_or(|g| g.is_match(path)) {
                                    hook::run(
                                        hook,
                                        Some(path),
                                        &folder,
                                        &reaper_tx,
                                        &mut running_hooks,
                                    )?;
                                }
                            }
                            for hook in hooks_map
                                .get(&(config::FolderEvent::RemoteFileConflict, Rc::clone(&folder)))
                                .unwrap_or(&vec![])
                            {
                                if CONFLICT_MATCHER.is_match(path) {
                                    hook::run(
                                        hook,
                                        Some(path),
                                        &folder,
                                        &reaper_tx,
                                        &mut running_hooks,
                                    )?;
                                }
                            }
                        }
                        syncthing::Event::FolderDownSyncDone { folder } => {
                            let folder: Rc<NormalizedPath> = Rc::new(folder.as_path().try_into()?);
                            for hook in hooks_map
                                .get(&(config::FolderEvent::FolderDownSyncDone, Rc::clone(&folder)))
                                .unwrap_or(&vec![])
                            {
                                hook::run(hook, None, &folder, &reaper_tx, &mut running_hooks)?;
                            }
                        }
                        syncthing::Event::FileConflict { path, folder } => {
                            let folder: Rc<NormalizedPath> = Rc::new(folder.as_path().try_into()?);
                            for hook in hooks_map
                                .get(&(config::FolderEvent::FileConflict, Rc::clone(&folder)))
                                .unwrap_or(&vec![])
                            {
                                hook::run(
                                    hook,
                                    Some(path),
                                    &folder,
                                    &reaper_tx,
                                    &mut running_hooks,
                                )?;
                            }
                        }
                    }
                }
                cursor = events.cursor();
            }
            #[expect(clippy::ref_patterns)]
            Err(ref err) => match err.root_cause().downcast_ref::<ureq::Error>() {
                Some(ureq::Error::Io(err2)) if err2.kind() == io::ErrorKind::ConnectionRefused => {
                    log::warn!(
                        "Syncthing server connection failed, will restart main loop. {err:?}"
                    );
                }
                _ => {
                    client_res?;
                }
            },
        }

        log::info!("Will reconnect in {RECONNECT_DELAY:?}");
        thread::sleep(RECONNECT_DELAY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Conflict file names as created by Syncthing must match, at any folder depth
    #[test]
    fn conflict_matcher_matches_conflict_files() {
        assert!(CONFLICT_MATCHER.is_match("doc.sync-conflict-20260711-084512-ABCDEFG.txt"));
        assert!(CONFLICT_MATCHER.is_match("sub/dir/doc.sync-conflict-20260711-084512-ABCDEFG.txt"));
        assert!(!CONFLICT_MATCHER.is_match("doc.txt"));
        assert!(!CONFLICT_MATCHER.is_match("sync-conflict.txt"));
    }
}
