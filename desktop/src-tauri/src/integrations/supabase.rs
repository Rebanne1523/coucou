// Supabase pill — project health and Edge Function errors.
//
// Three optional credentials in the OS secret store, two independent checks:
//
//   supabase-url    project URL (`https://<ref>.supabase.co`) or just the ref
//   supabase-key    the project's *public* key (anon / publishable) — enough to ask
//                   the Auth API whether it is up and how fast it answers. Never
//                   needs, and the settings say not to use, the service_role key.
//   supabase-token  a personal access token for the Management API — project
//                   status, per-service health, and Edge Function 5xx errors from
//                   the last hour. It is account-wide, so it is stored like any
//                   other secret and never leaves this module except as a
//                   Bearer header to api.supabase.com.
//
// Both checks are optional; the pill works with either one. Nothing is requested
// until the URL and at least one credential exist.
//
// NOTE: the endpoint paths and response shapes below were written from the
// Management API reference *without* access to it from the build environment
// (api.supabase.com was blocked there), so they have not been run against a real
// project. Parsing is deliberately tolerant — a missing field degrades the card,
// it does not break the poll — and anything unexpected is reported in the card's
// note instead of being hidden. Verify against
// https://supabase.com/docs/reference/api before trusting a green pill.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tauri::AppHandle;

use super::{client, emit, is_new, IntegrationEvent, IntegrationUpdate};
use crate::secrets;

const ID: &str = "integration_supabase";
const MANAGEMENT_API: &str = "https://api.supabase.com/v1";
/// Every service the health endpoint can be asked about.
const SERVICES: [&str; 6] = ["auth", "db", "pooler", "realtime", "rest", "storage"];

/// Edge Function invocations that ended in a 5xx, newest first. Mirrors the
/// dashboard's own Edge Functions log query.
const ERRORS_SQL: &str = "select id, function_edge_logs.timestamp, event_message, \
response.status_code, request.method, m.function_id \
from function_edge_logs \
cross join unnest(metadata) as m \
cross join unnest(m.response) as response \
cross join unnest(m.request) as request \
where response.status_code >= 500 \
order by timestamp desc limit 5";

// ── Input handling ────────────────────────────────────────────────────────────

/// The project's base URL, without a trailing slash. A bare ref (`abcdefgh`) is
/// expanded to `https://abcdefgh.supabase.co`. Plain http is only accepted for a
/// local `supabase start` stack — a key must not travel in clear text otherwise.
fn base_url(input: &str) -> Option<String> {
    let input = input.trim().trim_end_matches('/');
    if input.is_empty() {
        return None;
    }
    if !input.contains('.') && !input.contains(':') && !input.contains('/') {
        return is_ref(input).then(|| format!("https://{input}.supabase.co"));
    }
    let local = ["http://localhost", "http://127.0.0.1"]
        .iter()
        .any(|p| input.starts_with(p));
    if input.starts_with("https://") || local {
        Some(input.to_string())
    } else {
        None
    }
}

