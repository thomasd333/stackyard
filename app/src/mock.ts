// Fake backend for `npm run dev` in a plain browser (no Tauri), so the UI can be previewed without a
// build. Loaded only in dev when Tauri is absent; it never ends up in the exe.

type Args = Record<string, any>;

const root = "D:\\Stackyard";
const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));

const state = {
  root,
  rootSupported: true,
  tools: [
    tool("apache", "Apache", ["httpd-2.4.68-260617-Win64-VS18"], "Apache/2.4.68 (Win64)", true),
    tool("php", "PHP", ["php-8.5.12-1-Win32-vs17-x64", "php-8.3.36-1-Win32-vs16-x64"], "PHP 8.5.12 (cli)", true),
    tool("mysql", "MySQL", ["mysql-8.4.3-winx64"], "mysqld Ver 8.4.3", true),
    tool("postgresql", "PostgreSQL", [], null, false),
    tool("nodejs", "Node.js", ["node-v26.12.1-win-x64", "node-v22"], "v26.12.1", true),
    tool("composer", "Composer", ["default"], "Composer 2.9.1", true),
    tool("git", "Git", ["default"], "git version 2.51.0.windows.1", false),
    tool("python", "Python", ["python-3.13"], "Python 3.13.1", false),
  ],
  userPath: [
    { value: "C:\\Users\\me\\AppData\\Local\\Microsoft\\WindowsApps", ours: false, exists: true },
    { value: `${root}\\bin\\php\\php-8.5.12-1-Win32-vs17-x64`, ours: true, exists: true },
    { value: `${root}\\bin\\nodejs\\node-v26.12.1-win-x64`, ours: true, exists: true },
    { value: "C:\\old\\tool", ours: false, exists: false },
  ],
  terminal: { shell: "cmd", windowsTerminal: true, custom: "", wtAvailable: true, bashAvailable: true },
  sites: [] as { name: string; host: string; root: string; url: string; exists: boolean }[],
  siteSuggestions: ["www\\shop\\public", "www\\shop", "www\\blog"],
  mainUrl: "http://localhost/",
  ports: { apache: 80, mysql: 3306 },
  startup: { withWindows: false, restoreServices: true },
  version: "0.7.0",
  settingsRecovered: null as string | null,
  ui: { theme: "system", language: "system", locale: "en", systemLocale: "en" },
};
state.ui.systemLocale = state.ui.locale = navigator.language.startsWith("pl") ? "pl" : "en";

function tool(id: string, name: string, versions: string[], versionText: string | null, inPath: boolean) {
  return { id, name, versions, selected: versions[0] ?? null, versionText, inPath: inPath && versions.length > 0, pathDirs: [] };
}

const services = [
  { id: "apache", name: "Apache", status: "running", message: null, port: 80, pid: 12345, version: "2.4.68" },
  { id: "mysql", name: "MySQL", status: "stopped", message: null, port: 3306, pid: null, version: "8.4.3" },
] as { id: string; name: string; status: string; message: string | null; port: number; pid: number | null; version: string | null }[];

/** Moves a service through starting/stopping like the real one, so polling and the dots can be seen. */
function setService(id: string, run: boolean) {
  const s = services.find((x) => x.id === id);
  if (!s || (run ? s.status === "running" : s.status === "stopped")) return;
  s.status = run ? "starting" : "stopping";
  setTimeout(() => {
    s.status = run ? "running" : "stopped";
    s.pid = run ? 10000 + Math.floor(Math.random() * 30000) : null;
  }, 1200);
}

const copy = <T>(x: T): T => JSON.parse(JSON.stringify(x));

const handlers: Record<string, (a: Args) => unknown> = {
  get_state: () => state,
  refresh: () => state,
  get_services: () => services,
  start_service: (a) => (setService(a.id, true), services),
  stop_service: (a) => (setService(a.id, false), services),
  start_all: () => (services.forEach((s) => setService(s.id, true)), services),
  stop_all: () => (services.forEach((s) => setService(s.id, false)), services),
  set_tool_version: (a) => {
    state.tools.find((x) => x.id === a.id)!.selected = a.version;
    return state;
  },
  set_tool_in_path: (a) => {
    state.tools.find((x) => x.id === a.id)!.inPath = a.enabled;
    return state;
  },
  set_all_in_path: (a) => {
    state.tools.forEach((x) => (x.inPath = a.enabled && x.versions.length > 0));
    return state;
  },
  set_terminal: (a) => {
    Object.assign(state.terminal, { shell: a.shell, windowsTerminal: a.windowsTerminal, custom: a.custom });
    return state;
  },
  set_ui: (a) => {
    state.ui.theme = a.theme;
    state.ui.language = a.language;
    state.ui.locale = a.language === "system" ? state.ui.systemLocale : a.language;
    return state;
  },
  set_startup: (a) => {
    state.startup = { withWindows: a.withWindows, restoreServices: a.restoreServices };
    return state;
  },
  set_ports: (a) => {
    if (a.apache === a.mysql) throw "Apache and MySQL need different ports.";
    state.ports = { apache: a.apache, mysql: a.mysql };
    services[0].port = a.apache;
    services[1].port = a.mysql;
    state.mainUrl = a.apache === 80 ? "http://localhost/" : `http://localhost:${a.apache}/`;
    return state;
  },
  add_site: (a) => {
    const name = String(a.name).trim().toLowerCase();
    if (!/^[a-z0-9-]+$/.test(name)) throw "Use letters, digits and hyphens only.";
    if (state.sites.some((s) => s.name === name)) throw `${name}.localhost already exists.`;
    state.sites.push({ name, host: `${name}.localhost`, root: a.root || `www\\${name}`, url: `http://${name}.localhost/`, exists: true });
    return state;
  },
  remove_site: (a) => {
    state.sites = state.sites.filter((s) => s.name !== a.name);
    return state;
  },
  check_updates: async () => {
    await delay(800);
    return {
      offers: {
        php: [{ version: "8.5.13", lts: false, updates: "8.5.12" }, { version: "8.4.20", lts: false, updates: null }],
        nodejs: [{ version: "24.11.0", lts: true, updates: null }],
      },
      errors: [],
    };
  },
  get_updates: () => null,
  get_download: () => null,
  download_tool: () => {
    throw "Downloads are not simulated in the preview.";
  },
  dismiss_download: () => null,
  open_location: (a) => console.info("open_location", a.target),
  open_terminal: () => console.info("open_terminal", state.terminal),
  fit_window: () => null,
  quit: () => alert("Quit (preview)"),
};

(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: Args = {}) => {
    const handler = handlers[cmd];
    if (!handler) throw `Not mocked: ${cmd}`;
    await delay(60);
    return copy(await handler(args));
  },
  transformCallback: () => 0,
  unregisterCallback: () => {},
};

export {};
