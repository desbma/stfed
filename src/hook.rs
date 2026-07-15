//! Code to run hooks commands

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    ptr,
    sync::{Arc, Weak, mpsc},
    time::Duration,
};

use crate::config;

/// Unique identifier for a folder hook
#[derive(Eq, Hash, PartialEq)]
pub(crate) struct FolderHookId(usize);

impl FolderHookId {
    /// Create unique identifier for hook
    pub(crate) fn from_hook(hook: &config::FolderHook) -> Self {
        // Address identity is sound because hooks are not Clone and stay borrowed in place
        // for the daemon lifetime
        let val = ptr::from_ref(hook) as usize;
        Self(val)
    }
}

/// Run a given hook for a given path/folder
pub(crate) fn run(
    hook: &config::FolderHook,
    path: Option<&Path>,
    folder: &Path,
    reaper_tx: &mpsc::Sender<RunningHook>,
    running_hooks: &mut HashMap<FolderHookId, Weak<()>>,
) -> anyhow::Result<()> {
    let allow_concurrent = hook.allow_concurrent.unwrap_or(false);
    let hook_id = FolderHookId::from_hook(hook);
    let already_running = running_hooks
        .get(&hook_id)
        .and_then(Weak::upgrade)
        .is_some();
    if allow_concurrent || !already_running {
        log::info!("Running hook: {hook:?} with path {path:?} and folder {folder:?}");

        let child = Command::new(&hook.command[0])
            .args(&hook.command[1..])
            .env("STFED_PATH", path.unwrap_or(&PathBuf::from("")))
            .env("STFED_FOLDER", folder)
            .stdin(Stdio::null())
            .spawn()?;

        let token = Arc::new(());
        running_hooks.insert(hook_id, Arc::downgrade(&token));
        reaper_tx.send(RunningHook {
            _token: token,
            child,
        })?;
    } else {
        log::warn!(
            "A process is already running for this hook, and allow_concurrent is set for false, ignoring"
        );
    }

    Ok(())
}

/// A spawned hook process, whose liveness marks its hook as running
pub(crate) struct RunningHook {
    /// Token whose drop unmarks the hook as running
    _token: Arc<()>,
    /// The spawned process
    child: Child,
}

/// Reaper thread function, that waits for started processes
pub(crate) fn reaper(rx: &mpsc::Receiver<RunningHook>) -> anyhow::Result<()> {
    let mut watched: Vec<RunningHook> = Vec::new();
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
            for (i, running_hook) in watched.iter_mut().enumerate() {
                if let Some(rc) = running_hook.child.try_wait()? {
                    log::info!("Process exited with code {:?}", rc.code());
                    // Dropping the removed hook unmarks it as running
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
        }
    }

    /// A hook whose previous run has not been reaped must not run again
    #[test]
    fn skip_run_while_previous_run_not_reaped() {
        let hook = hook(&["true"], None);
        let (reaper_tx, reaper_rx) = mpsc::channel();
        let mut running_hooks = HashMap::new();

        run(&hook, None, Path::new("/"), &reaper_tx, &mut running_hooks).unwrap();
        let mut running_hook = reaper_rx.try_recv().unwrap();
        running_hook.child.wait().unwrap();

        run(&hook, None, Path::new("/"), &reaper_tx, &mut running_hooks).unwrap();
        assert!(reaper_rx.try_recv().is_err());
    }

    /// A hook allowing concurrent runs must spawn a process even while already running
    #[test]
    fn concurrent_runs_when_allowed() {
        let hook = hook(&["true"], Some(true));
        let (reaper_tx, reaper_rx) = mpsc::channel();
        let mut running_hooks = HashMap::new();

        run(&hook, None, Path::new("/"), &reaper_tx, &mut running_hooks).unwrap();
        run(&hook, None, Path::new("/"), &reaper_tx, &mut running_hooks).unwrap();

        for _ in 0..2 {
            let mut running_hook = reaper_rx.try_recv().unwrap();
            running_hook.child.wait().unwrap();
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
        let mut running_hooks = HashMap::new();

        run(
            &hook,
            Some(Path::new("sub/file.txt")),
            Path::new("/data/folder"),
            &reaper_tx,
            &mut running_hooks,
        )
        .unwrap();

        let mut running_hook = reaper_rx.try_recv().unwrap();
        assert!(running_hook.child.wait().unwrap().success());
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
        let mut running_hooks = HashMap::new();

        run(
            &hook,
            None,
            Path::new("/data/folder"),
            &reaper_tx,
            &mut running_hooks,
        )
        .unwrap();

        let mut running_hook = reaper_rx.try_recv().unwrap();
        assert!(running_hook.child.wait().unwrap().success());
        assert_eq!(fs::read_to_string(&out).unwrap(), "\n/data/folder");
    }

    /// The reaper must unregister a hook once its process exits, so it can run again
    #[test]
    fn reaper_unregisters_exited_hook() {
        let hook = hook(&["true"], None);
        let (reaper_tx, reaper_rx) = mpsc::channel();
        let mut running_hooks = HashMap::new();

        run(&hook, None, Path::new("/"), &reaper_tx, &mut running_hooks).unwrap();
        assert!(running_hooks.values().any(|t| t.upgrade().is_some()));

        thread::spawn(move || reaper(&reaper_rx));

        let deadline = Instant::now() + Duration::from_secs(5);
        while running_hooks.values().any(|t| t.upgrade().is_some()) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// A hook whose command fails to spawn must not stay marked as running
    #[test]
    fn failed_spawn_unmarks_hook() {
        let hook = hook(&["/nonexistent/hook/command"], None);
        let (reaper_tx, _reaper_rx) = mpsc::channel();
        let mut running_hooks = HashMap::new();

        assert!(run(&hook, None, Path::new("/"), &reaper_tx, &mut running_hooks).is_err());
        assert!(running_hooks.is_empty());
    }
}
