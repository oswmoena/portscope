import { invoke } from "@tauri-apps/api/core";

type Kind = "dev" | "docker" | "system";

interface PortEntry {
  id: string;
  pid: number;
  process: string;
  command: string;
  ports: number[];
  exposed: boolean;
  cwd: string | null;
  projectRoot: string | null;
  appName: string;
  framework: string | null;
  kind: Kind;
}

const REFRESH_MS = 3000;

const $ = <T extends HTMLElement>(sel: string) => document.querySelector<T>(sel)!;
const rowsEl = $<HTMLTableSectionElement>("#rows");
const emptyEl = $<HTMLDivElement>("#empty");
const summaryEl = $<HTMLParagraphElement>("#summary");
const searchEl = $<HTMLInputElement>("#search");
const showSystemEl = $<HTMLInputElement>("#show-system");
const refreshBtn = $<HTMLButtonElement>("#refresh");
const logEl = $<HTMLOListElement>("#log");
const freeForm = $<HTMLFormElement>("#free-port");
const freeStart = $<HTMLInputElement>("#free-start");
const freeResult = $<HTMLOutputElement>("#free-result");

let entries: PortEntry[] = [];
let lastSnapshot = "";
let previous: Map<string, PortEntry> | null = null;
let inFlight = false;
const freshIds = new Set<string>();
/** Rows whose "Detener" button is waiting for a second click. */
const confirming = new Map<string, number>();

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const esc = (s: string) =>
  s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);

const tildify = (p: string) => p.replace(/^\/Users\/[^/]+/, "~").replace(/^[A-Za-z]:\\Users\\[^\\]+/i, "~");

const fmtPorts = (ports: number[]) => ports.map((p) => `:${p}`).join(", ");

function log(tag: string, message: string, cls = "") {
  const li = document.createElement("li");
  const time = new Date().toLocaleTimeString();
  li.innerHTML = `<time>${time}</time><span class="tag">[${esc(tag)}]</span><span class="${cls}">${esc(message)}</span>`;
  logEl.append(li);
  while (logEl.children.length > 300) logEl.firstElementChild?.remove();
  logEl.scrollTop = logEl.scrollHeight;
}

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

async function refresh(manual = false) {
  if (inFlight) return;
  inFlight = true;
  if (manual) refreshBtn.classList.add("spin");
  try {
    const next = await invoke<PortEntry[]>("list_ports");
    diffAndLog(next);
    entries = next;
    const snapshot = JSON.stringify(next);
    if (snapshot !== lastSnapshot || manual) {
      lastSnapshot = snapshot;
      render();
    }
  } catch (err) {
    log("error", String(err), "err");
  } finally {
    inFlight = false;
    refreshBtn.classList.remove("spin");
  }
}

