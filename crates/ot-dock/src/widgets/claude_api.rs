//! Claude Code usage (`AiUsageService`, Claude provider): the CLI's OAuth credential (`.credentials.json`, else
//! the "Claude Code-credentials" entry in Windows Credential Manager), a refresh when it has expired or the server
//! answers 401 (written back to the file like OpenTaskbar does), and `GET /api/oauth/usage` over WinHTTP.
//! Tokens never leave this module and are never logged.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

const HOST: &str = "api.anthropic.com";
const USAGE_PATH: &str = "/api/oauth/usage";
const TOKEN_PATH: &str = "/v1/oauth/token";
/// `ClaudeOAuthClientId`: Claude Code's public OAuth client id.
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const KEYRING_TARGET: &str = "Claude Code-credentials";
const MAX_BODY: usize = 1 << 20;

/// A usage window ("five_hour", "seven_day", "seven_day_<model>").
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub label_key: String,
    pub used: f64,
    /// Unix seconds.
    pub resets_at: Option<i64>,
    pub window_seconds: i64,
    pub severity: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub windows: Vec<Window>,
    pub plan: Option<String>,
    /// Unix seconds.
    pub fetched_at: i64,
}

/// What a load ended with: a report, or the error key and the server's Retry-After.
#[derive(Clone, Debug)]
pub enum Outcome {
    Report(Report),
    Error {
        key: &'static str,
        retry_after: Option<Duration>,
    },
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// `CredentialPath`: `CLAUDE_CONFIG_DIR`, else `%USERPROFILE%\.claude`, then `.credentials.json`.
fn credential_path() -> (PathBuf, bool) {
    match std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        Some(dir) => (PathBuf::from(dir).join(".credentials.json"), true),
        None => {
            let home = std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .unwrap_or_default();
            (home.join(".claude").join(".credentials.json"), false)
        }
    }
}

#[derive(Clone)]
struct Credential {
    token: String,
    refresh: Option<String>,
    plan: Option<String>,
    /// The file to write a refreshed token to.
    path: Option<PathBuf>,
}

/// `ParseClaudeCredential`.
fn parse_credential(json: &str, path: Option<PathBuf>) -> Option<Credential> {
    let root: Value = serde_json::from_str(json).ok()?;
    let root = root.as_object()?;
    let auth = root
        .get("claudeAiOauth")
        .and_then(Value::as_object)
        .unwrap_or(root);
    let text = |a: &str, b: &str| {
        auth.get(a)
            .or_else(|| auth.get(b))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
    };
    let mut token = text("accessToken", "access_token").unwrap_or_default();
    let refresh = text("refreshToken", "refresh_token");
    let plan = text("subscriptionType", "rateLimitTier");
    if let Some(expires) = auth.get("expiresAt").and_then(Value::as_f64)
        && expires as i64 <= now_millis()
    {
        token.clear();
    }
    if token.is_empty() && refresh.is_none() {
        return None;
    }
    Some(Credential {
        token,
        refresh,
        plan,
        path,
    })
}

/// The Credential Manager entry's text and last write time (Unix seconds).
fn read_keyring() -> Option<(String, i64)> {
    use windows_sys::Win32::Security::Credentials::{
        CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
    };
    let target: Vec<u16> = KEYRING_TARGET.encode_utf16().chain(Some(0)).collect();
    // SAFETY: CredReadW allocates the credential, freed below; the blob is read within its size.
    unsafe {
        let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
        if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) == 0
            || credential.is_null()
        {
            return None;
        }
        let c = &*credential;
        let written = (i64::from(c.LastWritten.dwHighDateTime) << 32)
            | i64::from(c.LastWritten.dwLowDateTime);
        let written = written / 10_000_000 - 11_644_473_600;
        let size = c.CredentialBlobSize as usize;
        let text = (!c.CredentialBlob.is_null() && size > 0 && size <= 1 << 16).then(|| {
            String::from_utf8_lossy(std::slice::from_raw_parts(c.CredentialBlob, size)).into_owned()
        });
        CredFree(credential.cast());
        text.map(|t| (t, written))
    }
}

/// The credential to use: the file; the keyring when the file is missing or has no usable token (only when
/// `CLAUDE_CONFIG_DIR` is not set). `None`: signed out.
fn load_credential() -> Option<Credential> {
    let (path, configured) = credential_path();
    let file = std::fs::read_to_string(&path).ok();
    let parsed = file
        .as_deref()
        .and_then(|f| parse_credential(f, Some(path.clone())));
    if configured {
        return parsed;
    }
    if file.is_none() {
        return read_keyring().and_then(|(text, _)| parse_credential(&text, None));
    }
    if parsed.as_ref().is_some_and(|c| !c.token.is_empty()) {
        return parsed;
    }
    // No usable token in the file: the keyring's, with the file as the refresh target.
    match read_keyring().and_then(|(text, _)| parse_credential(&text, Some(path.clone()))) {
        Some(keyring) if !keyring.token.is_empty() || parsed.is_none() => Some(keyring),
        _ => parsed,
    }
}

