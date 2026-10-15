//! Linux and Windows backends for host APIs that shell out to macOS-only tools.
//!
//! | Host API         | macOS             | Linux / Windows                                  |
//! |------------------|-------------------|--------------------------------------------------|
//! | `host.keychain`  | `security` CLI    | Secret Service / Windows Credential Manager      |
//! | `host.sqlite`    | `sqlite3` CLI     | bundled SQLite (`rusqlite`)                       |
//! | `host.ls`        | `ps` + `lsof`     | `sysinfo` + `/proc/net/tcp*` / `netstat -ano`    |
//!
//! Results are shaped like the macOS tools' output so plugins see no difference.

use std::path::Path;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

pub fn current_user() -> String {
    ["USER", "USERNAME"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "openusage-user".to_string())
}

/// Entries to try for `service` + `account`.
///
/// Besides keyring-rs' own layout, Windows also gets the `service:account`
/// target name that go-keyring (used by the GitHub CLI and others) writes.
fn entries(service: &str, account: &str) -> Vec<keyring::Entry> {
    let mut entries = Vec::new();
    if let Ok(entry) = keyring::Entry::new(service, account) {
        entries.push(entry);
    }
    #[cfg(windows)]
    if let Ok(entry) =
        keyring::Entry::new_with_target(&format!("{service}:{account}"), service, account)
    {
        entries.push(entry);
    }
    entries
}

/// Read a secret. Without an explicit account, tries the current user and then
/// an empty account name (go-keyring's default).
pub fn read_password(service: &str, account: Option<&str>) -> Result<String, String> {
    let accounts = match account {
        Some(account) => vec![account.to_string()],
        None => vec![current_user(), String::new()],
    };

    let mut last_error = String::from("item not found");
    for account in &accounts {
        for entry in entries(service, account) {
            match entry.get_password() {
                Ok(value) => return Ok(value.trim().to_string()),
                Err(keyring::Error::NoEntry) => {}
                Err(error) => last_error = error.to_string(),
            }
        }
    }
    Err(last_error)
}

pub fn write_password(service: &str, account: Option<&str>, value: &str) -> Result<(), String> {
    let account = account.map(str::to_string).unwrap_or_else(current_user);
    keyring::Entry::new(service, &account)
        .and_then(|entry| entry.set_password(value))
        .map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// SQLite
// ---------------------------------------------------------------------------

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(2);

fn row_value(value: rusqlite::types::ValueRef<'_>) -> serde_json::Value {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Null => serde_json::Value::Null,
        ValueRef::Integer(n) => serde_json::Value::from(n),
        ValueRef::Real(n) => serde_json::Value::from(n),
        // `sqlite3 -json` prints text and blobs alike as JSON strings; VS Code-style
        // `state.vscdb` files store their values as UTF-8 blobs.
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            serde_json::Value::String(String::from_utf8_lossy(bytes).into_owned())
        }
    }
}

fn query_connection(conn: &rusqlite::Connection, sql: &str) -> rusqlite::Result<String> {
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    let mut stmt = conn.prepare(sql)?;
    let columns: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut object = serde_json::Map::new();
        for (index, column) in columns.iter().enumerate() {
            object.insert(column.clone(), row_value(row.get_ref(index)?));
        }
        out.push(serde_json::Value::Object(object));
    }
    Ok(serde_json::Value::Array(out).to_string())
}

fn immutable_uri(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let encoded = normalized
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('#', "%23")
        .replace('?', "%3F");
    // Windows drive paths need a leading slash in file URIs: file:/C:/...
    if encoded.as_bytes().get(1) == Some(&b':') {
        format!("file:/{encoded}?immutable=1")
    } else {
        format!("file:{encoded}?immutable=1")
    }
}

