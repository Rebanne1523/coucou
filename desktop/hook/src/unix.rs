//! The Linux half of the relay: connect to the Unix socket Coucou listens on.
//!
//! `$XDG_RUNTIME_DIR` is private to the user, so in the normal case only we can
//! reach the socket at all. When it is unset we fall back to `/tmp`, which anyone
//! can write to — so before sending a single byte we ask the kernel who is on the
//! other end (SO_PEERCRED) and refuse anything that is not our own uid.

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

/// Must match the server's `socket_path()` exactly (src-tauri/src/relay/unix.rs).
fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute()) {
        Some(dir) => dir.join("coucou.sock"),
        None => PathBuf::from(format!("/tmp/coucou-{}.sock", unsafe { libc::getuid() })),
    }
}

/// True when the process on the other end of `stream` runs as the same user.
///
/// A failure to answer is treated as "not ours": refusing a socket we cannot
/// vouch for costs one hook event, while trusting it could hand another account
/// the contents of every tool call.
fn peer_is_same_user(stream: &UnixStream) -> bool {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    rc == 0 && cred.uid == unsafe { libc::getuid() }
}

/// Connects to Coucou. No retries: if the socket is missing or refuses, Coucou is
/// closed, and waiting would only delay Claude Code. (A full accept backlog makes
/// `connect` block, but the main thread's deadline abandons us long before that
/// matters.)
pub fn connect() -> Option<UnixStream> {
    let stream = UnixStream::connect(socket_path()).ok()?;
    peer_is_same_user(&stream).then_some(stream)
}
