import { invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";

interface ToolInfo {
  id: string;
  name: string;
  versions: string[];
  selected: string | null;
  versionText: string | null;
  inPath: boolean;
  pathDirs: string[];
}

interface PathEntry {
  value: string;
  ours: boolean;
  exists: boolean;
}

interface TerminalInfo {
  shell: string;
  windowsTerminal: boolean;
  custom: string;
  wtAvailable: boolean;
  bashAvailable: boolean;
}

interface AppState {
  root: string;
  rootSupported: boolean;
  tools: ToolInfo[];
  userPath: PathEntry[];
  terminal: TerminalInfo;
}

interface Offer {
  version: string;
  lts: boolean;
  updates: string | null;
}

interface Updates {
  offers: Record<string, Offer[]>;
  errors: string[];
}

type JobPhase = "downloading" | "verifying" | "extracting" | "done" | "error";

interface Job {
  tool: string;
  version: string;
  phase: JobPhase;
  bytes: number;
  total: number | null;
  message: string | null;
  label: string | null;
  copiedFrom: string | null;
  copied: string[];
  sqlsrvMissing: boolean;
  backup: string | null;
}

type ServiceStatus = "stopped" | "starting" | "running" | "stopping" | "error";

interface ServiceInfo {
  id: string;
  name: string;
  status: ServiceStatus;
  message: string | null;
  port: number;
  pid: number | null;
  version: string | null;
}

const app = document.querySelector<HTMLDivElement>("#app")!;

let state: AppState | null = null;
let services: ServiceInfo[] = [];
let busy = false;
let notice: { kind: "info" | "error"; text: string } | null = null;
let updates: Updates | null = null;
let checking = false;
let job: Job | null = null;
/** Offer picked in each tool's download list, kept across re-renders. */
const chosenOffer: Record<string, string> = {};

const esc = (s: string) =>
  s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);

/** Runs a backend command, re-rendering with its returned state; `changesPath` shows the restart hint. */
async function run(cmd: string, args: Record<string, unknown> = {}, changesPath = false) {
  if (busy) return;
  busy = true;
  render();
  try {
    if (cmd === "open_location" || cmd === "open_terminal") {
      await invoke(cmd, args);
    } else {
      state = await invoke<AppState>(cmd, args);
      if (changesPath) notice = { kind: "info", text: t("pathChanged") };
    }
  } catch (e) {
    notice = { kind: "error", text: String(e) };
  } finally {
    busy = false;
    render();
  }
}

/** Service actions return the new service list; they don't block the rest of the UI. */
async function serviceCmd(cmd: string, args: Record<string, unknown> = {}) {
  try {
    services = await invoke<ServiceInfo[]>(cmd, args);
  } catch (e) {
    notice = { kind: "error", text: String(e) };
  }
  render();
  schedulePoll();
}

let pollTimer: number | undefined;

/** Polls quickly while a service is in transition, slowly otherwise (to notice crashes). */
function schedulePoll() {
  clearTimeout(pollTimer);
  const transitioning = services.some((s) => s.status === "starting" || s.status === "stopping");
  pollTimer = window.setTimeout(async () => {
    try {
      const next = await invoke<ServiceInfo[]>("get_services");
      if (JSON.stringify(next) !== JSON.stringify(services)) {
        services = next;
        render();
      }
    } finally {
      schedulePoll();
    }
  }, transitioning ? 400 : 2000);
}

const jobActive = (j: Job | null) => !!j && j.phase !== "done" && j.phase !== "error";

async function checkUpdates() {
  if (checking) return;
  checking = true;
  render();
  try {
    updates = await invoke<Updates>("check_updates");
    const count = Object.values(updates.offers)
      .flat()
      .filter((o) => o.updates).length;
    notice = updates.errors.length
      ? { kind: "error", text: t("checkFailed", { errors: updates.errors.join("\n") }) }
      : { kind: "info", text: count ? t("updatesFound", { count }) : t("upToDate") };
  } catch (e) {
    notice = { kind: "error", text: String(e) };
  } finally {
    checking = false;
    render();
  }
}

