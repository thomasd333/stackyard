import { invoke } from "@tauri-apps/api/core";
import { languageNames, setLocale, t } from "./i18n";

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

interface SiteInfo {
  name: string;
  host: string;
  root: string;
  url: string;
  exists: boolean;
}

interface AppState {
  root: string;
  rootSupported: boolean;
  tools: ToolInfo[];
  userPath: PathEntry[];
  terminal: TerminalInfo;
  sites: SiteInfo[];
  siteSuggestions: string[];
  /** Address of the default host (localhost -> www). */
  mainUrl: string;
  ui: UiInfo;
  ports: { apache: number; mysql: number };
  version: string;
  /** Parse error of a settings.json that was set aside and replaced (sent once). */
  settingsRecovered: string | null;
}

interface UiInfo {
  theme: string;
  language: string;
  /** What `language` resolves to ("system" becomes the Windows display language). */
  locale: string;
  /** What "system" resolves to. */
  systemLocale: string;
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
/** "Add site" form; `rootEdited` stops the folder from following the name once typed by hand. */
const siteDraft = { name: "", root: "", rootEdited: false };

const TABS = ["services", "tools", "sites", "settings"] as const;
type Tab = (typeof TABS)[number];
const tabLabel = { services: "tabServices", tools: "tabTools", sites: "tabSites", settings: "tabSettings" } as const;

/**
 * Applies the theme and language from settings. They are also kept in localStorage so the next start
 * paints right before the backend answers; settings.json stays the source of truth.
 */
let appliedUi = "";

function applyUi(ui: Pick<UiInfo, "theme" | "locale">) {
  const key = JSON.stringify({ theme: ui.theme, locale: ui.locale });
  if (key === appliedUi) return;
  appliedUi = key;
  setLocale(ui.locale);
  const root = document.documentElement;
  if (ui.theme === "light" || ui.theme === "dark") root.dataset.theme = ui.theme;
  else delete root.dataset.theme;
  try {
    localStorage.setItem("ui", key);
  } catch {
    // Only the first paint of the next start is affected.
  }
}

try {
  const saved = JSON.parse(localStorage.getItem("ui") ?? "null");
  if (saved) applyUi(saved);
} catch {
  // Defaults until the state arrives.
}

/** The last tab is remembered per machine; storage may be unavailable, so it is only a convenience. */
let tab: Tab = (() => {
  try {
    const saved = localStorage.getItem("tab");
    return TABS.find((x) => x === saved) ?? "services";
  } catch {
    return "services";
  }
})();

function setTab(next: Tab) {
  tab = next;
  try {
    localStorage.setItem("tab", next);
  } catch {
    // Not remembered, that's all.
  }
  render();
}

const esc = (s: string) =>
  s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);

/** A tooltip attribute. */
const tip = (text: string) => `title="${esc(text)}"`;

/** Takes a new state from the backend; a settings.json it had to set aside is reported once. */
function adopt(next: AppState) {
  state = next;
  applyUi(next.ui);
  if (next.settingsRecovered) notice = { kind: "error", text: t("settingsRecovered", { error: next.settingsRecovered }) };
}

