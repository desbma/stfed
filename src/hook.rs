//! Code to run hooks commands

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use crate::config;

/// Unique identifier for a folder hook
#[cfg_attr(test, derive(Debug))]
#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) struct FolderHookId(usize);

impl FolderHookId {
    /// Create unique identifier for hook, from its parse time index
    pub(crate) fn from_hook(hook: &config::FolderHook) -> Self {
        Self(hook.index)
    }
}

/// Run a given hook for a given path/folder
pub(crate) fn run(
    hook: &config::FolderHook,
    path: Option<&Path>,
    folder: &Path,
    reaper_tx: &mpsc::Sender<(FolderHookId, Child)>,
    running_hooks: &Arc<Mutex<HashSet<FolderHookId>>>,
) -> anyhow::Result<()> {
    let allow_concurrent = hook.allow_concurrent.unwrap_or(false);
    let hook_id = FolderHookId::from_hook(hook);
    let mut running_hooks_locked = running_hooks
        .lock()
        .map_err(|_| anyhow::anyhow!("Failed to take lock"))?;
    if allow_concurrent || !running_hooks_locked.contains(&hook_id) {
        running_hooks_locked.insert(hook_id.clone());
        drop(running_hooks_locked);

        log::info!("Running hook: {hook:?} with path {path:?} and folder {folder:?}");

        let child = Command::new(&hook.command[0])
            .args(&hook.command[1..])
            .env("STFED_PATH", path.unwrap_or(&PathBuf::from("")))
            .env("STFED_FOLDER", folder)
            .stdin(Stdio::null())
            .spawn()?;

        reaper_tx.send((hook_id, child))?;
    } else {
        log::warn!(
            "A process is already running for this hook, and allow_concurrent is set for false, ignoring"
        );
    }

    Ok(())
}