/// Read-only query returning a JSON array of row objects, like `sqlite3 -readonly -json`.
pub fn sqlite_query(path: &str, sql: &str) -> Result<String, String> {
    use rusqlite::{Connection, OpenFlags};

    let read_only = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    // Prefer a normal read-only open so WAL contents are visible, then fall back
    // to immutable=1 to get past WAL/SHM lock issues.
    let primary = Connection::open_with_flags(path, read_only)
        .and_then(|conn| query_connection(&conn, sql));
    match primary {
        Ok(json) => Ok(json),
        Err(primary_error) => {
            Connection::open_with_flags(immutable_uri(path), read_only | OpenFlags::SQLITE_OPEN_URI)
                .and_then(|conn| query_connection(&conn, sql))
                .map_err(|fallback_error| {
                    format!("sqlite3 error: {primary_error} (fallback: {fallback_error})")
                })
        }
    }
}

pub fn sqlite_exec(path: &str, sql: &str) -> Result<(), String> {
    use rusqlite::{Connection, OpenFlags};

    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("sqlite3 error: {error}"))?;
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)
        .and_then(|_| conn.execute_batch(sql))
        .map_err(|error| format!("sqlite3 error: {error}"))
}

// ---------------------------------------------------------------------------
// Processes and listening ports
// ---------------------------------------------------------------------------

fn join_command_line(argv: &[std::ffi::OsString]) -> String {
    let mut parts: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    // Keep argv0 parseable when the executable path contains spaces.
    if let Some(argv0) = parts.first_mut() {
        if argv0.contains(char::is_whitespace) {
            *argv0 = format!("\"{argv0}\"");
        }
    }
    parts.join(" ")
}

/// `(pid, command line)` for every visible process, like `ps -ax -o pid=,command=`.
pub fn list_processes() -> Vec<(i32, String)> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_exe(UpdateKind::Always),
    );

    system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            let pid = i32::try_from(pid.as_u32()).ok()?;
            let mut command = join_command_line(process.cmd());
            if command.is_empty() {
                command = process
                    .exe()
                    .map(|exe| exe.to_string_lossy().into_owned())
                    .unwrap_or_default();
            }
            (!command.is_empty()).then_some((pid, command))
        })
        .collect()
}

/// TCP ports `pid` is listening on, or `None` if they cannot be determined.
pub fn listening_ports(pid: i32) -> Option<Vec<i32>> {
    #[cfg(target_os = "linux")]
    {
        linux_listening_ports(pid)
    }
    #[cfg(windows)]
    {
        windows_listening_ports(pid)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = pid;
        None
    }
}

/// Socket inodes of listening TCP sockets, mapped to their port, from the text of
/// `/proc/net/tcp` or `/proc/net/tcp6`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_net_tcp_listeners(text: &str) -> Vec<(u64, i32)> {
    const TCP_LISTEN: &str = "0A";
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 10 || fields[3] != TCP_LISTEN {
                return None;
            }
            let port_hex = fields[1].rsplit(':').next()?;
            let port = i32::from_str_radix(port_hex, 16).ok()?;
            let inode = fields[9].parse::<u64>().ok()?;
            (inode != 0 && port > 0).then_some((inode, port))
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn linux_listening_ports(pid: i32) -> Option<Vec<i32>> {
    use std::collections::{BTreeSet, HashMap};

    let mut by_inode: HashMap<u64, i32> = HashMap::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(text) = std::fs::read_to_string(table) {
            by_inode.extend(parse_proc_net_tcp_listeners(&text));
        }
    }

    let fd_dir = Path::new("/proc").join(pid.to_string()).join("fd");
    let mut ports = BTreeSet::new();
    for entry in std::fs::read_dir(fd_dir).ok()?.flatten() {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let target = target.to_string_lossy();
        let Some(inode) = target
            .strip_prefix("socket:[")
            .and_then(|rest| rest.strip_suffix(']'))
            .and_then(|inode| inode.parse::<u64>().ok())
        else {
            continue;
        };
        if let Some(port) = by_inode.get(&inode) {
            ports.insert(*port);
        }
    }
    Some(ports.into_iter().collect())
}