/// `CredentialStamp`: the later of the file's and the keyring entry's write times, to notice a new sign-in.
pub fn credential_stamp() -> i64 {
    let (path, _) = credential_path();
    let file = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    let keyring = read_keyring().map_or(0, |(_, written)| written);
    file.max(keyring)
}

/// One HTTPS request: (status, body, Retry-After seconds).
fn request(
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: Option<&[u8]>,
) -> Option<(u32, Vec<u8>, Option<u64>)> {
    use windows_sys::Win32::Networking::WinHttp::*;
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() };
    // SAFETY: WinHTTP calls with valid strings and buffers; every handle is closed below.
    unsafe {
        let agent = wide("OpenTaskbar");
        let session = WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if session.is_null() {
            return None;
        }
        let close = |h: *mut core::ffi::c_void| {
            if !h.is_null() {
                WinHttpCloseHandle(h);
            }
        };
        // `HttpClient.Timeout = 15 s`.
        WinHttpSetTimeouts(session, 15_000, 15_000, 15_000, 15_000);
        let host = wide(HOST);
        let connection = WinHttpConnect(session, host.as_ptr(), INTERNET_DEFAULT_HTTPS_PORT, 0);
        let verb = wide(method);
        let target = wide(path);
        let request = if connection.is_null() {
            std::ptr::null_mut()
        } else {
            WinHttpOpenRequest(
                connection,
                verb.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        };
        let mut result = None;
        if !request.is_null() {
            // No redirects, no cookies (`AllowAutoRedirect = false`, `UseCookies = false`).
            let mut features: u32 = WINHTTP_DISABLE_REDIRECTS | WINHTTP_DISABLE_COOKIES;
            WinHttpSetOption(
                request,
                WINHTTP_OPTION_DISABLE_FEATURE,
                (&mut features as *mut u32).cast(),
                4,
            );
            let header_text: String = headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}\r\n"))
                .collect();
            let header_wide: Vec<u16> = header_text.encode_utf16().collect();
            let (data, len) = body.map_or((std::ptr::null(), 0), |b| {
                (b.as_ptr().cast(), b.len() as u32)
            });
            if WinHttpSendRequest(
                request,
                header_wide.as_ptr(),
                header_wide.len() as u32,
                data,
                len,
                len,
                0,
            ) != 0
                && WinHttpReceiveResponse(request, std::ptr::null_mut()) != 0
            {
                let mut status = 0u32;
                let mut size = 4u32;
                WinHttpQueryHeaders(
                    request,
                    WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                    std::ptr::null(),
                    (&mut status as *mut u32).cast(),
                    &mut size,
                    std::ptr::null_mut(),
                );
                let mut retry = [0u16; 64];
                let mut retry_size = (retry.len() * 2) as u32;
                let retry_after = (WinHttpQueryHeaders(
                    request,
                    WINHTTP_QUERY_RETRY_AFTER,
                    std::ptr::null(),
                    retry.as_mut_ptr().cast(),
                    &mut retry_size,
                    std::ptr::null_mut(),
                ) != 0)
                    .then(|| String::from_utf16_lossy(&retry[..(retry_size / 2) as usize]))
                    .and_then(|t| t.trim().parse::<u64>().ok());
                let mut body = Vec::new();
                loop {
                    let mut available = 0u32;
                    if WinHttpQueryDataAvailable(request, &mut available) == 0 || available == 0 {
                        break;
                    }
                    let start = body.len();
                    body.resize(start + available as usize, 0);
                    let mut read = 0u32;
                    if WinHttpReadData(
                        request,
                        body[start..].as_mut_ptr().cast(),
                        available,
                        &mut read,
                    ) == 0
                    {
                        body.truncate(start);
                        break;
                    }
                    body.truncate(start + read as usize);
                    if read == 0 || body.len() > MAX_BODY {
                        break;
                    }
                }
                if body.len() <= MAX_BODY {
                    result = Some((status, body, retry_after));
                }
            }
        }
        close(request);
        close(connection);
        close(session);
        result
    }
}

