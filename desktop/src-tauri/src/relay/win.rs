// Windows transport: `\\.\pipe\coucou-<sid>`, one pipe instance per connection.

use std::time::Duration;

use tauri::AppHandle;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use super::{handle, win_user, Conn};
use crate::log;

impl Conn for NamedPipeServer {
    fn finish(&mut self) {
        let _ = self.disconnect();
    }
}

/// `\\.\pipe\coucou-<sid>` — must match coucou-hook's `pipe_path()` exactly.
pub fn pipe_name() -> String {
    let key = win_user::current_user_sid()
        .unwrap_or_else(|| std::env::var("USERNAME").unwrap_or_else(|_| "user".into()));
    format!(r"\\.\pipe\coucou-{key}")
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let name = pipe_name();
        // first_pipe_instance also means we refuse to join a pipe somebody else
        // already owns under our name, rather than serving on top of it.
        let mut server = match ServerOptions::new().first_pipe_instance(true).create(&name) {
            Ok(s) => s,
            Err(err) => {
                log::line(format!("cannot open the relay pipe: {err}"));
                return;
            }
        };
        loop {
            if server.connect().await.is_err() {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
            // Hand the connected instance to a task and listen on a fresh one.
            let next = match ServerOptions::new().create(&name) {
                Ok(s) => s,
                Err(err) => {
                    log::line(format!("cannot reopen the relay pipe: {err}"));
                    return;
                }
            };
            let connected = std::mem::replace(&mut server, next);
            let app = app.clone();
            tauri::async_runtime::spawn(async move { handle(app, connected).await });
        }
    });
}
