//! Code to run hooks commands

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::config;

/// Unique identified for a folder hook
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct FolderHookId {
    /// Pointer value
    val: usize,
}

impl FolderHookId {
    /// Create unique identifier for hook
    pub fn from_hook(hook: &config::FolderHook) -> FolderHookId {
        let val = (hook as *const config::FolderHook) as usize;
        FolderHookId { val }
    }
}

/// Run a given hook for a given path/folder
pub fn run(
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

        log::info!(
            "Running hook: {:?} with path {:?} and folder {:?}",
            hook,
            path,
            folder
        );

        let child = Command::new(&hook.command[0])
            .args(&hook.command[1..])
            .env("STFED_PATH", path.unwrap_or(&PathBuf::from("")))
            .env("STFED_FOLDER", folder)
            .stdin(Stdio::null())
            .spawn()?;

        reaper_tx.send((hook_id, child))?;
    } else {
        log::warn!("A process is already running for this hook, and allow_concurrent is set for false, ignoring");
    }

    Ok(())
}

/// Reaper thread function, that waits for started processes
pub fn reaper(
    rx: mpsc::Receiver<(FolderHookId, Child)>,
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
            watched.push(new)
        }
        loop {
            let mut do_loop = false;
            for i in 0..watched.len() {
                let (hook_id, child) = watched.get_mut(i).unwrap();
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