function diffAndLog(next: PortEntry[]) {
  const current = new Map(next.map((e) => [e.id, e]));
  if (previous === null) {
    const dev = next.filter((e) => e.kind !== "system").length;
    log("portscope", `${next.length} procesos escuchando (${dev} apps de desarrollo)`);
  } else {
    for (const [id, e] of current) {
      const before = previous.get(id);
      if (!before) {
        freshIds.add(id);
        log(e.appName, `escuchando en ${fmtPorts(e.ports)}`, "up");
        continue;
      }
      const added = e.ports.filter((p) => !before.ports.includes(p));
      const removed = before.ports.filter((p) => !e.ports.includes(p));
      if (added.length) log(e.appName, `abrió ${fmtPorts(added)}`, "up");
      if (removed.length) log(e.appName, `liberó ${fmtPorts(removed)}`, "down");
    }
    for (const [id, e] of previous) {
      if (!current.has(id)) log(e.appName, `se detuvo, liberó ${fmtPorts(e.ports)}`, "down");
    }
  }
  previous = current;
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

function visibleEntries(): PortEntry[] {
  const q = searchEl.value.trim().toLowerCase();
  return entries.filter((e) => {
    if (!showSystemEl.checked && e.kind === "system") return false;
    if (!q) return true;
    const hay = [e.appName, e.framework ?? "", e.process, e.projectRoot ?? e.cwd ?? "", ...e.ports.map(String)]
      .join(" ")
      .toLowerCase();
    return hay.includes(q);
  });
}

function rowHtml(e: PortEntry): string {
  const folder = e.projectRoot ?? (e.kind !== "system" ? e.cwd : null);
  const subtitle =
    e.kind === "docker"
      ? `contenedor Docker`
      : folder
        ? tildify(folder)
        : e.command || e.process;
  const isConfirming = confirming.has(e.id);
  const canStop = e.pid > 0 || e.kind === "docker";

  return `
    <tr data-id="${esc(e.id)}" class="${freshIds.has(e.id) ? "fresh" : ""}">
      <td><span class="dot ${e.kind}" title="${e.kind}"></span></td>
      <td>
        <div class="app-name">
          ${esc(e.appName)}
          ${e.framework ? `<span class="badge">${esc(e.framework)}</span>` : ""}
          ${e.exposed ? `<span class="badge lan" title="Accesible desde la red local">LAN</span>` : ""}
        </div>
        <div class="app-path" title="${esc(e.command)}">${esc(subtitle)}</div>
      </td>
      <td class="pid">${e.pid || "—"}</td>
      <td>
        <div class="ports">
          ${e.ports.map((p) => `<button class="port" data-action="open-url" data-port="${p}" title="Abrir http://localhost:${p}">${p}</button>`).join("")}
        </div>
      </td>
      <td>
        <div class="actions">
          <button class="btn small" data-action="reveal" ${folder ? "" : "disabled"} title="Abrir carpeta">Carpeta</button>
          <button class="btn small" data-action="editor" ${folder ? "" : "disabled"} title="Abrir en VS Code">Editor</button>
          <button class="btn small danger ${isConfirming ? "confirm" : ""}" data-action="stop" ${canStop ? "" : "disabled"}>
            ${isConfirming ? "¿Seguro?" : "Detener"}
          </button>
        </div>
      </td>
    </tr>`;
}

function render() {
  const list = visibleEntries();
  rowsEl.innerHTML = list.map(rowHtml).join("");
  freshIds.clear();

  const dev = entries.filter((e) => e.kind !== "system");
  const usedPorts = dev.flatMap((e) => e.ports).length;
  summaryEl.textContent = `${dev.length} apps · ${usedPorts} puertos en uso`;

  emptyEl.hidden = list.length > 0;
  if (!list.length) {
    emptyEl.textContent = searchEl.value
      ? "Nada coincide con el filtro."
      : "No hay apps de desarrollo escuchando en ningún puerto.";
  }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

async function run(label: string, fn: () => Promise<unknown>) {
  try {
    await fn();
  } catch (err) {
    log(label, String(err), "err");
  }
}

async function stop(e: PortEntry) {
  if (!confirming.has(e.id)) {
    confirming.set(e.id, window.setTimeout(() => (confirming.delete(e.id), render()), 3000));
    render();
    return;
  }
  clearTimeout(confirming.get(e.id));
  confirming.delete(e.id);
  log(e.appName, "deteniendo…");
  await run(e.appName, () =>
    e.kind === "docker"
      ? invoke("stop_container", { name: e.appName })
      : invoke("kill_process", { pid: e.pid, force: false }),
  );
  setTimeout(() => refresh(true), 600);
}

rowsEl.addEventListener("click", (ev) => {
  const btn = (ev.target as HTMLElement).closest<HTMLButtonElement>("button[data-action]");
  const row = btn?.closest<HTMLTableRowElement>("tr[data-id]");
  if (!btn || !row || btn.disabled) return;
  const e = entries.find((x) => x.id === row.dataset.id);
  if (!e) return;
  const folder = e.projectRoot ?? e.cwd;

  switch (btn.dataset.action) {
    case "open-url":
      run(e.appName, () => invoke("open_url", { port: Number(btn.dataset.port) }));
      break;
    case "reveal":
      if (folder) run(e.appName, () => invoke("reveal_folder", { path: folder }));
      break;
    case "editor":
      if (folder) run(e.appName, () => invoke("open_in_editor", { path: folder }));
      break;
    case "stop":
      stop(e);
      break;
  }
});

freeForm.addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const start = Math.max(1, Math.min(65535, Number(freeStart.value) || 3000));
  freeResult.hidden = false;
  freeResult.classList.remove("error");
  try {
    const port = await invoke<number>("find_free_port", { start, end: Math.min(65535, start + 999) });
    let copied = false;
    try {
      await navigator.clipboard.writeText(String(port));
      copied = true;
    } catch {
      /* clipboard may be unavailable; still show the port */
    }
    freeResult.textContent = copied ? `${port} · copiado` : String(port);
    log("portscope", `puerto libre sugerido: ${port}`);
  } catch (err) {
    freeResult.textContent = String(err);
    freeResult.classList.add("error");
  }
});

refreshBtn.addEventListener("click", () => refresh(true));
searchEl.addEventListener("input", render);
showSystemEl.addEventListener("change", render);
$<HTMLButtonElement>("#clear-log").addEventListener("click", () => (logEl.innerHTML = ""));

// Poll while visible; refresh immediately when the window regains focus.
setInterval(() => !document.hidden && refresh(), REFRESH_MS);
window.addEventListener("focus", () => refresh());
refresh(true);
