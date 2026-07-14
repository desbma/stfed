//! Syncthing Folder Event Daemon

use std::{
    collections::{
        HashSet,
        hash_map::{Entry, HashMap},
    },
    io,
    path::Path,
    process::Child,
    rc::Rc,
    sync::{Arc, LazyLock, Mutex, mpsc},
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

/// Map from event kind + folder to the hooks to run
type HookMap = HashMap<(config::FolderEvent, Rc<NormalizedPath>), Vec<config::FolderHook>>;

/// Run a hook, logging errors instead of propagating them, so that a failing hook
/// (typo in command path, resource exhaustion...) can not crash the daemon
fn run_hook(
    hook: &config::FolderHook,
    path: Option<&Path>,
    folder: &NormalizedPath,
    reaper_tx: &mpsc::Sender<(hook::FolderHookId, Child)>,
    running_hooks: &Arc<Mutex<HashSet<hook::FolderHookId>>>,
) {
    if let Err(err) = hook::run(hook, path, folder, reaper_tx, running_hooks) {
        log::error!(
            "Failed to run hook command {command:?}: {err:#}",
            command = hook.command
        );
    }
}

/// Dispatch a single event to its matching hooks, individual hook run errors are
/// logged and do not interrupt dispatching
fn dispatch_event(
    event: &syncthing::Event,
    hooks_map: &HookMap,
    reaper_tx: &mpsc::Sender<(hook::FolderHookId, Child)>,
    running_hooks: &Arc<Mutex<HashSet<hook::FolderHookId>>>,
) -> anyhow::Result<()> {
    match event {
        syncthing::Event::FileDownSyncDone { path, folder } => {
            let folder: Rc<NormalizedPath> = Rc::new(folder.as_path().try_into()?);
            for hook in hooks_map
                .get(&(config::FolderEvent::FileDownSyncDone, Rc::clone(&folder)))
                .unwrap_or(&vec![])
            {
                if hook.filter.as_ref().is_none_or(|g| g.is_match(path)) {
                    run_hook(hook, Some(path), &folder, reaper_tx, running_hooks);
                }
            }
            for hook in hooks_map
                .get(&(config::FolderEvent::RemoteFileConflict, Rc::clone(&folder)))
                .unwrap_or(&vec![])
            {
                if CONFLICT_MATCHER.is_match(path) {
                    run_hook(hook, Some(path), &folder, reaper_tx, running_hooks);
                }
            }
        }
        syncthing::Event::FolderDownSyncDone { folder } => {
            let folder: Rc<NormalizedPath> = Rc::new(folder.as_path().try_into()?);
            for hook in hooks_map
                .get(&(config::FolderEvent::FolderDownSyncDone, Rc::clone(&folder)))
                .unwrap_or(&vec![])
            {
                run_hook(hook, None, &folder, reaper_tx, running_hooks);
            }
        }
        syncthing::Event::FileConflict { path, folder } => {
            let folder: Rc<NormalizedPath> = Rc::new(folder.as_path().try_into()?);
            for hook in hooks_map
                .get(&(config::FolderEvent::FileConflict, Rc::clone(&folder)))
                .unwrap_or(&vec![])
            {
                run_hook(hook, Some(path), &folder, reaper_tx, running_hooks);
            }
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    // Init logger
    simple_logger::SimpleLogger::new()
        .env()
        .init()
        .context("Failed to init logger")?;

    // Parse config
    let (cfg, hooks) = config::parse().context("Failed to read local config")?;

    // Build hook map for fast matching
    let mut hooks_map: HookMap = HashMap::new();
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

                    // Dispatch event, errors (unresolvable folder path, failing hook...)
                    // are logged and do not stop the event loop
                    if let Err(err) = dispatch_event(event, &hooks_map, &reaper_tx, &running_hooks)
                    {
                        log::error!("Failed to dispatch event {event:?}: {err:#}");
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
    use std::path::PathBuf;

    use super::*;

    /// Conflict file names as created by Syncthing must match, at any folder depth
    #[test]
    fn conflict_matcher_matches_conflict_files() {
        assert!(CONFLICT_MATCHER.is_match("doc.sync-conflict-20260711-084512-ABCDEFG.txt"));
        assert!(CONFLICT_MATCHER.is_match("sub/dir/doc.sync-conflict-20260711-084512-ABCDEFG.txt"));
        assert!(!CONFLICT_MATCHER.is_match("doc.txt"));
        assert!(!CONFLICT_MATCHER.is_match("sync-conflict.txt"));
    }

    /// Hook running `command` for `folder` on file down sync done
    fn test_hook(folder: &NormalizedPath, command: &[&str]) -> config::FolderHook {
        config::FolderHook {
            folder: folder.clone(),
            event: config::FolderEvent::FileDownSyncDone,
            filter: None,
            command: command.iter().map(|a| (*a).to_owned()).collect(),
            allow_concurrent: None,
        }
    }

    /// A hook that fails to run must not interrupt dispatching: following hooks still run
    #[test]
    fn failing_hook_does_not_interrupt_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let folder: NormalizedPath = dir.path().try_into().unwrap();
        let marker = dir.path().join("marker");
        let hooks = vec![
            test_hook(&folder, &["/nonexistent/hook/command"]),
            test_hook(&folder, &["touch", marker.to_str().unwrap()]),
        ];
        let mut hooks_map = HookMap::new();
        hooks_map.insert(
            (config::FolderEvent::FileDownSyncDone, Rc::new(folder)),
            hooks,
        );
        let running_hooks = Arc::new(Mutex::new(HashSet::new()));
        let (reaper_tx, reaper_rx) = mpsc::channel();
        let event = syncthing::Event::FileDownSyncDone {
            path: PathBuf::from("file.txt"),
            folder: dir.path().to_owned(),
        };

        dispatch_event(&event, &hooks_map, &reaper_tx, &running_hooks).unwrap();

        // Only the second hook spawned a process
        let (_hook_id, mut child) = reaper_rx.try_recv().unwrap();
        assert!(child.wait().unwrap().success());
        assert!(reaper_rx.try_recv().is_err());
        assert!(marker.is_file());
    }

    /// An event folder that does not resolve locally is an error, not an exit or panic
    #[test]
    fn unresolvable_folder_path_is_an_error() {
        let hooks_map = HookMap::new();
        let running_hooks = Arc::new(Mutex::new(HashSet::new()));
        let (reaper_tx, _reaper_rx) = mpsc::channel();
        let event = syncthing::Event::FileDownSyncDone {
            path: PathBuf::from("file.txt"),
            folder: PathBuf::from("/nonexistent/stfed/folder"),
        };

        assert!(dispatch_event(&event, &hooks_map, &reaper_tx, &running_hooks).is_err());
    }
}