/// `RefreshClaudeToken`: a new access token, written back to the credential file when it has one.
fn refresh(credential: &Credential) -> Option<Credential> {
    let refresh_token = credential.refresh.as_ref()?;
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLIENT_ID,
    })
    .to_string();
    let (status, body, _) = request(
        "POST",
        TOKEN_PATH,
        &[("Content-Type", "application/json".to_owned())],
        Some(body.as_bytes()),
    )?;
    if !(200..300).contains(&status) {
        ot_core::log::warn(format!("Claude token refresh failed (HTTP {status})."));
        return None;
    }
    let json: Value = serde_json::from_slice(&body).ok()?;
    let text = |a: &str, b: &str| {
        json.get(a)
            .or_else(|| json.get(b))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let token = text("access_token", "accessToken").filter(|t| !t.is_empty())?;
    let new_refresh = text("refresh_token", "refreshToken").or_else(|| credential.refresh.clone());
    let expires_in = json
        .get("expires_in")
        .or_else(|| json.get("expiresIn"))
        .and_then(Value::as_i64)
        .unwrap_or(28_800);
    let refreshed = Credential {
        token,
        refresh: new_refresh,
        plan: credential.plan.clone(),
        path: credential.path.clone(),
    };
    write_back(&refreshed, now_millis() + expires_in * 1000);
    Some(refreshed)
}

/// `WriteClaudeCredential`: the file's `claudeAiOauth` gets the new tokens and expiry first, its other fields
/// follow; written to a temporary file, then moved over the original.
fn write_back(credential: &Credential, expires_at_ms: i64) {
    let Some(path) = credential.path.as_ref().filter(|p| p.exists()) else {
        return;
    };
    let Some(mut root) = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object)
    else {
        return;
    };
    let mut auth = Map::new();
    auth.insert(
        "accessToken".into(),
        Value::String(credential.token.clone()),
    );
    auth.insert(
        "refreshToken".into(),
        credential
            .refresh
            .clone()
            .map_or(Value::Null, Value::String),
    );
    auth.insert("expiresAt".into(), Value::from(expires_at_ms));
    if let Some(old) = root.get("claudeAiOauth").and_then(Value::as_object) {
        for (key, value) in old {
            if !matches!(
                key.as_str(),
                "accessToken" | "access_token" | "refreshToken" | "refresh_token" | "expiresAt"
            ) {
                auth.insert(key.clone(), value.clone());
            }
        }
    }
    if let Some(object) = root.as_object_mut() {
        object.insert("claudeAiOauth".into(), Value::Object(auth));
    }
    let Ok(text) = serde_json::to_string_pretty(&root) else {
        return;
    };
    let mut temporary = path.clone().into_os_string();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    if std::fs::write(&temporary, text).is_ok() && std::fs::rename(&temporary, path).is_err() {
        let _ = std::fs::remove_file(&temporary);
        ot_core::log::warn("Claude credential could not be updated.");
    }
}

/// `Number`: a JSON number or a numeric string.
fn number(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// ISO 8601 ("2026-11-24T17:00:00.123+00:00", "…Z") to Unix seconds.
pub fn parse_iso(text: &str) -> Option<i64> {
    let t = text.trim();
    let (date, rest) = t.split_once(['T', ' '])?;
    let mut d = date.split('-');
    let (y, mo, da): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let zone_at = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (clock, zone) = rest.split_at(zone_at);
    let mut c = clock.split(':');
    let h: i64 = c.next()?.parse().ok()?;
    let mi: i64 = c.next()?.parse().ok()?;
    let s: f64 = c.next().map_or(Some(0.0), |v| v.parse().ok())?;
    let offset = match zone.chars().next() {
        Some('+') | Some('-') => {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let z = zone[1..].replace(':', "");
            let (zh, zm) = (
                z.get(0..2)?.parse::<i64>().ok()?,
                z.get(2..4).unwrap_or("0").parse::<i64>().ok()?,
            );
            sign * (zh * 3600 + zm * 60)
        }
        _ => 0,
    };
    // Days from the civil date (proleptic Gregorian).
    let (yy, mm) = if mo <= 2 {
        (y - 1, mo + 9)
    } else {
        (y, mo - 3)
    };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + s.floor() as i64 - offset)
}