/** Runs a backend command, re-rendering with its returned state; `changesPath` shows the restart hint. */
async function run(cmd: string, args: Record<string, unknown> = {}, changesPath = false) {
  if (busy) return;
  busy = true;
  render();
  try {
    if (cmd === "open_location" || cmd === "open_terminal") {
      await invoke(cmd, args);
    } else {
      adopt(await invoke<AppState>(cmd, args));
      if (changesPath) notice = { kind: "info", text: t("pathChanged") };
    }
    return true;
  } catch (e) {
    notice = { kind: "error", text: String(e) };
    return false;
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
        adopt(await invoke<AppState>("get_state"));
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
      <button data-download="${tool.id}" ${tip(t("downloadHint"))} ${jobActive(job) ? "disabled" : ""}>${esc(t("download"))}</button>
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
          ? `<select data-version="${tool.id}" ${tip(t("versionHint"))} ${tool.versions.length < 2 || busy ? "disabled" : ""}>${options}</select>`
          : `<span class="badge">${esc(t("notInstalled"))}</span>`
      }
      <label class="switch" ${tip(t("inPath"))}>
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
      <div class="section-head"><h2>${esc(t("terminal"))}</h2></div>
      <div class="tools">
        <div class="tool terminal">
          <select data-terminal="shell" class="narrow" ${tip(t("shellHint"))} ${dis}>${options}</select>
          <label class="switch" ${tip(t(term.wtAvailable ? "wtHint" : "wtUnavailable"))}>
            <input type="checkbox" data-terminal="wt" ${term.windowsTerminal && term.wtAvailable ? "checked" : ""}
              ${custom || !term.wtAvailable || busy ? "disabled" : ""} />
            <span class="slider"></span>
            <span class="switch-label">${esc(t("useWindowsTerminal"))}</span>
          </label>
          <button class="primary" data-action="terminal" ${dis}>${esc(t("openTerminal"))}</button>
          ${
            custom
              ? `<input type="text" class="wide" id="terminal-custom" data-terminal="custom" value="${esc(term.custom)}"
                   placeholder="${esc(t("customPlaceholder"))}" spellcheck="false" ${dis} />`
              : ""
          }
        </div>
      </div>
      <p class="hint">${esc(t(custom ? "customHint" : "terminalHint"))}</p>
    </section>`;
}

function pathRow(entry: PathEntry): string {
  const tags = [
    entry.ours ? `<span class="tag ours" ${tip(t("tagOursHint"))}>${esc(t("tagOurs"))}</span>` : "",
    entry.exists ? "" : `<span class="tag warn" ${tip(t("tagMissingHint"))}>${esc(t("tagMissing"))}</span>`,
  ].join("");
  return `<li class="${entry.ours ? "ours" : ""}"><code>${esc(entry.value)}</code>${tags}</li>`;
}

/** Link that opens a host in the browser; disabled (with the reason as tooltip) while Apache is stopped. */
function openLink(target: string, url: string, apacheRunning: boolean): string {
  return `<button class="link" data-open="${esc(target)}" ${tip(t(apacheRunning ? "openInBrowser" : "startApacheFirst"))}
    ${apacheRunning ? "" : "disabled"}>${esc(url)}</button>`;
}

function siteRow(site: SiteInfo, apacheRunning: boolean, dis: string): string {
  return `
    <li class="tool">
      <div class="tool-main">
        <span class="tool-name">${esc(site.host)}</span>
        <span class="tool-detail" title="${esc(site.root)}"><code>${esc(site.root)}</code></span>
      </div>
      ${site.exists ? "" : `<span class="tag warn">${esc(t("siteMissing"))}</span>`}
      ${openLink(`site:${site.name}`, site.url, apacheRunning)}
      <button data-remove-site="${esc(site.name)}" ${tip(t("removeSiteHint"))} ${dis}>${esc(t("remove"))}</button>
    </li>`;
}

/** The built-in localhost -> www host, always first and not removable. */
function mainSiteRow(s: AppState, apacheRunning: boolean): string {
  return `
    <li class="tool">
      <div class="tool-main">
        <span class="tool-name">localhost <span class="tag" ${tip(t("mainHostHint"))}>${esc(t("mainHost"))}</span></span>
        <span class="tool-detail"><code>www</code></span>
      </div>
      ${openLink("site", s.mainUrl, apacheRunning)}
    </li>`;
}

function sitesTab(s: AppState, apacheRunning: boolean, dis: string): string {
  const options = s.siteSuggestions.map((x) => `<option value="${esc(x)}"></option>`).join("");
  const list = `
    <ul class="tools">${mainSiteRow(s, apacheRunning)}${s.sites.map((x) => siteRow(x, apacheRunning, dis)).join("")}</ul>
    ${s.sites.length ? "" : `<p class="empty">${esc(t("noSites"))}</p>`}`;
  return `
    <section>
      <div class="section-head"><h2>${esc(t("sites"))}</h2></div>
      ${list}
    </section>

    <section>
      <div class="section-head"><h2>${esc(t("addSite"))}</h2></div>
      <form class="site-form" data-form="site">
        <label class="field">
          <span>${esc(t("siteName"))}</span>
          <span class="host-input">
            <input type="text" id="site-name" data-site="name" value="${esc(siteDraft.name)}"
              placeholder="myapp" spellcheck="false" autocomplete="off" ${dis} />
            <span class="suffix">.localhost</span>
          </span>
        </label>
        <label class="field grow" ${tip(t("siteFolderHint"))}>
          <span>${esc(t("siteFolder"))}</span>
          <input type="text" id="site-root" data-site="root" value="${esc(siteDraft.root)}" list="site-folders"
            placeholder="www\\myapp\\public" spellcheck="false" autocomplete="off" ${dis} />
          <datalist id="site-folders">${options}</datalist>
        </label>
        <button class="primary" type="submit" ${dis}>${esc(t("add"))}</button>
      </form>
      <p class="hint">${esc(t("sitesHint"))}</p>
    </section>`;
}

const themeLabel = { system: "themeSystem", light: "themeLight", dark: "themeDark" } as const;

function settingsTab(s: AppState, dis: string): string {
  const option = (value: string, label: string, selected: string) =>
    `<option value="${value}" ${value === selected ? "selected" : ""}>${esc(label)}</option>`;
  const themes = (Object.keys(themeLabel) as (keyof typeof themeLabel)[])
    .map((id) => option(id, t(themeLabel[id]), s.ui.theme))
    .join("");
  const languages = [
    option("system", t("languageSystem", { language: languageNames[s.ui.systemLocale] ?? "" }), s.ui.language),
    ...Object.entries(languageNames).map(([id, name]) => option(id, name, s.ui.language)),
  ].join("");
  const folders = (
    [
      ["root", "Stackyard", ""],
      ["www", t("openWww"), "\\www"],
      ["logs", t("openLogs"), "\\logs"],
      ["config", t("openConfig"), "\\config"],
    ] as const
  )
    .map(
      ([target, label, sub]) =>
        `<button data-open="${target}" ${tip(t("folderHint", { path: s.root + sub }))}>${esc(label)}</button>`,
    )
    .join("");
  return `
    <section>
      <div class="section-head"><h2>${esc(t("appearance"))}</h2></div>
      <div class="form-row">
        <label class="field">
          <span>${esc(t("theme"))}</span>
          <select data-ui="theme" class="narrow" ${dis}>${themes}</select>
        </label>
        <label class="field">
          <span>${esc(t("language"))}</span>
          <select data-ui="language" class="narrow" ${dis}>${languages}</select>
        </label>
      </div>
    </section>

    <section>
      <div class="section-head"><h2>${esc(t("ports"))}</h2></div>
      <form class="form-row" data-form="ports">
        <label class="field">
          <span>${esc(t("portApache"))}</span>
          <input type="number" class="port" name="apache" min="1" max="65535" value="${s.ports.apache}" required ${dis} />
        </label>
        <label class="field">
          <span>${esc(t("portMysql"))}</span>
          <input type="number" class="port" name="mysql" min="1" max="65535" value="${s.ports.mysql}" required ${dis} />
        </label>
        <button type="submit" ${dis}>${esc(t("save"))}</button>
      </form>
      <p class="hint">${esc(t("portsHint"))}</p>
    </section>

    <section>
      <div class="section-head"><h2>${esc(t("folders"))}</h2></div>
      <div class="form-row">${folders}</div>
    </section>`;
}

function servicesTab(s: AppState, dis: string): string {
  return `
    <section>
      <div class="section-head">
        <h2>${esc(t("services"))}</h2>
        <div class="actions">
          <button data-action="start_all" ${tip(t("startAllHint"))}>${esc(t("startAll"))}</button>
          <button data-action="stop_all" ${tip(t("stopAllHint"))}>${esc(t("stopAll"))}</button>
        </div>
      </div>
      <ul class="tools">${services.map(serviceRow).join("")}</ul>
    </section>

    ${terminalSection(s.terminal, dis)}`;
}

function toolsTab(s: AppState, dis: string): string {
  // Keep the User PATH panel open across re-renders.
  const pathOpen = app.querySelector("details")?.open ? "open" : "";
  return `
    <section>
      <div class="section-head">
        <h2>${esc(t("tools"))}</h2>
        <div class="actions">
          <button data-action="check_updates" ${checking ? "disabled" : ""} title="${esc(t("checkUpdatesHint"))}">
            ${esc(t(checking ? "checking" : "checkUpdates"))}</button>
          <button data-action="add_all" ${tip(t("addAllHint"))} ${dis}>${esc(t("addAllToPath"))}</button>
          <button data-action="remove_all" ${tip(t("removeAllHint"))} ${dis}>${esc(t("removeAllFromPath"))}</button>
        </div>
      </div>
      <ul class="tools">${s.tools.map(toolRow).join("")}</ul>
    </section>

    <section>
      <details ${pathOpen}>
        <summary><h2>${esc(t("userPath"))}</h2> <span class="count">${s.userPath.length}</span></summary>
        <p class="hint">${esc(t("userPathHint"))}</p>
        <ul class="path-list">${s.userPath.map(pathRow).join("")}</ul>
      </details>
    </section>`;
}

/** A small marker on a tab that needs attention (a failed service, an available update, a running download). */
function tabFlag(id: Tab): string {
  const flagged =
    id === "services"
      ? services.some((s) => s.status === "error")
      : id === "tools"
        ? jobActive(job) || Object.values(updates?.offers ?? {}).some((o) => o.some((x) => x.updates))
        : false;
  return flagged ? `<span class="flag"></span>` : "";
}

function render() {
  if (!state) {
    app.innerHTML = `<p class="loading">${esc(notice?.text ?? t("loading"))}</p>`;
    return;
  }
  applyUi(state.ui);
  const dis = busy ? "disabled" : "";
  const apacheRunning = services.some((s) => s.id === "apache" && s.status === "running");
  // Re-rendering replaces the inputs; put the cursor back where it was.
  const focused = document.activeElement as HTMLInputElement | null;
  const focus = focused?.id ? { id: focused.id, start: focused.selectionStart, end: focused.selectionEnd } : null;

  const tabs = TABS.map(
    (id) => `<button role="tab" class="tab ${id === tab ? "active" : ""}" aria-selected="${id === tab}"
      data-tab="${id}">${esc(t(tabLabel[id]))}${tabFlag(id)}</button>`,
  ).join("");
  const body =
    tab === "services"
      ? servicesTab(state, dis)
      : tab === "tools"
        ? toolsTab(state, dis)
        : tab === "sites"
          ? sitesTab(state, apacheRunning, dis)
          : settingsTab(state, dis);

  app.innerHTML = `
    <header>
      <h1>${esc(t("appName"))} <span class="version">${esc(state.version)}</span></h1>
      <button class="link" data-open="root" title="${esc(t("openRoot"))}">
        ${esc(t("root"))}: <code>${esc(state.root)}</code>
      </button>
      <button data-action="refresh" ${tip(t("refreshHint"))} ${dis}>${esc(t("refresh"))}</button>
      <button data-action="quit" title="${esc(t("quitHint"))}">${esc(t("quit"))}</button>
    </header>

    <nav class="tabs" role="tablist">${tabs}</nav>

    ${state.rootSupported ? "" : `<div class="notice error"><span>${esc(t("rootUnsupported"))}</span></div>`}

    ${
      notice
        ? `<div class="notice ${notice.kind}"><span>${esc(notice.text)}</span>
             <button class="link" data-action="dismiss">${esc(t("dismiss"))}</button></div>`
        : ""
    }

    ${body}`;

  if (focus) {
    const el = document.getElementById(focus.id) as HTMLInputElement | null;
    if (el && !el.disabled) {
      el.focus();
      if (focus.start !== null) el.setSelectionRange(focus.start, focus.end);
    }
  }
}

/** Adds the site from the form; the form is cleared only when it worked. */
async function addSite() {
  if (await run("add_site", { name: siteDraft.name, root: siteDraft.root })) {
    Object.assign(siteDraft, { name: "", root: "", rootEdited: false });
    notice = null;
    render();
  }
}

/** Fills the folder from the name (www\<name>\public, else www\<name>) until it is edited by hand. */
function followName() {
  if (siteDraft.rootEdited || !state) return;
  const name = siteDraft.name.trim().toLowerCase();
  const match = (x: string) => state!.siteSuggestions.find((s) => s.toLowerCase() === x);
  siteDraft.root = (name && (match(`www\\${name}\\public`) ?? match(`www\\${name}`))) || "";
  const input = app.querySelector<HTMLInputElement>("#site-root");
  if (input) input.value = siteDraft.root;
}

app.addEventListener("click", (ev) => {
  const target = ev.target as HTMLElement;
  const tabId = target.closest<HTMLElement>("[data-tab]")?.dataset.tab;
  if (tabId) {
    return setTab(tabId as Tab);
  }
  const removeSite = target.closest<HTMLElement>("[data-remove-site]")?.dataset.removeSite;
  if (removeSite) {
    return run("remove_site", { name: removeSite });
  }
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
    adopt(await invoke<AppState>("set_terminal", { ...state.terminal, custom: value }));
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

app.addEventListener("submit", (ev) => {
  ev.preventDefault();
  const form = ev.target as HTMLFormElement;
  if (form.dataset.form === "site") addSite();
  if (form.dataset.form === "ports") {
    const value = (name: string) => Number((form.elements.namedItem(name) as HTMLInputElement).value);
    run("set_ports", { apache: value("apache"), mysql: value("mysql") });
  }
});

app.addEventListener("input", (ev) => {
  const el = ev.target as HTMLInputElement;
  if (el.dataset.site === "name") {
    siteDraft.name = el.value;
    followName();
  } else if (el.dataset.site === "root") {
    siteDraft.root = el.value;
    siteDraft.rootEdited = el.value.trim() !== "";
  } else if (el.dataset.terminal === "custom") {
    clearTimeout(customTimer);
    customTimer = window.setTimeout(() => saveCustom(el.value), 400);
  }
});

app.addEventListener("change", (ev) => {
  const el = ev.target as HTMLInputElement | HTMLSelectElement;
  const term = el.dataset.terminal;
  if (el.dataset.ui && state) {
    const { theme, language } = state.ui;
    run("set_ui", el.dataset.ui === "theme" ? { theme: el.value, language } : { theme, language: el.value });
  } else if (el.dataset.offer) {
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
// `npm run dev` in a plain browser: a fake backend, to preview the UI without building (see mock.ts).
const backend = import.meta.env.DEV && !("__TAURI_INTERNALS__" in window) ? import("./mock") : Promise.resolve();
// The download job and the last check survive a window reload (they live in the backend).
backend.then(() => Promise.all([
  invoke<AppState>("get_state"),
  invoke<ServiceInfo[]>("get_services"),
  invoke<Job | null>("get_download"),
  invoke<Updates | null>("get_updates"),
]))
  .then(([s, svcs, j, u]) => {
    adopt(s);
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
