import { invoke } from "@tauri-apps/api/core";
import "./style.css";

type Connection = { id: string; alias: string; host: string; port: number; username: string; privateKey?: string | null };
type RemoteFile = { name: string; fullPath: string; isDirectory: boolean; size: string; lastModified: string };
type InitialData = { connections: Connection[]; favorites: Record<string, string[]>; lastConnectionId?: string | null; lastPaths: Record<string, string> };

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const select = $("connection-select") as HTMLSelectElement;
const rows = $("file-rows");
const dialog = $("connection-dialog") as HTMLDialogElement;
let connections: Connection[] = [];
let files: RemoteFile[] = [];
let selected = new Set<string>();
let currentPath = "/";
let favorites: Record<string, string[]> = {};
let activeConnection: Connection | null = null;
let editPromptOpen = false;

function status(message: string, connected = Boolean(activeConnection), error = false) {
  $("status-text").textContent = message;
  $("status-dot").className = `status-dot${connected ? " online" : ""}${error ? " error" : ""}`;
}

function enabled(id: string, value: boolean) { ($(id) as HTMLButtonElement).disabled = !value; }

function currentSelected() { return files.filter((file) => selected.has(file.fullPath)); }

function updateActions() {
  const online = Boolean(activeConnection);
  const picked = currentSelected();
  const chosen = connections.find((connection) => connection.id === select.value);
  enabled("connect-button", Boolean(chosen));
  $("connect-button").textContent = online ? "Reconnect" : "Connect";
  enabled("edit-connection", Boolean(chosen));
  enabled("delete-connection", Boolean(chosen));
  ["up-button", "favorite-button", "refresh-button", "upload-button"].forEach((id) => enabled(id, online));
  enabled("edit-button", online && picked.some((file) => !file.isDirectory));
  enabled("download-button", online && picked.some((file) => !file.isDirectory));
  enabled("delete-button", online && picked.length > 0);
  const favorite = online && (favorites[activeConnection!.id] ?? []).includes(currentPath);
  $("favorite-button").textContent = favorite ? "★ Favorited" : "☆ Favorite";
  $("selection-hint").textContent = picked.length ? `${picked.length} selected` : online ? `${files.length} items` : "Connect to a server to browse files";
  $("item-count").textContent = online ? `${files.length} items` : "";
  $("path-text").textContent = online ? currentPath : "Not connected";
  renderFavorites();
}

function renderConnections(preferred?: string) {
  const previous = preferred ?? select.value;
  select.replaceChildren(new Option("Choose a connection", ""));
  for (const connection of connections) select.add(new Option(connection.alias, connection.id));
  if (previous && connections.some((item) => item.id === previous)) select.value = previous;
  updateActions();
}

function renderFavorites() {
  const panel = $("favorites-row");
  panel.replaceChildren();
  const paths = activeConnection ? favorites[activeConnection.id] ?? [] : [];
  if (!paths.length) { panel.classList.add("hidden"); return; }
  panel.classList.remove("hidden");
  const label = document.createElement("span"); label.className = "favorites-label"; label.textContent = "FAVORITES"; panel.append(label);
  for (const path of paths) {
    const button = document.createElement("button"); button.className = `favorite-chip${path === currentPath ? " active" : ""}`; button.textContent = `☆ ${path}`;
    button.title = `Open ${path}`; button.addEventListener("click", () => navigate(path)); panel.append(button);
  }
}

function renderFiles() {
  rows.replaceChildren(); selected.clear();
  if (!files.length) {
    const tr = document.createElement("tr"); tr.className = "empty-row";
    tr.innerHTML = '<td colspan="3"><div class="empty-state"><div class="empty-icon">⌁</div><div class="empty-title">This folder is empty</div><div class="empty-copy">Upload files here or navigate to another folder.</div></div></td>';
    rows.append(tr);
  }
  for (const file of files) {
    const tr = document.createElement("tr"); tr.className = "file-row"; tr.tabIndex = 0; tr.dataset.path = file.fullPath;
    const name = document.createElement("td"); name.className = "file-name";
    const icon = document.createElement("span"); icon.className = `file-icon${file.isDirectory ? " folder-icon" : ""}`; icon.textContent = file.isDirectory ? "▰" : "▤";
    const label = document.createElement("span"); label.className = "file-label"; label.textContent = file.name;
    name.append(icon, label);
    const size = document.createElement("td"); size.className = "file-meta"; size.textContent = file.size;
    const modified = document.createElement("td"); modified.className = "file-meta"; modified.textContent = file.lastModified;
    tr.append(name, size, modified);
    tr.addEventListener("click", (event) => {
      if (event.ctrlKey || event.metaKey || event.shiftKey) {
        if (selected.has(file.fullPath)) selected.delete(file.fullPath); else selected.add(file.fullPath);
      } else { selected = new Set([file.fullPath]); }
      renderSelection();
    });
    tr.addEventListener("dblclick", () => file.isDirectory ? navigate(file.fullPath) : editFiles([file]));
    tr.addEventListener("keydown", (event) => { if (event.key === "Enter") file.isDirectory ? navigate(file.fullPath) : editFiles([file]); });
    rows.append(tr);
  }
  updateActions();
}

