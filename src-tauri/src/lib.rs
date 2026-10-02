use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Local};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use ssh2::{HashType, Session, Sftp};
use std::{
    collections::HashMap,
    fs::{self, File},
    net::TcpStream,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::Duration,
};
use tauri::State;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    id: String,
    alias: String,
    host: String,
    port: u16,
    username: String,
    #[serde(default)]
    private_key: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Preferences {
    connections: Vec<Connection>,
    favorites: HashMap<String, Vec<String>>,
    last_connection_id: Option<String>,
    last_paths: HashMap<String, String>,
    #[serde(default)]
    trusted_hosts: HashMap<String, String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteFile {
    name: String,
    full_path: String,
    is_directory: bool,
    size: String,
    last_modified: String,
}

#[derive(Clone)]
struct EditFile {
    remote_path: String,
    local_path: PathBuf,
    opened_at: std::time::SystemTime,
}

struct Connected {
    _session: Session,
    sftp: Sftp,
    connection_id: String,
    current_path: String,
    edits: Vec<EditFile>,
}

impl Drop for Connected {
    fn drop(&mut self) {
        for edit in &self.edits {
            let _ = fs::remove_file(&edit.local_path);
        }
    }
}

struct AppState {
    prefs: Mutex<Preferences>,
    connected: Mutex<Option<Connected>>,
}

fn config_path() -> Result<PathBuf, String> {
    let root = dirs::config_dir().ok_or("Could not locate the user config directory")?;
    Ok(root.join("SftpClientTauri").join("settings.json"))
}

fn load_preferences() -> Preferences {
    config_path()
        .ok()
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_preferences(prefs: &Preferences) -> Result<(), String> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(err)?;
    }
    let bytes = serde_json::to_vec_pretty(prefs).map_err(err)?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, bytes).map_err(err)?;
    fs::rename(&temp, &path)
        .or_else(|_| {
            fs::copy(&temp, &path)?;
            fs::remove_file(temp)
        })
        .map_err(err)
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn credential_entry(connection: &Connection) -> Result<Entry, String> {
    Entry::new(
        "dev.sftpclient.desktop",
        &format!(
            "{}@{}:{}",
            connection.username, connection.host, connection.port
        ),
    )
    .map_err(err)
}

fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return "/".into();
    }
    format!("/{}/", trimmed.trim_matches('/'))
}

fn path_join(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{child}")
    } else {
        format!("{}{child}", normalize_path(parent))
    }
}

fn format_size(size: u64) -> String {
    const KB: f64 = 1024.0;
    let bytes = size as f64;
    if bytes < KB {
        format!("{size} B")
    } else if bytes < KB * KB {
        format!("{:.1} KB", bytes / KB)
    } else if bytes < KB * KB * KB {
        format!("{:.1} MB", bytes / KB / KB)
    } else {
        format!("{:.1} GB", bytes / KB / KB / KB)
    }
}

#[tauri::command]
fn get_initial_data(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let prefs = state.prefs.lock().map_err(err)?;
    Ok(serde_json::json!({
        "connections": prefs.connections,
        "favorites": prefs.favorites,
        "lastConnectionId": prefs.last_connection_id,
        "lastPaths": prefs.last_paths,
    }))
}

#[tauri::command]
fn save_connection(
    alias: String,
    host: String,
    port: u16,
    username: String,
    password: String,
    private_key: Option<String>,
    id: Option<String>,
    state: State<'_, AppState>,
) -> Result<Connection, String> {
    if alias.trim().is_empty() || host.trim().is_empty() || username.trim().is_empty() {
        return Err("Alias, host, and username are required".into());
    }
    let mut prefs = state.prefs.lock().map_err(err)?;
    let existing = id.as_ref().and_then(|id| {
        prefs
            .connections
            .iter()
            .find(|connection| &connection.id == id)
            .cloned()
    });
    let connection = Connection {
        id: id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        alias: alias.trim().to_string(),
        host: host.trim().to_string(),
        port,
        username: username.trim().to_string(),
        private_key: private_key.filter(|key| !key.trim().is_empty()),
    };
    let secret = if password.is_empty() {
        existing
            .as_ref()
            .and_then(|old| credential_entry(old).ok())
            .and_then(|entry| entry.get_password().ok())
    } else {
        Some(password)
    };
    if let Some(secret) = secret {
        credential_entry(&connection)?
            .set_password(&secret)
            .map_err(err)?;
    }
    if let Some(old) = existing {
        let old_key = format!("{}@{}:{}", old.username, old.host, old.port);
        let new_key = format!(
            "{}@{}:{}",
            connection.username, connection.host, connection.port
        );
        if old_key != new_key {
            if let Ok(entry) = credential_entry(&old) {
                let _ = entry.delete_credential();
            }
        }
    }
    if let Some(index) = prefs
        .connections
        .iter()
        .position(|item| item.id == connection.id)
    {
        prefs.connections[index] = connection.clone();
    } else {
        prefs.connections.push(connection.clone());
    }
    save_preferences(&prefs)?;
    Ok(connection)
}