async function startDownload(tool: string) {
  const version = chosenOffer[tool] ?? updates?.offers[tool]?.[0]?.version;
  if (!version) return;
  try {
    job = await invoke<Job | null>("download_tool", { id: tool, version });
  } catch (e) {
    notice = { kind: "error", text: String(e) };
  }
  render();
  pollJob();
}

/** Follows a running download; when it ends, reloads the tools and the offers. */
function pollJob() {
  if (!jobActive(job)) return;
  window.setTimeout(async () => {
    try {
      job = await invoke<Job | null>("get_download");
      if (!jobActive(job)) {
        state = await invoke<AppState>("get_state");
        updates = await invoke<Updates | null>("get_updates");
      }
    } catch (e) {
      notice = { kind: "error", text: String(e) };
    }
    render();
    pollJob();
  }, 400);
}

async function dismissJob() {
  await invoke("dismiss_download");
  job = null;
  render();
}

const mb = (bytes: number) => (bytes / 1024 / 1024).toFixed(1);

const phaseLabel = { downloading: "dlDownloading", verifying: "dlVerifying", extracting: "dlExtracting" } as const;

/** Download progress/result for this tool, or the list of versions available to download. */
function downloadLine(tool: ToolInfo): string {
  if (job && job.tool === tool.id) {
    const what = `${tool.name} ${job.version}`;
    if (job.phase === "error") {
      return `
        <div class="tool-extra">
          <pre class="svc-message error">${esc(job.message ?? "")}</pre>
          <button class="link" data-action="dismiss_job">${esc(t("dismiss"))}</button>
        </div>`;
    }
    if (job.phase === "done") {
      const lines = [t("dlDone", { what })];
      if (job.copied.length) lines.push(t("dlCopied", { from: job.copiedFrom ?? "", files: job.copied.join(", ") }));
      if (job.sqlsrvMissing) lines.push(t("dlSqlsrvMissing", { series: job.version.split(".").slice(0, 2).join(".") }));
      if (job.backup) lines.push(t("dlComposerActive", { backup: job.backup }));
      else if (job.label && job.label !== tool.selected) lines.push(t("dlSelectHint"));
      const use =
        job.label && job.label !== tool.selected
          ? `<button class="primary" data-use="${tool.id}" data-label="${esc(job.label)}" ${busy ? "disabled" : ""}>${esc(
              t("dlUse", { version: job.version }),
            )}</button>`
          : "";
      return `
        <div class="tool-extra">
          <pre class="svc-message ${job.sqlsrvMissing ? "warn" : ""}">${esc(lines.join("\n"))}</pre>
          ${use}
          <button class="link" data-action="dismiss_job">${esc(t("dismiss"))}</button>
        </div>`;
    }
    const size = job.total ? `${mb(job.bytes)} / ${mb(job.total)} MB` : `${mb(job.bytes)} MB`;
    return `
      <div class="tool-extra">
        <span class="progress">${esc(t(phaseLabel[job.phase], { what }))}${job.phase === "downloading" ? ` ${esc(size)}` : ""}</span>
      </div>`;
  }

  const offers = updates?.offers[tool.id];
  if (!offers?.length) return "";
  const chosen = offers.some((o) => o.version === chosenOffer[tool.id]) ? chosenOffer[tool.id] : offers[0].version;
  const options = offers
    .map((o) => {
      const label = [o.version, o.lts ? "LTS" : "", o.updates ? t("dlUpdates", { version: o.updates }) : ""]
        .filter(Boolean)
        .join(" · ");
      return `<option value="${esc(o.version)}" ${o.version === chosen ? "selected" : ""}>${esc(label)}</option>`;
    })
    .join("");
  const hasUpdate = offers.some((o) => o.updates);
  return `
    <div class="tool-extra">
      ${hasUpdate ? `<span class="tag ours">${esc(t("updateAvailable"))}</span>` : `<span class="muted">${esc(t("dlOther"))}</span>`}
      <select data-offer="${tool.id}" class="narrow">${options}</select>
      <button data-download="${tool.id}" ${jobActive(job) ? "disabled" : ""}>${esc(t("download"))}</button>
    </div>`;
}

