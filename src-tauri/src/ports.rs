//! Discovers which processes are listening on TCP ports and figures out a
//! human-friendly name for each one (project name, framework, Docker container).
//!
//! Sources (macOS / Linux):
//!   - `lsof -iTCP -sTCP:LISTEN`  → pid, process name, ports
//!   - `lsof -d cwd`              → working directory of each pid
//!   - `ps -o command=`           → full command line
//! Sources (Windows):
//!   - `netstat -ano`             → pid, ports
//!   - `sysinfo` crate            → process name, command line, working directory
//! All platforms:
//!   - `docker ps`                → published container ports

use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Dev,
    Docker,
    System,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PortEntry {
    /// Stable id for the UI: `pid:<n>` or `docker:<name>`.
    pub id: String,
    pub pid: u32,
    pub process: String,
    pub command: String,
    pub ports: Vec<u16>,
    /// true if listening on a non-loopback address (reachable from the network).
    pub exposed: bool,
    pub cwd: Option<String>,
    pub project_root: Option<String>,
    pub app_name: String,
    pub framework: Option<String>,
    pub kind: Kind,
}

#[derive(Debug, PartialEq)]
struct Listener {
    pid: u32,
    process: String,
    ports: BTreeSet<u16>,
    exposed: bool,
}

#[derive(Debug, PartialEq)]
struct Container {
    name: String,
    image: String,
    ports: BTreeSet<u16>,
}

#[derive(Debug, Default)]
struct ProcInfo {
    name: Option<String>,
    command: String,
    cwd: Option<String>,
}

// GUI apps launched from Finder get a minimal PATH, so prefer absolute paths.
#[cfg(not(windows))]
const LSOF_BINS: &[&str] = &["/usr/sbin/lsof", "/usr/bin/lsof"];
#[cfg(not(windows))]
const PS_BINS: &[&str] = &["/bin/ps", "/usr/bin/ps"];
pub(crate) const DOCKER_BINS: &[&str] = &[
    "/usr/local/bin/docker",
    "/opt/homebrew/bin/docker",
    "/Applications/Docker.app/Contents/Resources/bin/docker",
    "/usr/bin/docker",
    r"C:\Program Files\Docker\Docker\resources\bin\docker.exe",
];

