// API keys live in the OS secret store — the Windows Credential Manager, or on
// Linux the Secret Service (KWallet or GNOME Keyring over D-Bus) — never on disk
// and never in the front end. The island can only ask whether a key is present.

use keyring::Entry;

const SERVICE: &str = "fr.louisraille.coucou";

/// Every key Coucou may store. Anything outside this list is refused.
pub const KNOWN_KEYS: &[&str] = &[
    "anthropic-api-key",
    "n8n-url",
    "n8n-api-key",
    "vercel-token",
    "github-token",
    "stripe-api-key",
    "resend-api-key",
    "notion-api-key",
    "calcom-api-key",
    "supabase-url",
    "supabase-key",
    "supabase-token",
];

fn entry(key: &str) -> Option<Entry> {
    if !KNOWN_KEYS.contains(&key) {
        return None;
    }
    Entry::new(SERVICE, key).ok()
}

pub fn get(key: &str) -> Option<String> {
    entry(key)?.get_password().ok().filter(|v| !v.is_empty())
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    let entry = entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    if value.is_empty() {
        let _ = entry.delete_credential();
        return Ok(());
    }
    entry.set_password(value).map_err(explain)
}

/// On Linux the usual failure is "nothing is serving the Secret Service": no
/// wallet daemon, or a locked one. Say so instead of a bare D-Bus error.
fn explain(err: keyring::Error) -> String {
    #[cfg(target_os = "linux")]
    if matches!(err, keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_)) {
        return format!(
            "No Secret Service available ({err}). Start KWallet (Plasma: System Settings → KDE Wallet → enable, and let it provide the Secret Service) or GNOME Keyring, then try again."
        );
    }
    err.to_string()
}

pub fn clear(key: &str) -> Result<(), String> {
    let entry = entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(explain(e)),
    }
}

pub fn present(key: &str) -> bool {
    get(key).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a running secret store (a D-Bus session with GNOME Keyring or KWallet,
    /// or the Windows Credential Manager), so it only runs on request:
    /// `cargo test -p coucou -- --ignored secret_store`.
    #[test]
    #[ignore]
    fn secret_store_round_trip() {
        let key = "supabase-token";
        let _ = clear(key);
        assert!(!present(key));
        set(key, "sbp_test_value").expect("the secret store must accept a value");
        assert_eq!(get(key).as_deref(), Some("sbp_test_value"));
        assert!(present(key));
        clear(key).unwrap();
        assert!(!present(key));
        // Keys outside the allow-list are refused whatever the store says.
        assert!(set("not-a-known-key", "x").is_err());
    }
}