const statusLabel = {
  stopped: "statusStopped",
  starting: "statusStarting",
  running: "statusRunning",
  stopping: "statusStopping",
  error: "statusError",
} as const;

function serviceRow(svc: ServiceInfo): string {
  const transitioning = svc.status === "starting" || svc.status === "stopping";
  const running = svc.status === "running";
  const detail = [svc.version ?? t("notInstalled"), t("port", { port: svc.port }), svc.pid ? `PID ${svc.pid}` : ""]
    .filter(Boolean)
    .map(esc)
    .join(" · ");
  const message =
    svc.message && (svc.status === "error" || svc.status === "starting")
      ? `<pre class="svc-message ${svc.status}">${esc(svc.message)}</pre>`
      : "";

  return `
    <li class="service">
      <div class="service-line">
        <span class="dot ${svc.status}"></span>
        <div class="tool-main">
          <span class="tool-name">${esc(svc.name)}</span>
          <span class="tool-detail">${detail}</span>
        </div>
        <span class="svc-status ${svc.status}">${esc(t(statusLabel[svc.status]))}</span>
        <button class="${running ? "" : "primary"}" data-svc="${svc.id}" data-svc-action="${running ? "stop" : "start"}"
          ${transitioning || !svc.version ? "disabled" : ""}>${esc(t(running ? "stop" : "start"))}</button>
      </div>
      ${message}
    </li>`;
}

function toolRow(tool: ToolInfo): string {
  const installed = tool.versions.length > 0;
  const options = tool.versions
    .map((v) => `<option value="${esc(v)}" ${v === tool.selected ? "selected" : ""}>${esc(v)}</option>`)
    .join("");
  const detail = installed
    ? esc(tool.versionText ?? t("versionUnknown"))
    : esc(t("installHint", { id: tool.id }));

  return `
    <li class="tool ${installed ? "" : "missing"}">
      <div class="tool-main">
        <span class="tool-name">${esc(tool.name)}</span>
        <span class="tool-detail" title="${esc(tool.pathDirs.join("\n"))}">${detail}</span>
      </div>
      ${
        installed
          ? `<select data-version="${tool.id}" ${tool.versions.length < 2 || busy ? "disabled" : ""}>${options}</select>`
          : `<span class="badge">${esc(t("notInstalled"))}</span>`
      }
      <label class="switch" title="${esc(t("inPath"))}">
        <input type="checkbox" data-path="${tool.id}" ${tool.inPath ? "checked" : ""} ${!installed || busy ? "disabled" : ""} />
        <span class="slider"></span>
        <span class="switch-label">PATH</span>
      </label>
      ${downloadLine(tool)}
    </li>`;
}

const shellLabel = {
  cmd: "shellCmd",
  powershell: "shellPowershell",
  bash: "shellBash",
  custom: "shellCustom",
} as const;

function terminalSection(term: TerminalInfo, dis: string): string {
  const custom = term.shell === "custom";
  const options = (Object.keys(shellLabel) as (keyof typeof shellLabel)[])
    .map((id) => {
      const off = id === "bash" && !term.bashAvailable;
      const label = off ? t("bashUnavailable") : t(shellLabel[id]);
      return `<option value="${id}" ${id === term.shell ? "selected" : ""} ${off ? "disabled" : ""}>${esc(label)}</option>`;
    })
    .join("");

  return `
    <section>
      <div class="section-head">
        <h2>${esc(t("terminal"))}</h2>
        <div class="actions">
          <select data-terminal="shell" class="narrow" ${dis}>${options}</select>
          <label class="switch" title="${esc(term.wtAvailable ? "" : t("wtUnavailable"))}">
            <input type="checkbox" data-terminal="wt" ${term.windowsTerminal && term.wtAvailable ? "checked" : ""}
              ${custom || !term.wtAvailable || busy ? "disabled" : ""} />
            <span class="slider"></span>
            <span class="switch-label">${esc(t("useWindowsTerminal"))}</span>
          </label>
          <button class="primary" data-action="terminal" ${dis}>${esc(t("openTerminal"))}</button>
        </div>
      </div>
      ${
        custom
          ? `<input type="text" class="wide" data-terminal="custom" value="${esc(term.custom)}"
               placeholder="${esc(t("customPlaceholder"))}" spellcheck="false" ${dis} />`
          : ""
      }
      <p class="hint">${esc(t(custom ? "customHint" : "terminalHint"))}</p>
    </section>`;
}