#[tauri::command]
fn delete_connection(id: String, state: State<'_, AppState>) -> Result<(), String> {
    let mut prefs = state.prefs.lock().map_err(err)?;
    if let Some(connection) = prefs.connections.iter().find(|item| item.id == id) {
        if let Ok(entry) = credential_entry(connection) {
            let _ = entry.delete_credential();
        }
    }
    prefs.connections.retain(|item| item.id != id);
    prefs.favorites.retain(|key, _| key != &id);
    prefs.last_paths.remove(&id);
    if prefs.last_connection_id.as_deref() == Some(&id) {
        prefs.last_connection_id = None;
    }
    save_preferences(&prefs)
}

#[tauri::command]
fn connect(id: String, state: State<'_, AppState>) -> Result<String, String> {
    let connection = {
        let prefs = state.prefs.lock().map_err(err)?;
        prefs
            .connections
            .iter()
            .find(|item| item.id == id)
            .cloned()
            .ok_or("Connection was not found")?
    };
    let password = credential_entry(&connection)?.get_password().ok();
    let address = format!("{}:{}", connection.host, connection.port);
    let tcp =
        TcpStream::connect(&address).map_err(|e| format!("Could not reach {address}: {e}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(err)?;
    tcp.set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(err)?;
    let mut session = Session::new().map_err(err)?;
    session.set_tcp_stream(tcp);
    session.handshake().map_err(err)?;
    let fingerprint = session
        .host_key_hash(HashType::Sha256)
        .map(|hash| STANDARD.encode(hash))
        .ok_or("The server did not provide a host key")?;
    let host_key_id = format!("{}:{}", connection.host.to_lowercase(), connection.port);
    let trusted = {
        let prefs = state.prefs.lock().map_err(err)?;
        prefs.trusted_hosts.get(&host_key_id).cloned()
    };
    match trusted {
        Some(saved) if saved != fingerprint => return Err(format!("Server host key changed for {}. Expected SHA256:{saved}, received SHA256:{fingerprint}.", connection.host)),
        None => return Err(format!("HOST_KEY_CONFIRM:{fingerprint}")),
        _ => {}
    }
    let authenticated = if let Some(key) = connection.private_key.as_deref() {
        session
            .userauth_pubkey_file(
                &connection.username,
                None,
                Path::new(key),
                password.as_deref(),
            )
            .is_ok()
    } else {
        false
    };
    if !authenticated {
        if let Some(secret) = password.as_deref() {
            session
                .userauth_password(&connection.username, secret)
                .map_err(err)?;
        } else {
            return Err("No saved password. Edit this connection and enter its password.".into());
        }
    }
    if !session.authenticated() {
        return Err("SSH authentication failed".into());
    }
    let sftp = session.sftp().map_err(err)?;
    let home = sftp
        .realpath(Path::new("."))
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/".into());
    let home = normalize_path(&home);
    let last_path = {
        let mut prefs = state.prefs.lock().map_err(err)?;
        prefs.last_connection_id = Some(id.clone());
        let path = prefs.last_paths.get(&id).cloned().unwrap_or(home);
        save_preferences(&prefs)?;
        path
    };
    *state.connected.lock().map_err(err)? = Some(Connected {
        _session: session,
        sftp,
        connection_id: id,
        current_path: last_path.clone(),
        edits: Vec::new(),
    });
    Ok(last_path)
}

#[tauri::command]
fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    *state.connected.lock().map_err(err)? = None;
    Ok(())
}

#[tauri::command]
fn trust_host(id: String, fingerprint: String, state: State<'_, AppState>) -> Result<(), String> {
    let mut prefs = state.prefs.lock().map_err(err)?;
    let connection = prefs
        .connections
        .iter()
        .find(|item| item.id == id)
        .ok_or("Connection was not found")?;
    let key = format!("{}:{}", connection.host.to_lowercase(), connection.port);
    prefs.trusted_hosts.insert(key, fingerprint);
    save_preferences(&prefs)
}

#[tauri::command]
fn list_directory(path: String, state: State<'_, AppState>) -> Result<Vec<RemoteFile>, String> {
    let mut connected = state.connected.lock().map_err(err)?;
    let client = connected.as_mut().ok_or("Not connected")?;
    let path = normalize_path(&path);
    let mut items = Vec::new();
    for (entry_path, stat) in client.sftp.readdir(Path::new(&path)).map_err(err)? {
        let Some(name) = entry_path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if name == "." || name == ".." {
            continue;
        }
        let is_directory = stat.is_dir();
        let modified = stat
            .mtime
            .and_then(|time| DateTime::from_timestamp(time as i64, 0))
            .map(|time| {
                time.with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "—".into());
        items.push(RemoteFile {
            name: name.to_string(),
            full_path: path_join(&path, name),
            is_directory,
            size: if is_directory {
                "<DIR>".into()
            } else {
                format_size(stat.size.unwrap_or(0))
            },
            last_modified: modified,
        });
    }
    items.sort_by(|a, b| {
        b.is_directory
            .cmp(&a.is_directory)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    client.current_path = path;
    let prefs = state.prefs.lock().map_err(err)?;
    let mut prefs = prefs;
    prefs
        .last_paths
        .insert(client.connection_id.clone(), client.current_path.clone());
    save_preferences(&prefs)?;
    Ok(items)
}

#[tauri::command]
fn change_directory(path: String, state: State<'_, AppState>) -> Result<Vec<RemoteFile>, String> {
    list_directory(path, state)
}

#[tauri::command]
fn get_favorites(connection_id: String, state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let prefs = state.prefs.lock().map_err(err)?;
    Ok(prefs
        .favorites
        .get(&connection_id)
        .cloned()
        .unwrap_or_default())
}

#[tauri::command]
fn toggle_favorite(path: String, state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let connected = state.connected.lock().map_err(err)?;
    let client = connected.as_ref().ok_or("Not connected")?;
    let id = client.connection_id.clone();
    let mut prefs = state.prefs.lock().map_err(err)?;
    let updated = {
        let favorites = prefs.favorites.entry(id).or_default();
        if let Some(index) = favorites.iter().position(|item| item == &path) {
            favorites.remove(index);
        } else {
            favorites.push(path);
        }
        favorites.clone()
    };
    save_preferences(&prefs)?;
    Ok(updated)
}

#[tauri::command]
fn upload_files(paths: Vec<String>, state: State<'_, AppState>) -> Result<String, String> {
    let mut connected = state.connected.lock().map_err(err)?;
    let client = connected.as_mut().ok_or("Not connected")?;
    let mut count = 0;
    for path in paths {
        let local = PathBuf::from(&path);
        let name = local
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("Invalid local filename")?;
        let mut source = File::open(&local).map_err(err)?;
        let mut destination = client
            .sftp
            .create(Path::new(&path_join(&client.current_path, name)))
            .map_err(err)?;
        std::io::copy(&mut source, &mut destination).map_err(err)?;
        count += 1;
    }
    Ok(format!("Uploaded {count} file(s)"))
}

#[tauri::command]
fn download_files(
    paths: Vec<String>,
    destination: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let connected = state.connected.lock().map_err(err)?;
    let client = connected.as_ref().ok_or("Not connected")?;
    let folder = PathBuf::from(destination);
    fs::create_dir_all(&folder).map_err(err)?;
    let mut count = 0;
    for remote in paths {
        let name = Path::new(&remote)
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("Invalid remote filename")?;
        let mut source = client.sftp.open(Path::new(&remote)).map_err(err)?;
        let mut destination = File::create(folder.join(name)).map_err(err)?;
        std::io::copy(&mut source, &mut destination).map_err(err)?;
        count += 1;
    }
    Ok(format!("Downloaded {count} file(s)"))
}

#[tauri::command]
fn delete_remote(paths: Vec<String>, state: State<'_, AppState>) -> Result<String, String> {
    let connected = state.connected.lock().map_err(err)?;
    let client = connected.as_ref().ok_or("Not connected")?;
    let mut count = 0;
    for path in paths {
        let stat = client.sftp.stat(Path::new(&path)).map_err(err)?;
        if stat.is_dir() {
            client.sftp.rmdir(Path::new(&path)).map_err(err)?;
        } else {
            client.sftp.unlink(Path::new(&path)).map_err(err)?;
        }
        count += 1;
    }
    Ok(format!("Deleted {count} item(s)"))
}

#[tauri::command]
fn edit_remote(paths: Vec<String>, state: State<'_, AppState>) -> Result<String, String> {
    let temp = std::env::temp_dir().join("SftpClientTauri");
    fs::create_dir_all(&temp).map_err(err)?;
    let mut connected = state.connected.lock().map_err(err)?;
    let client = connected.as_mut().ok_or("Not connected")?;
    let mut locals = Vec::new();
    for remote in paths {
        let name = Path::new(&remote)
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("Invalid remote filename")?;
        let local = temp.join(format!(
            "{}_{}_{}",
            client.connection_id.replace(['/', '\\', ':'], "_"),
            uuid::Uuid::new_v4(),
            name
        ));
        let mut source = client.sftp.open(Path::new(&remote)).map_err(err)?;
        let mut destination = File::create(&local).map_err(err)?;
        std::io::copy(&mut source, &mut destination).map_err(err)?;
        let opened_at = fs::metadata(&local)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::now());
        client.edits.push(EditFile {
            remote_path: remote,
            local_path: local.clone(),
            opened_at,
        });
        locals.push(local);
    }
    for local in locals {
        open_in_editor(&local)?;
    }
    Ok("Opened selected file(s) in your local editor. Return here after editing to upload changes.".into())
}

fn open_in_editor(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = Command::new(editor_path());

    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg("-a").arg("TextEdit");
        command
    };

    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");

    command.arg(path).spawn().map_err(err)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn editor_path() -> PathBuf {
    let candidates = [
        std::env::var("ProgramFiles")
            .ok()
            .map(|root| PathBuf::from(root).join("Notepad++/notepad++.exe")),
        std::env::var("ProgramFiles(x86)")
            .ok()
            .map(|root| PathBuf::from(root).join("Notepad++/notepad++.exe")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("notepad.exe"))
}

#[tauri::command]
fn modified_edits(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let connected = state.connected.lock().map_err(err)?;
    let client = connected.as_ref().ok_or("Not connected")?;
    Ok(client
        .edits
        .iter()
        .filter(|edit| {
            fs::metadata(&edit.local_path)
                .and_then(|m| m.modified())
                .map(|time| time > edit.opened_at)
                .unwrap_or(false)
        })
        .map(|edit| edit.local_path.to_string_lossy().to_string())
        .collect())
}

#[tauri::command]
fn upload_edited(paths: Vec<String>, state: State<'_, AppState>) -> Result<String, String> {
    let mut connected = state.connected.lock().map_err(err)?;
    let client = connected.as_mut().ok_or("Not connected")?;
    let mut count = 0;
    for local_path in paths {
        let local = PathBuf::from(&local_path);
        let edit = client
            .edits
            .iter()
            .find(|entry| entry.local_path == local)
            .ok_or("Edit session expired")?
            .clone();
        let mut source = File::open(&local).map_err(err)?;
        let mut destination = client
            .sftp
            .create(Path::new(&edit.remote_path))
            .map_err(err)?;
        std::io::copy(&mut source, &mut destination).map_err(err)?;
        let _ = fs::remove_file(&local);
        client.edits.retain(|entry| entry.local_path != local);
        count += 1;
    }
    Ok(format!("Uploaded {count} edited file(s)"))
}

#[tauri::command]
fn discard_edits(paths: Vec<String>, state: State<'_, AppState>) -> Result<(), String> {
    let mut connected = state.connected.lock().map_err(err)?;
    let client = connected.as_mut().ok_or("Not connected")?;
    for local_path in paths {
        let local = PathBuf::from(local_path);
        if let Some(index) = client
            .edits
            .iter()
            .position(|entry| entry.local_path == local)
        {
            let edit = client.edits.remove(index);
            let _ = fs::remove_file(edit.local_path);
        }
    }
    Ok(())
}

#[tauri::command]
fn pick_upload_files() -> Result<Vec<String>, String> {
    Ok(rfd::FileDialog::new()
        .pick_files()
        .unwrap_or_default()
        .into_iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect())
}

#[tauri::command]
fn pick_download_folder() -> Result<Option<String>, String> {
    Ok(rfd::FileDialog::new()
        .pick_folder()
        .map(|path| path.to_string_lossy().to_string()))
}

#[tauri::command]
fn get_current_path(state: State<'_, AppState>) -> Result<String, String> {
    let connected = state.connected.lock().map_err(err)?;
    Ok(connected
        .as_ref()
        .ok_or("Not connected")?
        .current_path
        .clone())
}

#[tauri::command]
fn get_connection_password(id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let prefs = state.prefs.lock().map_err(err)?;
    let connection = prefs
        .connections
        .iter()
        .find(|item| item.id == id)
        .ok_or("Connection was not found")?;
    Ok(credential_entry(connection)?.get_password().is_ok())
}

pub fn run() {
    tauri::Builder::default()
        .manage(AppState {
            prefs: Mutex::new(load_preferences()),
            connected: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            get_initial_data,
            save_connection,
            delete_connection,
            connect,
            disconnect,
            trust_host,
            list_directory,
            change_directory,
            get_favorites,
            toggle_favorite,
            upload_files,
            download_files,
            delete_remote,
            edit_remote,
            modified_edits,
            upload_edited,
            discard_edits,
            pick_upload_files,
            pick_download_folder,
            get_current_path,
            get_connection_password
        ])
        .run(tauri::generate_context!())
        .expect("error while running SFTP Client");
}