/// Listening ports owned by `pid` in `netstat -ano -p TCP[v6]` output.
///
/// The state column is localized, so a listening socket is recognized by its
/// unspecified foreign address (`0.0.0.0:0` / `[::]:0`) instead.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_netstat_listeners(text: &str, pid: i32) -> Vec<i32> {
    let mut ports = std::collections::BTreeSet::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 5 || !fields[0].eq_ignore_ascii_case("TCP") {
            continue;
        }
        if fields[fields.len() - 1].parse::<i32>().ok() != Some(pid) {
            continue;
        }
        let foreign = fields[2];
        if foreign != "0.0.0.0:0" && foreign != "[::]:0" {
            continue;
        }
        if let Some(port) = fields[1]
            .rsplit(':')
            .next()
            .and_then(|port| port.parse::<i32>().ok())
        {
            if port > 0 {
                ports.insert(port);
            }
        }
    }
    ports.into_iter().collect()
}

#[cfg(windows)]
fn windows_listening_ports(pid: i32) -> Option<Vec<i32>> {
    let mut ports = std::collections::BTreeSet::new();
    let mut any_ok = false;
    for protocol in ["TCP", "TCPv6"] {
        let output = super::host_api::host_command("netstat")
            .args(["-ano", "-p", protocol])
            .output()
            .ok()?;
        if !output.status.success() {
            continue;
        }
        any_ok = true;
        ports.extend(parse_netstat_listeners(
            &String::from_utf8_lossy(&output.stdout),
            pid,
        ));
    }
    any_ok.then(|| ports.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_net_tcp_listening_sockets_only() {
        let text = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:A474 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 41234 1 0000000000000000 100 0 0 10 0
   1: 0100007F:A475 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 41235 1 0000000000000000 20 4 30 10 -1
";
        assert_eq!(parse_proc_net_tcp_listeners(text), vec![(41234, 42100)]);
    }

    #[test]
    fn parses_netstat_listeners_for_one_pid() {
        let text = "
Active Connections

  Proto  Local Address          Foreign Address        State           PID
  TCP    127.0.0.1:42100        0.0.0.0:0              LISTENING       4321
  TCP    127.0.0.1:42101        0.0.0.0:0              DİNLİYOR        4321
  TCP    127.0.0.1:42100        127.0.0.1:50000        ESTABLISHED     4321
  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       999
  TCP    [::1]:42102            [::]:0                 LISTENING       4321
";
        assert_eq!(parse_netstat_listeners(text, 4321), vec![42100, 42101, 42102]);
    }

    #[test]
    fn immutable_uri_handles_unix_and_windows_paths() {
        assert_eq!(
            immutable_uri("/home/me/.config/Cursor/state.vscdb"),
            "file:/home/me/.config/Cursor/state.vscdb?immutable=1"
        );
        assert_eq!(
            immutable_uri(r"C:\Users\Me Too\AppData\Roaming\Cursor\state.vscdb"),
            "file:/C:/Users/Me%20Too/AppData/Roaming/Cursor/state.vscdb?immutable=1"
        );
    }

    #[test]
    fn sqlite_query_returns_rows_as_json_like_the_cli() {
        let dir = std::env::temp_dir().join(format!("openusage-sqlite-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.vscdb");
        let db = db.to_string_lossy().to_string();
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE ItemTable (key TEXT, value BLOB);
                 INSERT INTO ItemTable VALUES ('token', CAST('{\"a\":1}' AS BLOB));",
            )
            .unwrap();
        }

        let json = sqlite_query(&db, "SELECT key, value, 7 AS n FROM ItemTable").unwrap();
        let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            rows,
            serde_json::json!([{ "key": "token", "value": "{\"a\":1}", "n": 7 }])
        );

        sqlite_exec(&db, "DELETE FROM ItemTable").unwrap();
        assert_eq!(sqlite_query(&db, "SELECT * FROM ItemTable").unwrap(), "[]");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn join_command_line_quotes_argv0_with_spaces() {
        let argv = [
            std::ffi::OsString::from(r"C:\Program Files\Agy\language_server.exe"),
            std::ffi::OsString::from("--csrf_token"),
            std::ffi::OsString::from("abc"),
        ];
        assert_eq!(
            join_command_line(&argv),
            r#""C:\Program Files\Agy\language_server.exe" --csrf_token abc"#
        );
    }
}