function pathRow(entry: PathEntry): string {
  const tags = [
    entry.ours ? `<span class="tag ours">${esc(t("tagOurs"))}</span>` : "",
    entry.exists ? "" : `<span class="tag warn">${esc(t("tagMissing"))}</span>`,
  ].join("");
  return `<li class="${entry.ours ? "ours" : ""}"><code>${esc(entry.value)}</code>${tags}</li>`;
}

function render() {
  if (!state) {
    app.innerHTML = `<p class="loading">${esc(notice?.text ?? t("loading"))}</p>`;
    return;
  }
  const dis = busy ? "disabled" : "";
  const apacheRunning = services.some((s) => s.id === "apache" && s.status === "running");
  // Keep the User PATH panel open across re-renders.
  const pathOpen = app.querySelector("details")?.open ? "open" : "";

  app.innerHTML = `
    <header>
      <h1>${esc(t("appName"))}</h1>
      <button class="link" data-open="root" title="${esc(t("openRoot"))}">
        ${esc(t("root"))}: <code>${esc(state.root)}</code>
      </button>
      <button data-action="refresh" ${dis}>${esc(t("refresh"))}</button>
      <button data-action="quit" title="${esc(t("quitHint"))}">${esc(t("quit"))}</button>
    </header>

    ${state.rootSupported ? "" : `<div class="notice error"><span>${esc(t("rootUnsupported"))}</span></div>`}

    ${
      notice
        ? `<div class="notice ${notice.kind}"><span>${esc(notice.text)}</span>
             <button class="link" data-action="dismiss">${esc(t("dismiss"))}</button></div>`
        : ""
    }

    <section>
      <div class="section-head">
        <h2>${esc(t("services"))}</h2>
        <div class="actions">
          <button class="link" data-open="site" ${apacheRunning ? "" : "disabled"}>${esc(t("openSite"))}</button>
          <button class="link" data-open="www">${esc(t("openWww"))}</button>
          <button class="link" data-open="logs">${esc(t("openLogs"))}</button>
          <button class="link" data-open="config">${esc(t("openConfig"))}</button>
          <button data-action="start_all">${esc(t("startAll"))}</button>
          <button data-action="stop_all">${esc(t("stopAll"))}</button>
        </div>
      </div>
      <ul class="tools">${services.map(serviceRow).join("")}</ul>
    </section>

    ${terminalSection(state.terminal, dis)}

    <section>
      <div class="section-head">
        <h2>${esc(t("tools"))}</h2>
        <div class="actions">
          <button data-action="check_updates" ${checking ? "disabled" : ""} title="${esc(t("checkUpdatesHint"))}">
            ${esc(t(checking ? "checking" : "checkUpdates"))}</button>
          <button data-action="add_all" ${dis}>${esc(t("addAllToPath"))}</button>
          <button data-action="remove_all" ${dis}>${esc(t("removeAllFromPath"))}</button>
        </div>
      </div>
      <ul class="tools">${state.tools.map(toolRow).join("")}</ul>
    </section>

    <section>
      <details ${pathOpen}>
        <summary><h2>${esc(t("userPath"))}</h2> <span class="count">${state.userPath.length}</span></summary>
        <p class="hint">${esc(t("userPathHint"))}</p>
        <ul class="path-list">${state.userPath.map(pathRow).join("")}</ul>
      </details>
    </section>

    <footer class="hint">${esc(t("trayHint"))}</footer>`;
}