/// Reaper thread function, that waits for started processes
pub(crate) fn reaper(
    rx: &mpsc::Receiver<(FolderHookId, Child)>,
    running_hooks: &Arc<Mutex<HashSet<FolderHookId>>>,
) -> anyhow::Result<()> {
    let mut watched = Vec::new();
    loop {
        /// Wait delay for channel recv, only effective if having at least 1 process to watch
        const REAPER_WAIT_DELAY: Duration = Duration::from_millis(500);
        if watched.is_empty() {
            let new = rx.recv()?;
            watched.push(new);
        } else if let Ok(new) = rx.recv_timeout(REAPER_WAIT_DELAY) {
            watched.push(new);
        }
        loop {
            let mut do_loop = false;
            for (i, (hook_id, child)) in watched.iter_mut().enumerate() {
                if let Some(rc) = child.try_wait()? {
                    log::info!("Process exited with code {:?}", rc.code());
                    {
                        let mut running_hooks_locked = running_hooks
                            .lock()
                            .map_err(|_| anyhow::anyhow!("Failed to take lock"))?;
                        running_hooks_locked.remove(hook_id);
                    }
                    watched.swap_remove(i);
                    do_loop = true;
                    break;
                }
            }
            if !do_loop {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, thread, time::Instant};

    use super::*;

    /// Hook running `command`
    fn hook(command: &[&str], allow_concurrent: Option<bool>) -> config::FolderHook {
        config::FolderHook {
            folder: Path::new("/").try_into().unwrap(),
            event: config::FolderEvent::FileDownSyncDone,
            filter: None,
            command: command.iter().map(|a| (*a).to_owned()).collect(),
            allow_concurrent,
            index: 0,
        }
    }

    /// A hook whose previous run has not been reaped must not run again
    #[test]
    fn skip_run_while_previous_run_not_reaped() {
        let hook = hook(&["true"], None);
        let (reaper_tx, reaper_rx) = mpsc::channel();
        let running_hooks = Arc::new(Mutex::new(HashSet::new()));

        run(&hook, None, Path::new("/"), &reaper_tx, &running_hooks).unwrap();
        let (_hook_id, mut child) = reaper_rx.try_recv().unwrap();
        child.wait().unwrap();

        run(&hook, None, Path::new("/"), &reaper_tx, &running_hooks).unwrap();
        assert!(reaper_rx.try_recv().is_err());
    }

    /// A hook allowing concurrent runs must spawn a process even while already running
    #[test]
    fn concurrent_runs_when_allowed() {
        let hook = hook(&["true"], Some(true));
        let (reaper_tx, reaper_rx) = mpsc::channel();
        let running_hooks = Arc::new(Mutex::new(HashSet::new()));

        run(&hook, None, Path::new("/"), &reaper_tx, &running_hooks).unwrap();
        run(&hook, None, Path::new("/"), &reaper_tx, &running_hooks).unwrap();

        for _ in 0..2 {
            let (_hook_id, mut child) = reaper_rx.try_recv().unwrap();
            child.wait().unwrap();
        }
    }

    /// The event path and folder must be exported to the hook command environment
    #[test]
    fn export_path_and_folder_to_environment() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let script = format!(
            "printf '%s\\n%s' \"$STFED_PATH\" \"$STFED_FOLDER\" > {out}",
            out = out.to_str().unwrap()
        );
        let hook = hook(&["sh", "-c", &script], None);
        let (reaper_tx, reaper_rx) = mpsc::channel();
        let running_hooks = Arc::new(Mutex::new(HashSet::new()));

        run(
            &hook,
            Some(Path::new("sub/file.txt")),
            Path::new("/data/folder"),
            &reaper_tx,
            &running_hooks,
        )
        .unwrap();

        let (_hook_id, mut child) = reaper_rx.try_recv().unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(
            fs::read_to_string(&out).unwrap(),
            "sub/file.txt\n/data/folder"
        );
    }

    /// Without an event path, the exported path variable must be empty
    #[test]
    fn export_empty_path_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let script = format!(
            "printf '%s\\n%s' \"$STFED_PATH\" \"$STFED_FOLDER\" > {out}",
            out = out.to_str().unwrap()
        );
        let hook = hook(&["sh", "-c", &script], None);
        let (reaper_tx, reaper_rx) = mpsc::channel();
        let running_hooks = Arc::new(Mutex::new(HashSet::new()));

        run(
            &hook,
            None,
            Path::new("/data/folder"),
            &reaper_tx,
            &running_hooks,
        )
        .unwrap();

        let (_hook_id, mut child) = reaper_rx.try_recv().unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(fs::read_to_string(&out).unwrap(), "\n/data/folder");
    }

    /// The reaper must unregister a hook once its process exits, so it can run again
    #[test]
    fn reaper_unregisters_exited_hook() {
        let hook = hook(&["true"], None);
        let (reaper_tx, reaper_rx) = mpsc::channel();
        let running_hooks = Arc::new(Mutex::new(HashSet::new()));

        run(&hook, None, Path::new("/"), &reaper_tx, &running_hooks).unwrap();
        assert!(!running_hooks.lock().unwrap().is_empty());

        let running_hooks_reaper = Arc::clone(&running_hooks);
        thread::spawn(move || reaper(&reaper_rx, &running_hooks_reaper));

        let deadline = Instant::now() + Duration::from_secs(5);
        while !running_hooks.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Hook identity must be identical for clones of the same hook (the hook map
    /// stores clones), and unique across different hooks
    #[test]
    fn hook_id_is_stable_and_unique() {
        let hook0 = hook(&["true"], None);
        let hook1 = config::FolderHook {
            index: 1,
            ..hook0.clone()
        };

        assert_eq!(
            FolderHookId::from_hook(&hook0),
            FolderHookId::from_hook(&hook0.clone())
        );
        assert_ne!(
            FolderHookId::from_hook(&hook0),
            FolderHookId::from_hook(&hook1)
        );
    }
}
