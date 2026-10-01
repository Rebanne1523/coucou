// Linux transport: a Unix socket in the user's runtime directory.
//
// `$XDG_RUNTIME_DIR` is private to the user (0700, tmpfs, cleaned at logout), so
// nobody else can reach `coucou.sock` there. If it is missing we fall back to
// `/tmp/coucou-<uid>.sock`, which is not private — so on top of the directory we
// check the owner of the socket file before binding and the uid of every peer
// that connects. coucou-hook does the mirror check on its side.

use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::time::Duration;

use tauri::AppHandle;
use tokio::net::{UnixListener, UnixStream};

use super::{handle, Conn};
use crate::log;

impl Conn for UnixStream {
    fn finish(&mut self) {
        // Dropping the stream closes it; nothing else to flush.
    }
}

/// Must match coucou-hook's `socket_path()` exactly.
pub fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute()) {
        Some(dir) => dir.join("coucou.sock"),
        None => PathBuf::from(format!("/tmp/coucou-{}.sock", unsafe { libc::getuid() })),
    }
}

/// Makes the path free for us to bind: removes a stale socket of ours, refuses
/// anything else. Returns false when something live or foreign is in the way.
async fn clear_path(path: &PathBuf) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return true };
    let mine = meta.uid() == unsafe { libc::getuid() };
    if !meta.file_type().is_socket() || !mine {
        log::line(format!("{} exists and is not our socket — not touching it", path.display()));
        return false;
    }
    // Another Coucou answering means we are the second instance.
    if UnixStream::connect(path).await.is_ok() {
        log::line("another Coucou already owns the relay socket".to_string());
        return false;
    }
    std::fs::remove_file(path).is_ok()
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let path = socket_path();
        if !clear_path(&path).await {
            return;
        }
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(err) => {
                log::line(format!("cannot open the relay socket {}: {err}", path.display()));
                return;
            }
        };
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        let uid = unsafe { libc::getuid() };

        loop {
            let (stream, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(err) => {
                    log::line(format!("relay accept failed: {err}"));
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
            };
            // Whoever is on the other end must be us: a hook run by another
            // account never gets to put a card on this island.
            if stream.peer_cred().map(|c| c.uid()).ok() != Some(uid) {
                log::line("relay: refused a connection from another user".to_string());
                continue;
            }
            let app = app.clone();
            tauri::async_runtime::spawn(async move { handle(app, stream).await });
        }
    });
}
