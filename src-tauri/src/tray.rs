//! Menu bar (macOS) / system tray (Windows) icon.
//!
//! A single background loop scans ports, keeps the latest snapshot in state,
//! rebuilds the tray menu when something changes and notifies the window
//! through the `ports-updated` event.

use crate::actions;
use crate::ports::{self, Kind, PortEntry};
use serde::Serialize;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tauri::{
    image::Image,
    menu::{Menu, MenuBuilder, MenuItemBuilder, Submenu, SubmenuBuilder},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, Wry,
};

const TRAY_ID: &str = "portscope-tray";
const POLL_EVERY: Duration = Duration::from_secs(3);

pub const EVENT_PORTS: &str = "ports-updated";
pub const EVENT_LOG: &str = "portscope-log";

#[cfg(target_os = "macos")]
const REVEAL_LABEL: &str = "Mostrar en Finder";
#[cfg(not(target_os = "macos"))]
const REVEAL_LABEL: &str = "Mostrar carpeta";

#[derive(Default)]
pub struct Snapshot {
    entries: Mutex<Vec<PortEntry>>,
    json: Mutex<String>,
}

#[derive(Serialize, Clone)]
struct LogLine {
    tag: String,
    message: String,
    cls: &'static str,
}

fn log(app: &AppHandle, tag: &str, message: impl Into<String>, cls: &'static str) {
    let _ = app.emit(EVENT_LOG, LogLine { tag: tag.into(), message: message.into(), cls });
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    app.manage(Snapshot::default());

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(include_bytes!("../icons/tray-template.png"))?)
        .icon_as_template(true)
        .tooltip("PortScope")
        .menu(&build_menu(app, &[])?)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .build(app)?;

    let handle = app.clone();
    thread::spawn(move || loop {
        refresh(&handle);
        thread::sleep(POLL_EVERY);
    });
    Ok(())
}

pub fn refresh(app: &AppHandle) {
    match ports::list_ports() {
        Ok(entries) => publish(app, entries),
        Err(err) => log(app, "error", err, "err"),
    }
}

/// Stores a new snapshot; if it changed, updates the tray and notifies the window.
pub fn publish(app: &AppHandle, entries: Vec<PortEntry>) {
    let state = app.state::<Snapshot>();
    let json = serde_json::to_string(&entries).unwrap_or_default();
    {
        let mut last = state.json.lock().unwrap();
        if *last == json {
            return;
        }
        *last = json;
    }
    *state.entries.lock().unwrap() = entries.clone();
    let _ = app.emit(EVENT_PORTS, &entries);

    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(tray) = handle.tray_by_id(TRAY_ID) else { return };
        if let Ok(menu) = build_menu(&handle, &entries) {
            let _ = tray.set_menu(Some(menu));
        }
        set_count_title(&handle, &entries);
    });
}

fn app_count(entries: &[PortEntry]) -> usize {
    entries.iter().filter(|e| e.kind != Kind::System).count()
}

/// Shows the number of running apps next to the icon (macOS only; no-op elsewhere).
fn set_count_title(app: &AppHandle, entries: &[PortEntry]) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let n = app_count(entries);
    let _ = tray.set_title(if n > 0 { Some(n.to_string()) } else { None });
    let _ = tray.set_tooltip(Some(format!("PortScope · {n} apps escuchando")));
}

fn folder(e: &PortEntry) -> Option<String> {
    e.project_root
        .clone()
        .or_else(|| (e.kind != Kind::System).then(|| e.cwd.clone()).flatten())
}

// ---------------------------------------------------------------------------
// Menu
// ---------------------------------------------------------------------------

fn build_menu(app: &AppHandle, entries: &[PortEntry]) -> tauri::Result<Menu<Wry>> {
    let apps: Vec<&PortEntry> = entries.iter().filter(|e| e.kind != Kind::System).collect();
    let header = match apps.len() {
        0 => "Sin apps escuchando".to_string(),
        1 => "1 app escuchando".to_string(),
        n => format!("{n} apps escuchando"),
    };

    let mut menu = MenuBuilder::new(app).item(&MenuItemBuilder::new(header).enabled(false).build(app)?);
    for e in apps {
        menu = menu.item(&app_submenu(app, e)?);
    }
    menu.separator()
        .item(&MenuItemBuilder::with_id("show", "Abrir PortScope").build(app)?)
        .item(&MenuItemBuilder::with_id("quit", "Salir de PortScope").build(app)?)
        .build()
}

fn app_submenu(app: &AppHandle, e: &PortEntry) -> tauri::Result<Submenu<Wry>> {
    let ports = e.ports.iter().map(|p| format!(":{p}")).collect::<Vec<_>>().join("  ");
    let mut sub = SubmenuBuilder::new(app, format!("{}    {}", e.app_name, ports));

    if let Some(fw) = &e.framework {
        sub = sub.item(&MenuItemBuilder::new(fw).enabled(false).build(app)?).separator();
    }
    for p in &e.ports {
        sub = sub.item(
            &MenuItemBuilder::with_id(format!("open:{p}"), format!("Abrir localhost:{p}")).build(app)?,
        );
    }
    let has_folder = folder(e).is_some();
    sub.separator()
        .item(
            &MenuItemBuilder::with_id(format!("reveal:{}", e.id), REVEAL_LABEL)
                .enabled(has_folder)
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id(format!("editor:{}", e.id), "Abrir en editor")
                .enabled(has_folder)
                .build(app)?,
        )
        .separator()
        .item(&MenuItemBuilder::with_id(format!("stop:{}", e.id), "Detener").build(app)?)
        .build()
}

fn on_menu(app: &AppHandle, id: &str) {
    let (action, arg) = id.split_once(':').unwrap_or((id, ""));
    match action {
        "open" => {
            if let Ok(port) = arg.parse() {
                if let Err(err) = actions::open_url(port) {
                    log(app, "error", err, "err");
                }
            }
        }
        "reveal" | "editor" | "stop" => {
            let entry = app
                .state::<Snapshot>()
                .entries
                .lock()
                .unwrap()
                .iter()
                .find(|e| e.id == arg)
                .cloned();
            let Some(e) = entry else { return };
            let app = app.clone();
            let action = action.to_string();
            // Run off the main thread: these spawn processes and may block briefly.
            thread::spawn(move || {
                let result = match action.as_str() {
                    "reveal" => folder(&e).map_or(Ok(()), |f| actions::reveal_folder(&f)),
                    "editor" => folder(&e).map_or(Ok(()), |f| actions::open_in_editor(&f)),
                    _ => {
                        log(&app, &e.app_name, "deteniendo…", "");
                        let r = if e.kind == Kind::Docker {
                            actions::stop_container(&e.app_name)
                        } else {
                            actions::kill_process(e.pid, false)
                        };
                        thread::sleep(Duration::from_millis(700));
                        refresh(&app);
                        r
                    }
                };
                if let Err(err) = result {
                    log(&app, &e.app_name, err, "err");
                }
            });
        }
        "show" => show_main(app),
        "quit" => app.exit(0),
        _ => {}
    }
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}