app.addEventListener("click", (ev) => {
  const target = ev.target as HTMLElement;
  const svc = target.closest<HTMLElement>("[data-svc]");
  if (svc) {
    return serviceCmd(svc.dataset.svcAction === "stop" ? "stop_service" : "start_service", { id: svc.dataset.svc });
  }
  const download = target.closest<HTMLElement>("[data-download]")?.dataset.download;
  if (download) {
    return startDownload(download);
  }
  const use = target.closest<HTMLElement>("[data-use]");
  if (use) {
    const tool = state?.tools.find((x) => x.id === use.dataset.use);
    return run("set_tool_version", { id: use.dataset.use, version: use.dataset.label }, tool?.inPath ?? false).then(
      dismissJob,
    );
  }
  const open = target.closest<HTMLElement>("[data-open]")?.dataset.open;
  if (open) {
    return run("open_location", { target: open });
  }
  switch (target.closest<HTMLElement>("[data-action]")?.dataset.action) {
    case "refresh":
      return run("refresh");
    case "start_all":
      return serviceCmd("start_all");
    case "stop_all":
      return serviceCmd("stop_all");
    case "add_all":
      return run("set_all_in_path", { enabled: true }, true);
    case "remove_all":
      return run("set_all_in_path", { enabled: false }, true);
    case "terminal":
      return openTerminal();
    case "check_updates":
      return checkUpdates();
    case "dismiss_job":
      return dismissJob();
    case "quit":
      return invoke("quit");
    case "dismiss":
      notice = null;
      return render();
  }
});

/** Saves the terminal choice, taking `changed` over the current state. */
function setTerminal(changed: Partial<Pick<TerminalInfo, "shell" | "windowsTerminal" | "custom">>) {
  if (!state) return;
  const { shell, windowsTerminal, custom } = { ...state.terminal, ...changed };
  run("set_terminal", { shell, windowsTerminal, custom });
}

let customTimer: number | undefined;

/** Saves the custom terminal path without re-rendering (that would drop focus and swallow clicks). */
async function saveCustom(value: string) {
  clearTimeout(customTimer);
  if (!state || value.trim() === state.terminal.custom) return;
  try {
    state = await invoke<AppState>("set_terminal", { ...state.terminal, custom: value });
  } catch (e) {
    notice = { kind: "error", text: String(e) };
    render();
  }
}

async function openTerminal() {
  // Save what was just typed before the debounce fires.
  const input = app.querySelector<HTMLInputElement>('[data-terminal="custom"]');
  if (input) await saveCustom(input.value);
  run("open_terminal");
}

app.addEventListener("input", (ev) => {
  const el = ev.target as HTMLInputElement;
  if (el.dataset.terminal === "custom") {
    clearTimeout(customTimer);
    customTimer = window.setTimeout(() => saveCustom(el.value), 400);
  }
});

app.addEventListener("change", (ev) => {
  const el = ev.target as HTMLInputElement | HTMLSelectElement;
  const term = el.dataset.terminal;
  if (el.dataset.offer) {
    chosenOffer[el.dataset.offer] = el.value;
  } else if (term === "shell") {
    setTerminal({ shell: el.value });
  } else if (term === "wt") {
    setTerminal({ windowsTerminal: (el as HTMLInputElement).checked });
  } else if (el.dataset.path) {
    run("set_tool_in_path", { id: el.dataset.path, enabled: (el as HTMLInputElement).checked }, true);
  } else if (el.dataset.version) {
    const tool = state?.tools.find((x) => x.id === el.dataset.version);
    run("set_tool_version", { id: el.dataset.version, version: el.value }, tool?.inPath ?? false);
  }
});

render();
// The download job and the last check survive a window reload (they live in the backend).
Promise.all([
  invoke<AppState>("get_state"),
  invoke<ServiceInfo[]>("get_services"),
  invoke<Job | null>("get_download"),
  invoke<Updates | null>("get_updates"),
])
  .then(([s, svcs, j, u]) => {
    state = s;
    services = svcs;
    job = j;
    updates = u;
    pollJob();
  })
  .catch((e) => (notice = { kind: "error", text: String(e) }))
  .finally(() => {
    render();
    schedulePoll();
    // Make the window tall enough for everything on the first view (limited to the screen).
    invoke("fit_window", { contentHeight: document.documentElement.scrollHeight }).catch(() => {});
  });
