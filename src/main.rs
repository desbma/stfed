//! Syncthing Folder Event Daemon

use std::{
    collections::{
        hash_map::{Entry, HashMap},
        HashSet,
    },
    io,
    rc::Rc,
    sync::{mpsc, Arc, LazyLock, Mutex},
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
        .init()
        .context("Failed to init logger")?;

    // Parse config
    let (cfg, hooks) = config::parse().context("Failed to read local config")?;

    // Build hook map for fast matching
    let mut hooks_map: HashMap<(config::FolderEvent, Rc<NormalizedPath>), Vec<config::FolderHook>> =
        HashMap::new();
    for hook in &hooks.hooks {
        match hooks_map.entry((hook.event.clone(), Rc::new(hook.folder.clone()))) {
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
        .name("reaper".to_owned())
        .spawn(move || -> anyhow::Result<()> { hook::reaper(&reaper_rx, &running_hooks_reaper) })?;

    loop {
        // Setup client
        let client_res = syncthing::Client::new(&cfg);
        match client_res {
            Ok(client) => {
                // Event loop
                for event in client.iter_events() {
                    // Handle special events
                    let event = match &event {
                        Err(err) => {
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
                            }
                            event?;
                            unreachable!();
                        }
                        Ok(event) => event,
                    };
                    log::info!("New event: {:?}", event);

                    // Dispatch event
                    match event {
                        syncthing::Event::FileDownSyncDone { path, folder } => {
                            let folder: Rc<NormalizedPath> = Rc::new(folder.as_path().try_into()?);
                            for hook in hooks_map
                                .get(&(config::FolderEvent::FileDownSyncDone, Rc::clone(&folder)))
                                .unwrap_or(&vec![])
                            {
                                if hook.filter.as_ref().map_or(true, |g| g.is_match(path)) {
                                    hook::run(
                                        hook,
                                        Some(path),
                                        &folder,
                                        &reaper_tx,
                                        &running_hooks,
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
                                        &running_hooks,
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
                                hook::run(hook, None, &folder, &reaper_tx, &running_hooks)?;
                            }
                        }
                        syncthing::Event::FileConflict { path, folder } => {
                            let folder: Rc<NormalizedPath> = Rc::new(folder.as_path().try_into()?);
                            for hook in hooks_map
                                .get(&(config::FolderEvent::FileConflict, Rc::clone(&folder)))
                                .unwrap_or(&vec![])
                            {
                                hook::run(hook, Some(path), &folder, &reaper_tx, &running_hooks)?;
                            }
                        }
                    }
                }
            }
            #[expect(clippy::ref_patterns)]
            Err(ref err) => match err.root_cause().downcast_ref::<io::Error>() {
                Some(err2) if err2.kind() == io::ErrorKind::ConnectionRefused => {
                    log::warn!(
                        "Syncthing server connection failed, will restart main loop. {:?}",
                        err
                    );
                }
                _ => {
                    client_res?;
                }
            },
        }

        log::info!("Will reconnect in {:?}", RECONNECT_DELAY);
        thread::sleep(RECONNECT_DELAY);
    }
}