function renderSelection() {
  rows.querySelectorAll<HTMLTableRowElement>(".file-row").forEach((row) => row.classList.toggle("selected", selected.has(row.dataset.path ?? "")));
  updateActions();
}

async function navigate(path: string) {
  try {
    status(`Loading ${path}…`);
    files = await invoke<RemoteFile[]>("change_directory", { path });
    currentPath = path === "/" ? "/" : `/${path.replace(/^\/+|\/+$/g, "")}/`;
    renderFiles(); status(`${currentPath} · ${files.length} item(s)`);
  } catch (error) { status(String(error), true, true); }
}

async function connect() {
  const id = select.value; if (!id) return;
  const connection = connections.find((item) => item.id === id); if (!connection) return;
  try {
    status(`Connecting to ${connection.alias}…`, false);
    let path: string;
    try {
      path = await invoke<string>("connect", { id });
    } catch (error) {
      const message = String(error);
      const prefix = "HOST_KEY_CONFIRM:";
      if (!message.includes(prefix)) throw error;
      const fingerprint = message.slice(message.indexOf(prefix) + prefix.length);
      if (!confirm(`Trust this SSH server key for ${connection.host}:${connection.port}?\n\nSHA256: ${fingerprint}\n\nOnly accept it if you recognize this server.`)) throw new Error("Server key was not trusted");
      await invoke("trust_host", { id, fingerprint });
      path = await invoke<string>("connect", { id });
    }
    currentPath = path; activeConnection = connection;
    await navigate(currentPath); status(`Connected to ${connection.alias} · ${currentPath}`);
  } catch (error) {
    await invoke("disconnect").catch(() => undefined);
    activeConnection = null; files = []; renderFiles(); status(`Connection failed: ${String(error)}`, false, true);
  }
}

function openConnectionForm(connection?: Connection) {
  $("dialog-title").textContent = connection ? "Edit connection" : "Add connection";
  $("connection-id").setAttribute("value", connection?.id ?? "");
  ($("alias-input") as HTMLInputElement).value = connection?.alias ?? "";
  ($("host-input") as HTMLInputElement).value = connection?.host ?? "";
  ($("port-input") as HTMLInputElement).value = String(connection?.port ?? 22);
  ($("username-input") as HTMLInputElement).value = connection?.username ?? "";
  ($("password-input") as HTMLInputElement).value = "";
  ($("password-input") as HTMLInputElement).placeholder = connection ? "Leave blank to keep the saved password" : "Stored in your system credential store";
  ($("key-input") as HTMLInputElement).value = connection?.privateKey ?? "";
  dialog.showModal();
}

$("connection-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const oldId = ($("connection-id") as HTMLInputElement).value || undefined;
  try {
    const connection = await invoke<Connection>("save_connection", {
      id: oldId,
      alias: ($("alias-input") as HTMLInputElement).value,
      host: ($("host-input") as HTMLInputElement).value,
      port: Number(($("port-input") as HTMLInputElement).value),
      username: ($("username-input") as HTMLInputElement).value,
      password: ($("password-input") as HTMLInputElement).value,
      privateKey: ($("key-input") as HTMLInputElement).value || null,
    });
    if (activeConnection?.id === connection.id) {
      await invoke("disconnect"); activeConnection = null; files = []; renderFiles();
    }
    dialog.close(); connections = (await invoke<InitialData>("get_initial_data")).connections;
    renderConnections(connection.id); status("Connection saved", Boolean(activeConnection));
  } catch (error) { status(`Could not save connection: ${String(error)}`, Boolean(activeConnection), true); }
});

