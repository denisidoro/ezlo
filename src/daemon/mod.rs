//! Background completion daemon: a Unix-socket server shared by all zsh sessions of a user.

mod client;
pub mod index_loader;
pub mod protocol;
mod session;

pub use client::{query_daemon_status, spawn_detached_daemon, stop_daemon};

use crate::config::Config;
use crate::daemon::index_loader::{ensure_fresh_cache, load_index_isolated};
use crate::engine::generator::{GeneratorCache, GeneratorRunner};
use crate::history::HistFileCache;
use crate::shell::zsh::native::NativeZshPool;
use crate::spec::SpecIndex;
use crate::store::Store;
use anyhow::{Context, Result};
use nix::fcntl::{Flock, FlockArg};
use nix::unistd::Uid;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::net::UnixListener;
use tokio::sync::watch;

/// Quiet period after the last `.ezlo` event before reloading: editors emit several events per save.
const SPEC_RELOAD_DEBOUNCE: Duration = Duration::from_millis(200);

/// The daemon exits after this long without clients.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(30);

pub struct DaemonShared {
    pub config: Config,
    /// Swapped wholesale when a spec file changes.
    pub index: RwLock<Arc<SpecIndex>>,
    pub store: Store,
    pub cache: GeneratorCache,
    pub runner: Arc<dyn GeneratorRunner>,
    pub native_pool: NativeZshPool,
    pub histfiles: HistFileCache,
    pub active_clients: AtomicUsize,
    pub shutdown_tx: watch::Sender<bool>,
}

pub async fn run_daemon(
    socket_path: PathBuf,
    config: Config,
    store: Store,
    initial_index: SpecIndex,
    runner: Arc<dyn GeneratorRunner>,
) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }

    let Some(_lock) = acquire_instance_lock(&socket_path)? else {
        // Another daemon instance is already serving this socket.
        return Ok(());
    };

    let _ = fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind socket {}", socket_path.display()))?;
    let _ = fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600));

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let native_pool = NativeZshPool::new(config.sources.native.rc.clone());
    let shared = Arc::new(DaemonShared {
        config,
        index: RwLock::new(Arc::new(initial_index)),
        store,
        cache: GeneratorCache::new(),
        runner,
        native_pool,
        histfiles: HistFileCache::new(),
        active_clients: AtomicUsize::new(0),
        shutdown_tx,
    });

    let _watcher = start_spec_watcher(&shared);
    let expected_uid = Uid::current().as_raw();
    let mut last_activity = Instant::now();
    let mut idle_tick = tokio::time::interval(IDLE_CHECK_INTERVAL);

    loop {
        tokio::select! {
            accept_res = listener.accept() => {
                let Ok((stream, _addr)) = accept_res else { continue };
                if let Ok(cred) = stream.peer_cred() {
                    if cred.uid() != expected_uid {
                        tracing::warn!("rejecting peer with uid {} (expected {expected_uid})", cred.uid());
                        continue;
                    }
                }
                last_activity = Instant::now();
                shared.active_clients.fetch_add(1, Ordering::SeqCst);
                let session_shared = Arc::clone(&shared);
                tokio::spawn(async move {
                    session::handle_client(stream, Arc::clone(&session_shared)).await;
                    session_shared.active_clients.fetch_sub(1, Ordering::SeqCst);
                });
            }
            _ = idle_tick.tick() => {
                if shared.active_clients.load(Ordering::SeqCst) == 0
                    && last_activity.elapsed() >= IDLE_TIMEOUT
                {
                    break;
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
        }
    }

    let _ = fs::remove_file(&socket_path);
    Ok(())
}

/// Exclusive `flock` next to the socket; `None` when another instance holds it.
fn acquire_instance_lock(socket_path: &Path) -> Result<Option<Flock<fs::File>>> {
    let lock_path = socket_path.with_extension("lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open lock file {}", lock_path.display()))?;
    Ok(Flock::lock(file, FlockArg::LockExclusiveNonblock).ok())
}

/// Hot-reloads the index when a `.ezlo` file under the user or generated spec dirs changes, once
/// events have been quiet for [`SPEC_RELOAD_DEBOUNCE`].
fn start_spec_watcher(shared: &Arc<DaemonShared>) -> Option<RecommendedWatcher> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        if event.paths.iter().any(|p| p.extension().is_some_and(|e| e == crate::spec::SPEC_EXT)) {
            let _ = tx.send(());
        }
    })
    .ok()?;

    let dirs = shared
        .config
        .spec_dirs()
        .into_iter()
        .chain(std::iter::once(Config::generated_specs_dir()));
    for dir in dirs {
        let _ = fs::create_dir_all(&dir);
        let _ = watcher.watch(&dir, RecursiveMode::Recursive);
    }

    // Exits once the watcher (and with it the sender) is dropped.
    let reload_shared = Arc::clone(shared);
    std::thread::spawn(move || {
        while rx.recv().is_ok() {
            while rx.recv_timeout(SPEC_RELOAD_DEBOUNCE).is_ok() {}
            reload_index(&reload_shared);
        }
    });
    Some(watcher)
}

/// Rebuilds the index (out of process, see [`load_index_isolated`]) and swaps it in.
fn reload_index(shared: &DaemonShared) {
    ensure_fresh_cache(&shared.config);
    let Ok(mut guard) = shared.index.write() else {
        return;
    };
    // Free the old index before loading the new one so both never coexist: glibc would keep that
    // peak (~one extra index) for good. Queries just wait on the lock for the short cache load.
    *guard = Arc::new(SpecIndex::default());
    *guard = Arc::new(load_index_isolated(&shared.config));
}
