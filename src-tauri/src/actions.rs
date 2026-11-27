//! Side-effecting actions triggered from the UI: kill, stop, open.

use crate::ports::{cmd, find_bin, DOCKER_BINS};
use std::path::Path;

fn run_checked(bin: &str, args: &[&str]) -> Result<(), String> {
    let out = cmd(bin)
        .args(args)
        .output()
        .map_err(|e| format!("No se pudo ejecutar {bin}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if msg.is_empty() { format!("{bin} terminó con {}", out.status) } else { msg })
    }
}

/// Fire-and-forget launch (for tools like explorer.exe that return non-zero on success).
#[cfg_attr(not(windows), allow(dead_code))]
fn spawn(bin: &str, args: &[&str]) -> Result<(), String> {
    cmd(bin)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("No se pudo ejecutar {bin}: {e}"))
}

pub fn kill_process(pid: u32, force: bool) -> Result<(), String> {
    if pid <= 4 || pid == std::process::id() {
        return Err(format!("No se puede detener el PID {pid}"));
    }
    let pid = pid.to_string();
    #[cfg(windows)]
    {
        // Console apps (node, python…) ignore a graceful taskkill, so always force.
        let _ = force;
        run_checked("taskkill", &["/PID", &pid, "/T", "/F"])
    }
    #[cfg(not(windows))]
    {
        let signal = if force { "-KILL" } else { "-TERM" };
        run_checked("/bin/kill", &[signal, &pid])
    }
}

pub fn stop_container(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
    if !valid {
        return Err(format!("Nombre de contenedor inválido: {name}"));
    }
    run_checked(find_bin(DOCKER_BINS, "docker"), &["stop", name])
}

fn existing_dir(path: &str) -> Result<&str, String> {
    if Path::new(path).is_dir() {
        Ok(path)
    } else {
        Err(format!("La carpeta no existe: {path}"))
    }
}

pub fn open_url(port: u16) -> Result<(), String> {
    let url = format!("http://localhost:{port}");
    #[cfg(target_os = "macos")]
    return run_checked("/usr/bin/open", &[&url]);
    #[cfg(windows)]
    return spawn("rundll32", &["url.dll,FileProtocolHandler", &url]);
    #[cfg(all(unix, not(target_os = "macos")))]
    return run_checked("xdg-open", &[&url]);
}

pub fn reveal_folder(path: &str) -> Result<(), String> {
    let dir = existing_dir(path)?;
    #[cfg(target_os = "macos")]
    return run_checked("/usr/bin/open", &[dir]);
    #[cfg(windows)]
    return spawn("explorer", &[dir]);
    #[cfg(all(unix, not(target_os = "macos")))]
    return run_checked("xdg-open", &[dir]);
}

pub fn open_in_editor(path: &str) -> Result<(), String> {
    let dir = existing_dir(path)?;
    #[cfg(target_os = "macos")]
    return run_checked("/usr/bin/open", &["-a", "Visual Studio Code", dir])
        .or_else(|_| run_checked("/usr/bin/open", &["-a", "Cursor", dir]))
        .map_err(|_| "No encontré VS Code ni Cursor instalados".to_string());
    // `code` is a .cmd shim on Windows, so it must go through cmd.exe.
    #[cfg(windows)]
    return run_checked("cmd", &["/C", "code", dir])
        .or_else(|_| run_checked("cmd", &["/C", "cursor", dir]))
        .map_err(|_| "No encontré VS Code ni Cursor en el PATH".to_string());
    #[cfg(all(unix, not(target_os = "macos")))]
    return run_checked("code", &[dir]);
}