$("connect-button").addEventListener("click", () => { void connect(); });
select.addEventListener("change", () => updateActions());
$("add-connection").addEventListener("click", () => openConnectionForm());
$("edit-connection").addEventListener("click", () => { const item = connections.find((connection) => connection.id === select.value); if (item) openConnectionForm(item); });
$("delete-connection").addEventListener("click", async () => {
  const item = connections.find((connection) => connection.id === select.value); if (!item || !confirm(`Delete connection “${item.alias}”?`)) return;
  await invoke("delete_connection", { id: item.id }); connections = connections.filter((connection) => connection.id !== item.id);
  if (activeConnection?.id === item.id) { await invoke("disconnect"); activeConnection = null; files = []; renderFiles(); }
  renderConnections(""); status("Connection deleted", Boolean(activeConnection));
});
$("close-dialog").addEventListener("click", () => dialog.close());
$("cancel-dialog").addEventListener("click", () => dialog.close());
$("up-button").addEventListener("click", () => { if (currentPath !== "/") { const parent = currentPath.replace(/\/$/, "").replace(/\/[^/]+$/, "") || "/"; void navigate(parent); } });
$("refresh-button").addEventListener("click", () => { void navigate(currentPath); });
$("favorite-button").addEventListener("click", async () => { if (!activeConnection) return; favorites[activeConnection.id] = await invoke<string[]>("toggle_favorite", { path: currentPath }); updateActions(); });

$("upload-button").addEventListener("click", async () => {
  const paths = await invoke<string[]>("pick_upload_files"); if (!paths.length) return;
  status(`Uploading ${paths.length} file(s)…`);
  try { status(await invoke<string>("upload_files", { paths })); await navigate(currentPath); } catch (error) { status(`Upload failed: ${String(error)}`, true, true); }
});

$("download-button").addEventListener("click", async () => {
  const paths = currentSelected().filter((file) => !file.isDirectory).map((file) => file.fullPath); if (!paths.length) return;
  const destination = await invoke<string | null>("pick_download_folder"); if (!destination) return;
  try { status(await invoke<string>("download_files", { paths, destination })); } catch (error) { status(`Download failed: ${String(error)}`, true, true); }
});

$("delete-button").addEventListener("click", async () => {
  const picked = currentSelected(); if (!picked.length) return;
  if (!confirm(`Delete ${picked.length} selected item(s)?\n\n${picked.map((file) => file.name).join("\n")}`)) return;
  try { status(await invoke<string>("delete_remote", { paths: picked.map((file) => file.fullPath) })); await navigate(currentPath); }
  catch (error) { status(`Delete failed: ${String(error)}`, true, true); }
});

async function editFiles(picked: RemoteFile[]) {
  const paths = picked.filter((file) => !file.isDirectory).map((file) => file.fullPath); if (!paths.length) return;
  try { status(await invoke<string>("edit_remote", { paths })); }
  catch (error) { status(`Edit failed: ${String(error)}`, true, true); }
}
$("edit-button").addEventListener("click", () => { void editFiles(currentSelected()); });

async function checkModifiedEdits() {
  if (!activeConnection || editPromptOpen || !document.hasFocus()) return;
  try {
    const paths = await invoke<string[]>("modified_edits"); if (!paths.length) return;
    editPromptOpen = true;
    const upload = confirm(`${paths.length} edited file(s) changed. Upload them to the server?`);
    if (upload) { status(await invoke<string>("upload_edited", { paths })); await navigate(currentPath); }
    else await invoke("discard_edits", { paths });
  } catch (error) { status(`Upload failed: ${String(error)}`, true, true); }
  finally { editPromptOpen = false; }
}
window.addEventListener("focus", () => { void checkModifiedEdits(); });
window.setInterval(() => { void checkModifiedEdits(); }, 3000);

async function initialize() {
  try {
    const data = await invoke<InitialData>("get_initial_data");
    connections = data.connections; favorites = data.favorites;
    renderConnections(data.lastConnectionId ?? "");
    if (data.lastConnectionId) void connect();
  } catch (error) { status(`Could not load saved connections: ${String(error)}`, false, true); }
}
void initialize();