/// `Parse` (Claude): the quota windows, their severities and the plan.
pub fn parse_usage(body: &[u8], plan: Option<String>) -> Option<Report> {
    let root: Value = serde_json::from_slice(body).ok()?;
    let root = root.as_object()?;
    let mut severities: Vec<(String, String)> = Vec::new();
    if let Some(limits) = root.get("limits").and_then(Value::as_array) {
        for limit in limits {
            if let (Some(group), Some(severity)) = (
                limit.get("group").and_then(Value::as_str),
                limit.get("severity").and_then(Value::as_str),
            ) && !severities.iter().any(|(g, _)| g == group)
            {
                severities.push((group.to_owned(), severity.to_owned()));
            }
        }
    }
    let severity = |group: &str| {
        severities
            .iter()
            .find(|(g, _)| g == group)
            .map(|(_, s)| s.clone())
    };
    let mut windows = Vec::new();
    for (name, value) in root {
        let (label_key, seconds, sev) = match name.as_str() {
            "five_hour" => ("AiUsageSession".to_owned(), 18_000, severity("session")),
            "seven_day" => ("AiUsageWeekly".to_owned(), 604_800, severity("weekly")),
            n if n.starts_with("seven_day_") => {
                (format!("AiUsageWeeklyModel:{}", &n[10..]), 604_800, None)
            }
            _ => continue,
        };
        let Some(object) = value.as_object() else {
            continue;
        };
        let Some(utilization) = number(object.get("utilization")) else {
            continue;
        };
        let used = if utilization.is_finite() {
            (utilization / 100.0).clamp(0.0, 1.0)
        } else {
            0.0
        };
        windows.push(Window {
            label_key,
            used,
            resets_at: object
                .get("resets_at")
                .and_then(Value::as_str)
                .and_then(parse_iso),
            window_seconds: seconds,
            severity: sev,
        });
    }
    (!windows.is_empty()).then(|| Report {
        windows,
        plan,
        fetched_at: now_unix(),
    })
}

/// One load (`LoadAsync` for Claude): credential, refresh when expired, the usage request, a refresh and one
/// retry after a 401.
pub fn load() -> Outcome {
    let Some(mut credential) = load_credential() else {
        return Outcome::Error {
            key: "ErrorAiUsageSignIn",
            retry_after: None,
        };
    };
    if credential.token.is_empty() {
        match refresh(&credential) {
            Some(fresh) => credential = fresh,
            None => {
                return Outcome::Error {
                    key: "ErrorAiUsageSignIn",
                    retry_after: None,
                };
            }
        }
    }
    let mut retried = false;
    loop {
        let headers = [
            ("Authorization", format!("Bearer {}", credential.token)),
            ("anthropic-beta", "oauth-2025-04-20".to_owned()),
        ];
        let Some((status, body, retry_after)) = request("GET", USAGE_PATH, &headers, None) else {
            return Outcome::Error {
                key: "ErrorAiUsageLoad",
                retry_after: None,
            };
        };
        match status {
            200..=299 => {
                return match parse_usage(&body, credential.plan.clone()) {
                    Some(report) => Outcome::Report(report),
                    None => Outcome::Error {
                        key: "ErrorAiUsageLoad",
                        retry_after: None,
                    },
                };
            }
            401 if !retried && credential.refresh.is_some() => {
                retried = true;
                match refresh(&credential) {
                    Some(fresh) => credential = fresh,
                    None => {
                        return Outcome::Error {
                            key: "ErrorAiUsageSignIn",
                            retry_after: None,
                        };
                    }
                }
            }
            401 => {
                return Outcome::Error {
                    key: "ErrorAiUsageSignIn",
                    retry_after: None,
                };
            }
            429 => {
                return Outcome::Error {
                    key: "ErrorAiUsageRateLimited",
                    retry_after: retry_after.map(Duration::from_secs),
                };
            }
            _ => {
                return Outcome::Error {
                    key: "ErrorAiUsageLoad",
                    retry_after: None,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_dates() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso("2026-11-24T17:00:00+03:00"),
            parse_iso("2026-11-24T14:00:00Z")
        );
        assert_eq!(
            parse_iso("2026-11-24T14:00:00.999999+00:00"),
            parse_iso("2026-11-24T14:00:00Z")
        );
    }

    #[test]
    fn usage_windows() {
        let body = br#"{"five_hour":{"utilization":43.0,"resets_at":"2026-11-24T17:00:00Z"},
            "seven_day":{"utilization":"12","resets_at":null},"seven_day_opus":{"utilization":5},
            "seven_day_breakdown":{"rows":[]},"limits":[{"group":"session","severity":"warning"}]}"#;
        let report = parse_usage(body, None).unwrap();
        assert_eq!(report.windows.len(), 3);
        assert_eq!(report.windows[0].label_key, "AiUsageSession");
        assert!((report.windows[0].used - 0.43).abs() < 1e-9);
        assert_eq!(report.windows[0].severity.as_deref(), Some("warning"));
        assert_eq!(report.windows[1].resets_at, None);
        assert_eq!(report.windows[2].label_key, "AiUsageWeeklyModel:opus");
    }

    #[test]
    fn credentials_parse_without_tokens_leaking() {
        let c = parse_credential(
            r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","expiresAt":1}}"#,
            None,
        )
        .unwrap();
        assert!(c.token.is_empty(), "an expired token is not used");
        assert_eq!(c.refresh.as_deref(), Some("r"));
        assert!(parse_credential(r#"{"claudeAiOauth":{}}"#, None).is_none());
    }
}
