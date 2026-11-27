mod actions;
mod ports;

use ports::PortEntry;

/// Runs blocking work (spawning lsof/ps/docker) off the main thread so the UI stays responsive.
async fn blocking<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn list_ports() -> Result<Vec<PortEntry>, String> {
    blocking(ports::list_ports).await
}

#[tauri::command]
async fn find_free_port(start: u16, end: u16) -> Result<u16, String> {
    blocking(move || ports::find_free_port(start, end)).await
}

#[tauri::command]
async fn kill_process(pid: u32, force: bool) -> Result<(), String> {
    blocking(move || actions::kill_process(pid, force)).await
}

#[tauri::command]
async fn stop_container(name: String) -> Result<(), String> {
    blocking(move || actions::stop_container(&name)).await
}

#[tauri::command]
fn open_url(port: u16) -> Result<(), String> {
    actions::open_url(port)
}

#[tauri::command]
fn reveal_folder(path: String) -> Result<(), String> {
    actions::reveal_folder(&path)
}

#[tauri::command]
fn open_in_editor(path: String) -> Result<(), String> {
    actions::open_in_editor(&path)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            list_ports,
            find_free_port,
            kill_process,
            stop_container,
            open_url,
            reveal_folder,
            open_in_editor
        ])
        .run(tauri::generate_context!())
        .expect("error while running PortScope");
}