fn is_ref(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// The project ref, when it can be known: a bare ref, or the first label of a
/// `*.supabase.co` host. Custom domains and self-hosted stacks have none, and the
/// Management API checks are skipped for them.
fn project_ref(input: &str) -> Option<String> {
    let input = input.trim().trim_end_matches('/');
    if is_ref(input) {
        return Some(input.to_string());
    }
    let host = input.strip_prefix("https://")?.split('/').next()?;
    let label = host.strip_suffix(".supabase.co")?;
    is_ref(label).then(|| label.to_string())
}

// ── Time ──────────────────────────────────────────────────────────────────────

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `2023-11-14T22:13:20Z` — no date library for one timestamp format.
fn iso_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Days since 1970-01-01 → civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

// ── Checks ────────────────────────────────────────────────────────────────────

/// Is the project's Auth API answering, and how fast? Needs only the public key.
async fn probe_auth(base: &str, key: &str) -> Value {
    let started = Instant::now();
    let response = client()
        .get(format!("{base}/auth/v1/health"))
        .header("apikey", key)
        .send()
        .await;
    let ms = started.elapsed().as_millis() as u64;
    match response {
        Ok(r) => {
            let status = r.status().as_u16();
            json!({ "ok": r.status().is_success(), "ms": ms, "status": status })
        }
        Err(_) => json!({ "ok": false, "ms": ms, "status": 0 }),
    }
}

fn management_error(status: u16) -> String {
    match status {
        401 => "Invalid access token (401)".into(),
        403 => "This token can't access the project (403)".into(),
        404 => "Project not found — check the project URL (404)".into(),
        429 => "Supabase rate limit — slowing down (429)".into(),
        other => format!("Supabase API error {other}"),
    }
}

async fn management_get(
    path: &str,
    token: &str,
    query: &[(&str, &str)],
) -> Result<Value, u16> {
    let response = client()
        .get(format!("{MANAGEMENT_API}{path}"))
        .bearer_auth(token)
        .header("Accept", "application/json")
        .query(query)
        .send()
        .await
        .map_err(|_| 0u16)?;
    if !response.status().is_success() {
        return Err(response.status().as_u16());
    }
    response.json::<Value>().await.map_err(|_| 0u16)
}

/// `{ name, region, status }` from the project object.
fn parse_project(v: &Value) -> Value {
    json!({
        "name": v.get("name").and_then(Value::as_str).unwrap_or("Supabase project"),
        "region": v.get("region").and_then(Value::as_str),
        "status": v.get("status").and_then(Value::as_str).unwrap_or("UNKNOWN"),
    })
}

/// One entry per service the API reported, in the order we asked for them.
fn parse_services(v: &Value) -> Vec<Value> {
    let Some(list) = v.as_array() else { return Vec::new() };
    let mut out: Vec<Value> = list
        .iter()
        .filter_map(|s| {
            let name = s.get("name")?.as_str()?;
            let status = s.get("status").and_then(Value::as_str).unwrap_or("");
            let healthy = s
                .get("healthy")
                .and_then(Value::as_bool)
                .unwrap_or(status == "ACTIVE_HEALTHY");
            Some(json!({ "name": name, "healthy": healthy, "status": status }))
        })
        .collect();
    out.sort_by_key(|s| {
        SERVICES
            .iter()
            .position(|n| Some(*n) == s["name"].as_str())
            .unwrap_or(usize::MAX)
    });
    out
}

/// Rows of the error query → what the card shows.
fn parse_errors(v: &Value) -> Vec<Value> {
    let Some(rows) = v.get("result").and_then(Value::as_array) else { return Vec::new() };
    rows.iter()
        .filter_map(|r| {
            let id = r.get("id")?.as_str()?;
            let message = r.get("event_message").and_then(Value::as_str).unwrap_or("");
            // The message reads "POST | 500 | … | https://<ref>.supabase.co/functions/v1/<name> | …"
            let function = message
                .split("/functions/v1/")
                .nth(1)
                .and_then(|rest| rest.split(['|', ' ', '?', '/']).next())
                .filter(|n| !n.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    r.get("function_id")
                        .and_then(Value::as_str)
                        .map(|f| f.chars().take(8).collect())
                })
                .unwrap_or_else(|| "Edge Function".into());
            // BigQuery timestamps are microseconds since the epoch.
            let micros = r.get("timestamp").and_then(|t| {
                t.as_i64().or_else(|| t.as_f64().map(|f| f as i64)).or_else(|| t.as_str()?.parse().ok())
            });
            Some(json!({
                "id": id,
                "function": function,
                "status": r.get("status_code").and_then(Value::as_i64).unwrap_or(500),
                "method": r.get("method").and_then(Value::as_str).unwrap_or(""),
                "at": micros.map(|m| m / 1_000),
            }))
        })
        .collect()
}

/// What is wrong, as a stable comma-separated signature — empty when all is well.
/// It is what the island compares between polls to tell "newly broken" from "still
/// broken", so the order has to be stable.
fn health_signature(project: Option<&Value>, services: &[Value], auth: Option<&Value>) -> String {
    let mut bad: Vec<String> = Vec::new();
    if let Some(status) = project.and_then(|p| p["status"].as_str()) {
        // The management status is also ACTIVE_HEALTHY / COMING_UP while services
        // start; only a stopped project is a problem in itself.
        if matches!(status, "INACTIVE" | "PAUSED" | "PAUSING" | "REMOVED" | "GOING_DOWN") {
            bad.push(status.to_lowercase());
        }
    }
    for s in services {
        if s["healthy"] == json!(false) && s["status"].as_str() != Some("COMING_UP") {
            if let Some(name) = s["name"].as_str() {
                bad.push(name.to_string());
            }
        }
    }
    if auth.map(|a| a["ok"] == json!(false)).unwrap_or(false) {
        bad.push("auth-api".into());
    }
    bad.sort();
    bad.dedup();
    bad.join(",")
}