/// `Command::new` that never flashes a console window on Windows.
pub(crate) fn cmd(bin: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(bin);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub(crate) fn find_bin(candidates: &[&'static str], fallback: &'static str) -> &'static str {
    candidates
        .iter()
        .copied()
        .find(|b| Path::new(b).exists())
        .unwrap_or(fallback)
}

fn run(bin: &str, args: &[&str]) -> Option<String> {
    // lsof exits with 1 when nothing matches, so ignore the status and use stdout.
    let out = cmd(bin).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg_attr(windows, allow(dead_code))]
fn join_pids(pids: &[u32]) -> String {
    pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

/// Splits an `lsof -F` line into its one-char tag and value.
fn field(line: &str) -> (Option<char>, &str) {
    let mut chars = line.chars();
    let tag = chars.next();
    (tag, chars.as_str())
}

// ---------------------------------------------------------------------------
// Scanning: macOS / Linux
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
fn scan_listeners() -> Result<Vec<Listener>, String> {
    let bin = find_bin(LSOF_BINS, "lsof");
    let out = cmd(bin)
        .args(["-nP", "+c", "0", "-iTCP", "-sTCP:LISTEN", "-F", "pcn"])
        .output()
        .map_err(|e| format!("No se pudo ejecutar lsof: {e}"))?;
    Ok(parse_lsof_listen(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(not(windows))]
fn scan_processes(pids: &[u32]) -> HashMap<u32, ProcInfo> {
    let mut cwds = scan_cwds(pids);
    let mut commands = scan_commands(pids);
    pids.iter()
        .map(|pid| {
            let info = ProcInfo {
                name: None, // lsof already gave us the process name
                command: commands.remove(pid).unwrap_or_default(),
                cwd: cwds.remove(pid),
            };
            (*pid, info)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Scanning: Windows
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn scan_listeners() -> Result<Vec<Listener>, String> {
    let out = cmd("netstat")
        .args(["-ano"])
        .output()
        .map_err(|e| format!("No se pudo ejecutar netstat: {e}"))?;
    Ok(parse_netstat(&String::from_utf8_lossy(&out.stdout)))
}

/// Parses `netstat -ano`. The "LISTENING" column is localized ("ESCUCHANDO" on
/// Spanish Windows), so a listener is detected by its foreign address `*:0` instead.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_netstat(raw: &str) -> Vec<Listener> {
    let mut by_pid: Vec<Listener> = Vec::new();
    for line in raw.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 5 || !cols[0].eq_ignore_ascii_case("TCP") {
            continue;
        }
        let (local, foreign, pid) = (cols[1], cols[2], cols[cols.len() - 1]);
        if !(foreign == "0.0.0.0:0" || foreign == "[::]:0") {
            continue;
        }
        let (Some((host, port)), Ok(pid)) = (local.rsplit_once(':'), pid.parse::<u32>()) else {
            continue;
        };
        let Ok(port) = port.parse::<u16>() else { continue };
        if pid == 0 {
            continue;
        }
        let idx = match by_pid.iter().position(|l| l.pid == pid) {
            Some(i) => i,
            None => {
                by_pid.push(Listener { pid, process: String::new(), ports: BTreeSet::new(), exposed: false });
                by_pid.len() - 1
            }
        };
        let l = &mut by_pid[idx];
        l.ports.insert(port);
        if !is_loopback(host) {
            l.exposed = true;
        }
    }
    by_pid
}

#[cfg(windows)]
fn scan_processes(pids: &[u32]) -> HashMap<u32, ProcInfo> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    let wanted: Vec<Pid> = pids.iter().map(|p| Pid::from_u32(*p)).collect();
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&wanted),
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always),
    );
    pids.iter()
        .filter_map(|pid| {
            let p = sys.process(Pid::from_u32(*pid))?;
            let command = p
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            let info = ProcInfo {
                name: Some(p.name().to_string_lossy().into_owned()),
                command,
                cwd: p.cwd().map(|c| c.display().to_string()).filter(|c| !c.is_empty()),
            };
            Some((*pid, info))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Parsers (pure, tested on every platform)
// ---------------------------------------------------------------------------

#[cfg_attr(windows, allow(dead_code))]
fn parse_lsof_listen(raw: &str) -> Vec<Listener> {
    let mut list: Vec<Listener> = Vec::new();
    for line in raw.lines() {
        match field(line) {
            (Some('p'), v) => {
                if let Ok(pid) = v.parse() {
                    list.push(Listener {
                        pid,
                        process: String::new(),
                        ports: BTreeSet::new(),
                        exposed: false,
                    });
                }
            }
            (Some('c'), v) => {
                if let Some(l) = list.last_mut() {
                    l.process = v.to_string();
                }
            }
            (Some('n'), v) => {
                let Some(l) = list.last_mut() else { continue };
                let Some((host, port)) = v.rsplit_once(':') else { continue };
                if let Ok(p) = port.parse::<u16>() {
                    l.ports.insert(p);
                    if !is_loopback(host) {
                        l.exposed = true;
                    }
                }
            }
            _ => {}
        }
    }
    list.retain(|l| !l.ports.is_empty());
    list
}

fn is_loopback(host: &str) -> bool {
    host == "[::1]" || host == "localhost" || host.starts_with("127.")
}

#[cfg(not(windows))]
fn scan_cwds(pids: &[u32]) -> HashMap<u32, String> {
    if pids.is_empty() {
        return HashMap::new();
    }
    let bin = find_bin(LSOF_BINS, "lsof");
    run(bin, &["-a", "-d", "cwd", "-p", &join_pids(pids), "-F", "pn"])
        .map(|raw| parse_lsof_cwd(&raw))
        .unwrap_or_default()
}

#[cfg_attr(windows, allow(dead_code))]
fn parse_lsof_cwd(raw: &str) -> HashMap<u32, String> {
    let mut map = HashMap::new();
    let mut current: Option<u32> = None;
    for line in raw.lines() {
        match field(line) {
            (Some('p'), v) => current = v.parse().ok(),
            (Some('n'), v) => {
                if let Some(pid) = current {
                    map.insert(pid, v.to_string());
                }
            }
            _ => {}
        }
    }
    map
}

#[cfg(not(windows))]
fn scan_commands(pids: &[u32]) -> HashMap<u32, String> {
    if pids.is_empty() {
        return HashMap::new();
    }
    let bin = find_bin(PS_BINS, "ps");
    run(bin, &["-ww", "-o", "pid=,command=", "-p", &join_pids(pids)])
        .map(|raw| parse_ps(&raw))
        .unwrap_or_default()
}

#[cfg_attr(windows, allow(dead_code))]
fn parse_ps(raw: &str) -> HashMap<u32, String> {
    raw.lines()
        .filter_map(|line| {
            let (pid, cmd) = line.trim_start().split_once(char::is_whitespace)?;
            Some((pid.parse().ok()?, cmd.trim().to_string()))
        })
        .collect()
}

fn scan_docker() -> Vec<Container> {
    let Some(bin) = DOCKER_BINS.iter().find(|b| Path::new(b).exists()) else {
        return Vec::new();
    };
    run(bin, &["ps", "--format", "{{.Names}}\t{{.Image}}\t{{.Ports}}"])
        .map(|raw| parse_docker_ps(&raw))
        .unwrap_or_default()
}

/// Parses lines like `db\tpostgres:16\t0.0.0.0:5432->5432/tcp, :::5432->5432/tcp`.
fn parse_docker_ps(raw: &str) -> Vec<Container> {
    raw.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let name = parts.next()?.trim().to_string();
            let image = parts.next()?.trim().to_string();
            let mut ports = BTreeSet::new();
            for chunk in parts.next().unwrap_or("").split(',') {
                let Some((host, _)) = chunk.trim().split_once("->") else { continue };
                let Some((_, spec)) = host.rsplit_once(':') else { continue };
                match spec.split_once('-') {
                    // Port ranges: 0.0.0.0:8000-8002->8000-8002/tcp
                    Some((a, b)) => {
                        if let (Ok(a), Ok(b)) = (a.parse::<u16>(), b.parse::<u16>()) {
                            ports.extend(a..=b);
                        }
                    }
                    None => {
                        if let Ok(p) = spec.parse() {
                            ports.insert(p);
                        }
                    }
                }
            }
            (!name.is_empty() && !ports.is_empty()).then_some(Container { name, image, ports })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Naming & framework detection
// ---------------------------------------------------------------------------

const PROJECT_MARKERS: &[&str] = &[
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "requirements.txt",
    "manage.py",
    "composer.json",
    "go.mod",
    "Gemfile",
    "pom.xml",
    "build.gradle",
    "deno.json",
    ".git",
];

/// Walks up from `start` looking for a project marker, stopping at $HOME or `/`.
fn find_project_root(start: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d == Path::new("/") || Some(d) == home {
            break;
        }
        if PROJECT_MARKERS.iter().any(|m| d.join(m).exists()) {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn json_name(path: &Path) -> Option<String> {
    read_json(path)?
        .get("name")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Minimal `name = "..."` lookup for Cargo.toml / pyproject.toml.
fn toml_name(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    text.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("name")?.trim_start().strip_prefix('=')?;
        let name = rest.trim().trim_matches(|c| c == '"' || c == '\'');
        (!name.is_empty()).then(|| name.to_string())
    })
}

fn project_name(root: &Path) -> String {
    json_name(&root.join("package.json"))
        .or_else(|| json_name(&root.join("composer.json")))
        .or_else(|| toml_name(&root.join("Cargo.toml")))
        .or_else(|| toml_name(&root.join("pyproject.toml")))
        .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| root.display().to_string())
}

fn package_deps(root: &Path) -> BTreeSet<String> {
    let Some(pkg) = read_json(&root.join("package.json")) else {
        return BTreeSet::new();
    };
    ["dependencies", "devDependencies"]
        .iter()
        .filter_map(|k| pkg.get(k)?.as_object())
        .flat_map(|o| o.keys().cloned())
        .collect()
}

/// Order matters: more specific tools first (e.g. Astro/Nuxt run on top of Vite).
const COMMAND_HINTS: &[(&str, &str)] = &[
    ("next-server", "Next.js"),
    ("next dev", "Next.js"),
    ("next start", "Next.js"),
    ("/next/dist", "Next.js"),
    ("nuxt", "Nuxt"),
    ("astro", "Astro"),
    ("remix", "Remix"),
    ("storybook", "Storybook"),
    ("@angular", "Angular"),
    ("ng serve", "Angular"),
    ("react-scripts", "Create React App"),
    ("expo start", "Expo"),
    ("vite", "Vite"),
    ("webpack", "Webpack"),
    ("nest start", "NestJS"),
    ("uvicorn", "Uvicorn"),
    ("gunicorn", "Gunicorn"),
    ("manage.py runserver", "Django"),
    ("flask", "Flask"),
    ("streamlit", "Streamlit"),
    ("jupyter", "Jupyter"),
    ("rails", "Rails"),
    ("puma", "Puma"),
    ("artisan serve", "Laravel"),
    ("php -s", "PHP"),
    ("hugo", "Hugo"),
    ("jekyll", "Jekyll"),
    ("postgres", "PostgreSQL"),
    ("mysqld", "MySQL"),
    ("mariadbd", "MariaDB"),
    ("redis-server", "Redis"),
    ("mongod", "MongoDB"),
    ("nginx", "nginx"),
    ("httpd", "Apache"),
];

const DEP_HINTS: &[(&str, &str)] = &[
    ("next", "Next.js"),
    ("nuxt", "Nuxt"),
    ("astro", "Astro"),
    ("@remix-run/dev", "Remix"),
    ("@angular/core", "Angular"),
    ("@nestjs/core", "NestJS"),
    ("vite", "Vite"),
    ("express", "Express"),
    ("fastify", "Fastify"),
    ("hono", "Hono"),
    ("koa", "Koa"),
];

const RUNTIME_HINTS: &[(&str, &str)] = &[
    ("node", "Node.js"),
    ("python", "Python"),
    ("ruby", "Ruby"),
    ("java", "Java"),
    ("php", "PHP"),
    ("bun", "Bun"),
    ("deno", "Deno"),
    ("dotnet", ".NET"),
];

/// Friendly names for common macOS daemons that confuse people (e.g. AirPlay on :5000).
const KNOWN_PROCESSES: &[(&str, &str)] = &[
    ("ControlCenter", "AirPlay Receiver"),
    ("rapportd", "Continuity (rapportd)"),
    ("sharingd", "AirDrop / Sharing"),
    ("com.docker.backend", "Docker Desktop"),
    ("OrbStack Helper", "OrbStack"),
];

fn hint_from_command(command: &str) -> Option<&'static str> {
    let cmd = command.to_lowercase();
    COMMAND_HINTS
        .iter()
        .find(|(needle, _)| cmd.contains(needle))
        .map(|(_, name)| *name)
}

fn hint_from_deps(deps: &BTreeSet<String>) -> Option<&'static str> {
    DEP_HINTS
        .iter()
        .find(|(dep, _)| deps.contains(*dep))
        .map(|(_, name)| *name)
}

fn hint_from_runtime(process: &str) -> Option<&'static str> {
    let p = process.to_lowercase();
    RUNTIME_HINTS
        .iter()
        .find(|(prefix, _)| p.starts_with(prefix))
        .map(|(_, name)| *name)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn list_ports() -> Result<Vec<PortEntry>, String> {
    let mut listeners = scan_listeners()?;
    let pids: Vec<u32> = listeners.iter().map(|l| l.pid).collect();
    let mut procs = scan_processes(&pids);
    for l in listeners.iter_mut().filter(|l| l.process.is_empty()) {
        if let Some(name) = procs.get_mut(&l.pid).and_then(|p| p.name.take()) {
            l.process = name;
        }
    }
    let containers = scan_docker();
    let home = home_dir();

    let mut entries = Vec::new();

    // Docker containers claim their published ports from whichever process
    // actually holds the socket (Docker Desktop backend, OrbStack, ...).
    for c in &containers {
        let holder = listeners
            .iter_mut()
            .find(|l| c.ports.iter().any(|p| l.ports.contains(p)));
        let (pid, process, exposed) = match holder {
            Some(l) => {
                l.ports.retain(|p| !c.ports.contains(p));
                (l.pid, l.process.clone(), l.exposed)
            }
            None => (0, "docker".to_string(), true),
        };
        entries.push(PortEntry {
            id: format!("docker:{}", c.name),
            pid,
            process,
            command: format!("docker container {}", c.name),
            ports: c.ports.iter().copied().collect(),
            exposed,
            cwd: None,
            project_root: None,
            app_name: c.name.clone(),
            framework: Some(c.image.clone()),
            kind: Kind::Docker,
        });
    }

    for l in listeners.into_iter().filter(|l| !l.ports.is_empty()) {
        let info = procs.remove(&l.pid).unwrap_or_default();
        let cwd = info.cwd;
        let command = info.command;
        let root = cwd
            .as_deref()
            .and_then(|c| find_project_root(Path::new(c), home.as_deref()));

        let cmd_hint = hint_from_command(&command);
        let framework = cmd_hint
            .or_else(|| root.as_deref().and_then(|r| hint_from_deps(&package_deps(r))))
            .or_else(|| hint_from_runtime(&l.process))
            .map(String::from);

        let in_home = match (&cwd, &home) {
            (Some(c), Some(h)) => Path::new(c).starts_with(h) && Path::new(c) != h.as_path(),
            _ => false,
        };
        let kind = if root.is_some() || cmd_hint.is_some() || in_home {
            Kind::Dev
        } else {
            Kind::System
        };

        let app_name = match &root {
            Some(r) => project_name(r),
            None => {
                let bare = l.process.strip_suffix(".exe").unwrap_or(&l.process);
                KNOWN_PROCESSES
                    .iter()
                    .find(|(p, _)| *p == bare)
                    .map(|(_, n)| n.to_string())
                    .unwrap_or_else(|| bare.to_string())
            }
        };

        entries.push(PortEntry {
            id: format!("pid:{}", l.pid),
            pid: l.pid,
            process: l.process,
            command,
            ports: l.ports.into_iter().collect(),
            exposed: l.exposed,
            cwd,
            project_root: root.map(|r| r.display().to_string()),
            app_name,
            framework,
            kind,
        });
    }

    entries.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.ports.first().cmp(&b.ports.first())));
    Ok(entries)
}

/// First port in `[start, end]` that no process is listening on and that we can bind.
pub fn find_free_port(start: u16, end: u16) -> Result<u16, String> {
    if start == 0 || start > end {
        return Err("Rango de puertos inválido".into());
    }
    let used: BTreeSet<u16> = scan_listeners()?.into_iter().flat_map(|l| l.ports).collect();
    (start..=end)
        .find(|p| !used.contains(p) && TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .ok_or_else(|| format!("No hay puertos libres entre {start} y {end}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lsof_listen_output() {
        let raw = "p805\ncControlCenter\nf9\nn*:7000\nf10\nn*:7000\nf11\nn*:5000\n\
                   p1033\ncGoogle Chrome\nf31\nn[::1]:7679\n\
                   p42\ncnode\nf20\nn127.0.0.1:3000\n";
        let list = parse_lsof_listen(raw);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].process, "ControlCenter");
        assert_eq!(list[0].ports, BTreeSet::from([5000, 7000]));
        assert!(list[0].exposed);
        assert_eq!(list[1].process, "Google Chrome");
        assert!(!list[1].exposed);
        assert_eq!(list[2].ports, BTreeSet::from([3000]));
        assert!(!list[2].exposed);
    }

    #[test]
    fn parses_netstat_output_any_locale() {
        let raw = "\r\nConexiones activas\r\n\r\n  Proto  Dirección local   Dirección remota   Estado       PID\r\n\
                   \x20 TCP    0.0.0.0:135            0.0.0.0:0              ESCUCHANDO       1100\r\n\
                   \x20 TCP    127.0.0.1:3000         0.0.0.0:0              ESCUCHANDO       4242\r\n\
                   \x20 TCP    [::]:3000              [::]:0                 ESCUCHANDO       4242\r\n\
                   \x20 TCP    127.0.0.1:3000         127.0.0.1:50123        ESTABLECIDA      4242\r\n\
                   \x20 UDP    0.0.0.0:5353           *:*                                     900\r\n";
        let list = parse_netstat(raw);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].pid, 1100);
        assert!(list[0].exposed);
        assert_eq!(list[1].pid, 4242);
        assert_eq!(list[1].ports, BTreeSet::from([3000]));
        assert!(list[1].exposed); // also bound on [::]
    }

    #[test]
    fn parses_cwd_and_ps() {
        let cwd = parse_lsof_cwd("p42\nfcwd\nn/Users/me/proyectos/api\np7\nfcwd\nn/\n");
        assert_eq!(cwd[&42], "/Users/me/proyectos/api");
        assert_eq!(cwd[&7], "/");

        let ps = parse_ps("   42 node /Users/me/api/node_modules/.bin/vite --port 5173\n  7 /usr/sbin/foo\n");
        assert_eq!(ps[&42], "node /Users/me/api/node_modules/.bin/vite --port 5173");
        assert_eq!(ps[&7], "/usr/sbin/foo");
    }

    #[test]
    fn parses_docker_ps() {
        let raw = "db\tpostgres:16\t0.0.0.0:5432->5432/tcp, :::5432->5432/tcp\n\
                   worker\tmyapp:latest\t\n\
                   web\tnginx\t0.0.0.0:8000-8002->8000-8002/tcp\n";
        let list = parse_docker_ps(raw);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "db");
        assert_eq!(list[0].ports, BTreeSet::from([5432]));
        assert_eq!(list[1].ports, BTreeSet::from([8000, 8001, 8002]));
    }

    #[test]
    fn detects_frameworks() {
        assert_eq!(hint_from_command("node /x/node_modules/.bin/next dev"), Some("Next.js"));
        assert_eq!(hint_from_command("node /x/node_modules/.bin/astro dev"), Some("Astro"));
        assert_eq!(hint_from_command("node /x/node_modules/.bin/vite"), Some("Vite"));
        assert_eq!(hint_from_command("python manage.py runserver 8000"), Some("Django"));
        assert_eq!(hint_from_command("/usr/bin/some-daemon"), None);
        assert_eq!(hint_from_runtime("node"), Some("Node.js"));
        assert_eq!(hint_from_runtime("Python3.12"), Some("Python"));
    }
}