// ── Poll ──────────────────────────────────────────────────────────────────────

pub async fn poll(app: AppHandle) {
    let Some(raw_url) = secrets::get("supabase-url") else { return };
    let key = secrets::get("supabase-key");
    let token = secrets::get("supabase-token");

    let fail = |message: &str| {
        emit(&app, IntegrationUpdate {
            id: ID,
            data: json!({}),
            error: Some(message.to_string()),
            event: None,
        });
    };
    let Some(base) = base_url(&raw_url) else {
        return fail("Project URL must be https://<ref>.supabase.co");
    };
    if key.is_none() && token.is_none() {
        return fail("Add a project key or an access token in Settings");
    }
    let reference = project_ref(&raw_url);

    let auth = match &key {
        Some(k) => Some(probe_auth(&base, k).await),
        None => None,
    };

    let mut note: Option<String> = None;
    let mut project: Option<Value> = None;
    let mut services: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut errors_known = false;

    match (&token, &reference) {
        (Some(token), Some(r)) => {
            match management_get(&format!("/projects/{r}"), token, &[]).await {
                Ok(v) => project = Some(parse_project(&v)),
                Err(code) => note = Some(management_error(code)),
            }
            if project.is_some() {
                let pairs: Vec<(&str, &str)> = SERVICES.iter().map(|s| ("services", *s)).collect();
                if let Ok(v) = management_get(&format!("/projects/{r}/health"), token, &pairs).await {
                    services = parse_services(&v);
                }
                let now = now_secs();
                let start = iso_utc(now - 3_600);
                let end = iso_utc(now);
                let query = [
                    ("sql", ERRORS_SQL),
                    ("iso_timestamp_start", start.as_str()),
                    ("iso_timestamp_end", end.as_str()),
                ];
                match management_get(&format!("/projects/{r}/analytics/endpoints/logs.all"), token, &query).await {
                    Ok(v) if v.get("error").map(|e| e.is_null()).unwrap_or(true) => {
                        errors = parse_errors(&v);
                        errors_known = true;
                    }
                    _ => note = note.or(Some("Edge Function logs unavailable".into())),
                }
            }
        }
        (Some(_), None) => {
            note = Some("Custom domain: add the project ref as the URL to read health and logs".into());
        }
        _ => {}
    }

    // Nothing usable at all: say why instead of showing an empty card.
    let key_down = auth.as_ref().map(|a| a["ok"] == json!(false)).unwrap_or(false);
    if project.is_none() && (auth.is_none() || key_down) && token.is_some() && key.is_none() {
        return fail(note.as_deref().unwrap_or("Can't reach Supabase"));
    }
    if let Some(a) = &auth {
        if matches!(a["status"].as_u64(), Some(401 | 403)) {
            note = note.or(Some("Project key rejected — use the anon / publishable key".into()));
        }
    }

    let signature = health_signature(project.as_ref(), &services, auth.as_ref());
    let label = project
        .as_ref()
        .and_then(|p| p["name"].as_str())
        .unwrap_or("Supabase")
        .to_string();

    // One event per update. A change in health outranks a new function error.
    let health_changed = is_new("supabase-health", &signature);
    let newest_error = errors.first().and_then(|e| e["id"].as_str()).unwrap_or("none").to_string();
    let error_is_new = errors_known && is_new("supabase-fn", &newest_error) && newest_error != "none";

    let event = if health_changed {
        Some(IntegrationEvent {
            success: signature.is_empty(),
            label,
            detail: Some(if signature.is_empty() {
                "Everything is healthy again".to_string()
            } else {
                format!("Down: {}", signature.replace(',', ", "))
            }),
        })
    } else if error_is_new {
        let e = &errors[0];
        Some(IntegrationEvent {
            success: false,
            label: e["function"].as_str().unwrap_or("Edge Function").to_string(),
            detail: Some(format!(
                "{} {}",
                e["method"].as_str().unwrap_or(""),
                e["status"]
            ).trim().to_string()),
        })
    } else {
        None
    };

    emit(&app, IntegrationUpdate {
        id: ID,
        data: json!({
            "project": project,
            "services": services,
            "auth": auth,
            "errors": errors,
            "errorsKnown": errors_known,
            "note": note,
            "ref": reference,
        }),
        error: None,
        event,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_refs() {
        assert_eq!(base_url("abcdefgh").as_deref(), Some("https://abcdefgh.supabase.co"));
        assert_eq!(base_url("https://abcdefgh.supabase.co/").as_deref(), Some("https://abcdefgh.supabase.co"));
        assert_eq!(base_url("http://localhost:54321").as_deref(), Some("http://localhost:54321"));
        // A key never goes over plain http to a remote host.
        assert_eq!(base_url("http://abcdefgh.supabase.co"), None);
        assert_eq!(base_url("javascript:alert(1)"), None);
        assert_eq!(base_url("  "), None);

        assert_eq!(project_ref("abcdefgh").as_deref(), Some("abcdefgh"));
        assert_eq!(project_ref("https://abcdefgh.supabase.co").as_deref(), Some("abcdefgh"));
        assert_eq!(project_ref("https://db.example.com"), None);
        assert_eq!(project_ref("https://evil.com/.supabase.co"), None);
    }

    #[test]
    fn timestamps_are_formatted_in_utc() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z"); // leap day
        assert_eq!(iso_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(iso_utc(1_791_000_000), "2026-10-03T04:00:00Z");
    }

    #[test]
    fn health_is_read_tolerantly() {
        let services = parse_services(&json!([
            { "name": "storage", "healthy": true, "status": "ACTIVE_HEALTHY" },
            { "name": "db", "healthy": false, "status": "UNHEALTHY" },
            { "name": "auth", "status": "ACTIVE_HEALTHY" },
            { "name": "realtime", "healthy": false, "status": "COMING_UP" },
            { "nope": 1 },
        ]));
        let names: Vec<&str> = services.iter().filter_map(|s| s["name"].as_str()).collect();
        assert_eq!(names, ["auth", "db", "realtime", "storage"]);
        // `healthy` missing → derived from the status; COMING_UP is not an outage.
        assert_eq!(health_signature(None, &services, None), "db");
        assert_eq!(parse_services(&json!({ "unexpected": true })).len(), 0);
    }

    #[test]
    fn a_paused_project_or_dead_auth_api_is_an_outage() {
        let paused = parse_project(&json!({ "name": "shop", "status": "INACTIVE" }));
        assert_eq!(health_signature(Some(&paused), &[], None), "inactive");
        let auth = json!({ "ok": false, "ms": 10, "status": 0 });
        assert_eq!(health_signature(None, &[], Some(&auth)), "auth-api");
        let fine = parse_project(&json!({ "status": "ACTIVE_HEALTHY" }));
        assert_eq!(health_signature(Some(&fine), &[], Some(&json!({ "ok": true }))), "");
    }

    #[test]
    fn function_errors_name_the_function() {
        let rows = parse_errors(&json!({ "result": [
            { "id": "a1", "timestamp": 1_791_000_000_000_000i64, "status_code": 502,
              "method": "POST", "function_id": "11111111-2222",
              "event_message": "POST | 502 | 1.2.3.4 | req | https://abc.supabase.co/functions/v1/send-invoice | UA" },
            { "id": "b2", "timestamp": "1790999000000000", "status_code": 500, "method": "GET",
              "function_id": "deadbeef-0000", "event_message": "no url here" },
        ]}));
        assert_eq!(rows[0]["function"], "send-invoice");
        assert_eq!(rows[0]["at"], 1_791_000_000_000i64);
        assert_eq!(rows[1]["function"], "deadbeef");
        assert_eq!(rows[1]["at"], 1_790_999_000_000i64);
        assert!(parse_errors(&json!({ "error": "boom" })).is_empty());
    }
}
