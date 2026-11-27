"use strict";
// Setup wizard page. Talks to Rust with window.ipc.postMessage(JSON) and receives window.ot.receive({type, data}).
// Opened in a plain browser (no window.ipc) it runs against a small mock so the screens can be designed there.

const $ = (selector, root = document) => root.querySelector(selector);
const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];

// The user agreement comes right after the welcome, before the questions (user request, 2026-11-24).
const SCREENS = ["welcome", "license", "about", "install", "look", "buttons", "widgets", "done"];
const CUSTOMIZE = ["look", "buttons", "widgets"];
const DEFAULT_RADIUS = { Compact: 16, Medium: 19, Large: 22 };
const PREVIEW_ICON = { Compact: 16, Medium: 19, Large: 22 };
const REAL_ICON = { Compact: 28, Medium: 34, Large: 38 };
const APP_COLORS = [["#4f8cff", "#7b5cff"], ["#ff6a88", "#ff9a5a"], ["#2fd59b", "#1aa6c9"], ["#ffc94a", "#ff8f3a"]];
const GLYPHS = {
  start: '<svg viewBox="0 0 24 24"><path d="M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z" fill="currentColor" stroke="none"/></svg>',
  search: '<svg viewBox="0 0 24 24"><circle cx="10.5" cy="10.5" r="6" stroke="currentColor"/><path d="M15 15l5 5" stroke="currentColor"/></svg>',
  taskview: '<svg viewBox="0 0 24 24"><rect x="3" y="5" width="8" height="14" rx="2" stroke="currentColor"/><rect x="13" y="5" width="8" height="14" rx="2" stroke="currentColor"/></svg>',
  desktop: '<svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="13" rx="2" stroke="currentColor"/><path d="M8 21h8M12 17v4" stroke="currentColor"/></svg>',
  recycle: '<svg viewBox="0 0 24 24"><path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13" stroke="currentColor"/></svg>'
};

const state = {
  index: 0,
  settings: {},
  widgets: [],
  languages: [],
  systemTheme: "dark",
  dryRun: false,
  demo: false,
  installed: false,
  installing: false,
  accepted: false,
  startWithWindows: true,
  previewKeys: new Map()
};

// ---------- Messaging ----------

function post(message) {
  if (window.ipc) window.ipc.postMessage(JSON.stringify(message));
  else mock(message);
}

window.ot = {
  receive(message) {
    const handler = handlers[message.type];
    if (handler) handler(message.data);
  }
};

const handlers = {
  init(data) {
    state.settings = data.settings || {};
    state.widgets = data.widgets || [];
    state.languages = data.languages || [];
    state.systemTheme = data.systemTheme || "dark";
    state.dryRun = !!data.dryRun;
    state.demo = !!data.demo;
    state.installed = !!data.installed;
    $("#dry-badge").hidden = !state.dryRun;
    $("#import-card").hidden = !data.canImport;
    fillLanguages();
    applyTheme();
    applyTexts();
    syncControls();
    renderWidgets();
    show(0, "forward");
    requestAnimationFrame(() => $("#app").classList.add("ready"));
    if (state.demo) runDemo();
  },
  widgets(list) {
    state.widgets = list;
    renderWidgets();
  },
  settings(settings) {
    state.settings = settings;
    applyTheme();
    applyTexts();
    syncControls();
    renderWidgets();
    const card = $("#import-card");
    card.classList.add("done");
    $("#import-help").textContent = t("importDone");
    $("#btn-import").hidden = true;
  },
  imported(result) {
    if (!result.ok) $("#import-help").textContent = t("importFailed");
  },
  installed(result) {
    state.installing = false;
    const install = $("#install");
    if (result.ok) {
      state.installed = true;
      install.dataset.state = "done";
    } else {
      install.dataset.state = "error";
      $("#install-error").textContent = result.error || "";
    }
    updateNav();
    if (state.demo && result.ok) setTimeout(next, 1600);
  },
  systemTheme(theme) {
    state.systemTheme = theme;
    applyTheme();
  }
};

// ---------- Texts and theme ----------

function language() {
  return String(state.settings.Language || "en-US").slice(0, 2).toLowerCase();
}

function t(key) {
  const table = STRINGS[language()] || STRINGS.en;
  return key in table ? table[key] : STRINGS.en[key];
}

function applyTexts() {
  document.documentElement.lang = language();
  for (const element of $$("[data-i18n]")) element.textContent = t(element.dataset.i18n);
  for (const element of $$("[data-i18n-label]")) {
    element.setAttribute("aria-label", t(element.dataset.i18nLabel));
    element.title = t(element.dataset.i18nLabel);
  }
  const license = $("#license-text");
  license.replaceChildren();
  for (const [heading, body] of t("license")) {
    const h = document.createElement("h3");
    h.textContent = heading;
    const p = document.createElement("p");
    p.textContent = body;
    license.append(h, p);
  }
  updateWidgetCount();
  // Text widths changed: move the segmented thumbs.
  requestAnimationFrame(() => $$(".segmented").forEach(placeThumb));
}

function resolvedTheme() {
  const theme = state.settings.Theme || "System";
  return theme === "System" ? state.systemTheme : theme.toLowerCase();
}

function applyTheme() {
  document.documentElement.dataset.theme = resolvedTheme();
  renderPreviews();
}

function fillLanguages() {
  const select = $("#language");
  select.replaceChildren();
  const languages = state.languages.length ? state.languages : Object.keys(LANGUAGE_NAMES);
  for (const code of languages) {
    const option = document.createElement("option");
    option.value = code;
    option.textContent = LANGUAGE_NAMES[code] || code;
    select.append(option);
  }
}

// ---------- Settings ----------

let pending = null;
let pendingTimer = 0;

/** Applies a change locally and sends it (sliders are coalesced to one message per 60 ms). */
function set(patch, { coalesce = false } = {}) {
  Object.assign(state.settings, patch);
  if (coalesce) {
    pending = Object.assign(pending || {}, patch);
    if (!pendingTimer) {
      pendingTimer = setTimeout(() => {
        post({ type: "set", patch: pending });
        pending = null;
        pendingTimer = 0;
      }, 60);
    }
  } else {
    post({ type: "set", patch });
  }
  syncControls();
}

function syncControls() {
  const s = state.settings;
  $("#language").value = s.Language || "en-US";
  for (const group of $$("[role=radiogroup][data-setting]")) {
    for (const option of $$("[role=radio]", group)) {
      const checked = option.dataset.value === String(s[group.dataset.setting]);
      option.setAttribute("aria-checked", checked);
      option.tabIndex = checked ? 0 : -1;
    }
    if (group.classList.contains("segmented")) placeThumb(group);
  }
  for (const toggle of $$("input.switch[data-setting]")) toggle.checked = !!s[toggle.dataset.setting];
  $("#start-with-windows").checked = state.startWithWindows;

  const size = s.Size || "Compact";
  const radius = s.DockCornerRadius ?? DEFAULT_RADIUS[size];
  setRange($("#radius"), radius, `${radius} px`, $("#radius-out"));
  const opacity = Math.round((s.DockOpacity ?? 0.9) * 100);
  const solid = s.DockBackground === "Solid";
  setRange($("#opacity"), solid ? 100 : opacity, solid ? "100%" : `${opacity}%`, $("#opacity-out"));
  $("#opacity").disabled = solid;
  $("#opacity").closest(".field").style.opacity = solid ? ".45" : "";
  renderPreviews();
}

function setRange(input, value, label, output) {
  if (document.activeElement !== input || input.value === "") input.value = value;
  const min = Number(input.min), max = Number(input.max);
  input.style.setProperty("--p", `${((input.value - min) / (max - min)) * 100}%`);
  output.textContent = label;
}

function placeThumb(group) {
  let thumb = $(".thumb", group);
  if (!thumb) {
    thumb = document.createElement("span");
    thumb.className = "thumb";
    group.prepend(thumb);
  }
  const checked = $("[aria-checked=true]", group);
  thumb.style.opacity = checked ? 1 : 0;
  if (checked) {
    thumb.style.left = `${checked.offsetLeft}px`;
    thumb.style.width = `${checked.offsetWidth}px`;
  }
}

// ---------- Live preview (look and buttons screens) ----------

function renderPreviews() {
  for (const preview of $$(".preview")) renderPreview(preview);
}

function renderPreview(preview) {
  const s = state.settings;
  const tone = resolvedTheme();
  const size = s.Size || "Compact";
  const icon = PREVIEW_ICON[size];
  const factor = icon / REAL_ICON[size];
  preview.dataset.tone = tone;

  let dock = $(".pv-dock", preview);
  if (!dock) {
    dock = document.createElement("div");
    dock.className = "pv-dock";
    preview.append(dock);
  }
  dock.dataset.edge = s.Edge || "Bottom";
  const thickness = icon + 14;
  const radius = Math.min((s.DockCornerRadius ?? DEFAULT_RADIUS[size]) * factor, thickness / 2);
  const solid = s.DockBackground === "Solid";
  const alpha = solid ? 1 : (s.DockOpacity ?? 0.9);
  const dark = tone === "dark";
  dock.style.borderRadius = `${radius}px`;
  dock.style.background = dark
    ? `linear-gradient(180deg, rgba(43,49,62,${alpha}), rgba(27,32,41,${alpha}))`
    : `linear-gradient(180deg, rgba(255,255,255,${alpha}), rgba(243,243,243,${alpha}))`;
  dock.style.borderColor = dark ? "rgba(255,255,255,.16)" : "rgba(0,0,0,.08)";
  dock.style.boxShadow = dark ? "0 6px 18px rgba(0,0,0,.4)" : "0 6px 18px rgba(30,40,80,.2)";
  dock.style.color = dark ? "#f1f5f9" : "#0f172a";
  dock.style.setProperty("--s", `${icon}px`);

  const items = [["start", "glyph"]];
  if (s.ShowSearchButton) items.push(["search", "glyph"]);
  if (s.ShowTaskViewButton) items.push(["taskview", "glyph"]);
  APP_COLORS.forEach((_, i) => items.push([`app${i}`, "app"]));
  if (s.ShowDesktopButton || s.ShowRecycleBin) items.push(["sep", "sep"]);
  if (s.ShowDesktopButton) items.push(["desktop", "glyph"]);
  if (s.ShowRecycleBin) items.push(["recycle", "glyph"]);

  const previous = state.previewKeys.get(preview) || null;
  const keys = items.map(([key]) => key);
  dock.replaceChildren(
    ...items.map(([key, kind]) => {
      if (kind === "sep") {
        const separator = document.createElement("span");
        separator.className = "pv-sep";
        return separator;
      }
      const item = document.createElement("span");
      item.className = `pv-item ${kind}`;
      if (kind === "app") {
        const [c1, c2] = APP_COLORS[Number(key.slice(3))];
        item.style.setProperty("--c1", c1);
        item.style.setProperty("--c2", c2);
      } else {
        item.innerHTML = GLYPHS[key];
      }
      item.style.borderRadius = `${Math.max(4, radius * 0.45)}px`;
      if (previous && !previous.includes(key)) item.classList.add("pop");
      return item;
    })
  );
  state.previewKeys.set(preview, keys);
}

// ---------- Widgets ----------

function selectedWidgets() {
  return Array.isArray(state.settings.Widgets) ? state.settings.Widgets : [];
}

function renderWidgets() {
  const grid = $("#widget-grid");
  const selected = new Set(selectedWidgets());
  grid.replaceChildren(
    ...state.widgets.map((widget, i) => {
      const card = document.createElement("button");
      card.className = "widget";
      card.style.setProperty("--d", i);
      card.setAttribute("aria-pressed", selected.has(widget.id));
      const name = document.createElement("b");
      name.textContent = widget.name;
      const description = document.createElement("small");
      description.textContent = widget.description;
      card.append(name, description);
      card.addEventListener("click", () => toggleWidget(widget.id, card));
      return card;
    })
  );
  updateWidgetCount();
}

function toggleWidget(id, card) {
  const list = selectedWidgets().filter((w) => w !== id);
  const on = card.getAttribute("aria-pressed") !== "true";
  if (on) list.push(id);
  card.setAttribute("aria-pressed", on);
  const installed = new Set(Array.isArray(state.settings.InstalledWidgets) ? state.settings.InstalledWidgets : []);
  if (on) installed.add(id);
  set({ Widgets: list, InstalledWidgets: [...installed] });
  updateWidgetCount();
}

function updateWidgetCount() {
  const count = selectedWidgets().length;
  $("#widget-count").textContent = count ? t("widgetsSelected").replace("{n}", count) : "";
}

// ---------- Navigation ----------

function canGoNext() {
  switch (SCREENS[state.index]) {
    case "license": return state.accepted;
    case "install": return state.installed;
    default: return true;
  }
}

function canGoBack() {
  const screen = SCREENS[state.index];
  if (state.index === 0 || screen === "done") return false;
  if (screen === "install" && (state.installed || state.installing)) return false;
  return true;
}

function updateNav() {
  const screen = SCREENS[state.index];
  const nav = $("#nav");
  nav.classList.toggle("hidden", screen === "welcome" || screen === "done");
  $("#btn-next").disabled = !canGoNext() || state.installing;
  $("#btn-back").disabled = !canGoBack();
  $("#btn-back").style.visibility = state.index <= 1 ? "hidden" : "";
  $("#btn-skip").hidden = !CUSTOMIZE.includes(screen);

  const steps = $("#steps");
  if (steps.childElementCount !== SCREENS.length - 1) {
    steps.replaceChildren(...SCREENS.slice(1).map(() => document.createElement("i")));
  }
  [...steps.children].forEach((dot, i) => {
    dot.className = i + 1 === state.index ? "current" : i + 1 < state.index ? "done" : "";
  });
  $("#progress").style.width = `${(state.index / (SCREENS.length - 1)) * 100}%`;
}

function show(index, direction) {
  const screens = $$(".screen");
  const current = screens.find((s) => s.classList.contains("active"));
  const target = $(`.screen[data-screen="${SCREENS[index]}"]`);
  state.index = index;
  if (current && current !== target) {
    current.classList.remove("active", "enter-forward", "enter-back");
    current.classList.add(`leave-${direction}`);
    setTimeout(() => current.classList.remove("leave-forward", "leave-back"), 340);
  }
  target.classList.remove("leave-forward", "leave-back", "enter-forward", "enter-back");
  void target.offsetWidth; // restart the entrance animations
  target.classList.add("active", `enter-${direction}`);
  updateNav();
  onEnter(SCREENS[index]);
}

function onEnter(screen) {
  if (screen === "look" || screen === "buttons") {
    renderPreviews();
    requestAnimationFrame(() => $$(".segmented").forEach(placeThumb));
  }
  if (screen === "done") burst();
  const focusTarget = $(`.screen[data-screen="${screen}"] .btn.primary`) || $("#btn-next");
  if (focusTarget && !focusTarget.disabled) setTimeout(() => focusTarget.focus({ preventScroll: true }), 400);
}

function next() {
  if (!canGoNext() || state.index >= SCREENS.length - 1) return;
  show(state.index + 1, "forward");
}

function back() {
  if (!canGoBack()) return;
  show(state.index - 1, "back");
}

function install() {
  if (state.installing || state.installed) return;
  state.installing = true;
  $("#install").dataset.state = "working";
  updateNav();
  post({ type: "install", startWithWindows: state.startWithWindows });
}

function finish() {
  post({ type: "finish" });
}

function burst() {
  const host = $("#burst");
  const colors = ["#4f8cff", "#ff6a88", "#2fd59b", "#ffc94a", "#b26bff", "#5ad1ff"];
  host.replaceChildren(
    ...Array.from({ length: 22 }, (_, i) => {
      const particle = document.createElement("i");
      const angle = (i / 22) * Math.PI * 2 + Math.random() * 0.3;
      const distance = 90 + Math.random() * 70;
      particle.style.setProperty("--x", `${Math.cos(angle) * distance}px`);
      particle.style.setProperty("--y", `${Math.sin(angle) * distance}px`);
      particle.style.setProperty("--r", `${Math.random() * 540 - 270}deg`);
      particle.style.setProperty("--c", colors[i % colors.length]);
      return particle;
    })
  );
}

// ---------- Events ----------

function wire() {
  $("#titlebar").addEventListener("mousedown", (event) => {
    if (event.button === 0 && !event.target.closest("button")) post({ type: "drag" });
  });
  $("#btn-min").addEventListener("click", () => post({ type: "minimize" }));
  $("#btn-close").addEventListener("click", () => post({ type: "close" }));
  $("#btn-next").addEventListener("click", next);
  $("#btn-back").addEventListener("click", back);
  $("#btn-skip").addEventListener("click", () => show(SCREENS.indexOf("done"), "forward"));
  $("#btn-install").addEventListener("click", install);
  $("#btn-import").addEventListener("click", () => post({ type: "import" }));
  for (const button of $$("[data-action=next]")) button.addEventListener("click", next);
  for (const button of $$("[data-action=finish]")) button.addEventListener("click", finish);

  $("#language").addEventListener("change", (event) => {
    set({ Language: event.target.value });
    applyTexts();
  });
  $("#accept").addEventListener("change", (event) => {
    state.accepted = event.target.checked;
    updateNav();
  });
  $("#start-with-windows").addEventListener("change", (event) => (state.startWithWindows = event.target.checked));

  for (const group of $$("[role=radiogroup][data-setting]")) {
    const options = $$("[role=radio]", group);
    options.forEach((option, i) => {
      option.addEventListener("click", () => {
        set({ [group.dataset.setting]: option.dataset.value });
        if (group.dataset.setting === "Theme") applyTheme();
      });
      option.addEventListener("keydown", (event) => {
        const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[event.key];
        if (!step) return;
        event.preventDefault();
        const target = options[(i + step + options.length) % options.length];
        target.click();
        target.focus();
      });
    });
  }
  for (const toggle of $$("input.switch[data-setting]")) {
    toggle.addEventListener("change", () => set({ [toggle.dataset.setting]: toggle.checked }));
  }
  $("#radius").addEventListener("input", (event) => set({ DockCornerRadius: Number(event.target.value) }, { coalesce: true }));
  $("#opacity").addEventListener("input", (event) => set({ DockOpacity: Number(event.target.value) / 100 }, { coalesce: true }));

  document.addEventListener("keydown", (event) => {
    if (event.altKey && event.key === "ArrowLeft") back();
    else if (event.altKey && event.key === "ArrowRight") next();
    else if (event.key === "Enter" && event.target === document.body) next();
  });
  window.addEventListener("resize", () => $$(".segmented").forEach(placeThumb));
}

// ---------- Demo (screenshot tests): walks through every screen by itself ----------

function runDemo() {
  const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const log = (text) => post({ type: "log", text });
  (async () => {
    await wait(2200); next();                         // license
    await wait(1200); $("#accept").click();
    await wait(1000); next();                         // about
    await wait(2200); next();                         // install
    await wait(1500); log("demo: install"); install(); // `installed` moves on
    while (SCREENS[state.index] !== "look") await wait(200);
    await wait(1500); $('[data-setting=Theme] [data-value=Light]').click();
    await wait(1500); $('[data-setting=Theme] [data-value=Dark]').click();
    await wait(1200);
    for (const value of [6, 12, 20, 28]) { set({ DockCornerRadius: value }); await wait(350); }
    await wait(800); set({ DockOpacity: 0.6 });
    await wait(1500); next();                         // buttons
    await wait(1200); $("[data-setting=ShowSearchButton]").click();
    await wait(1200); $("[data-setting=ShowSearchButton]").click();
    await wait(1200); next();                         // widgets
    await wait(1200); $$(".widget").slice(0, 3).forEach((card) => card.click());
    await wait(1800); next();                         // done
    await wait(3500); log("demo: finish"); finish();
  })();
}

// ---------- Browser mock (design work without Rust) ----------

function mock(message) {
  if (message.type === "install") setTimeout(() => window.ot.receive({ type: "installed", data: { ok: true } }), 1800);
  if (message.type === "set" && message.patch.Language) {
    // Widget names stay English in the mock.
  }
}

wire();
if (window.ipc) {
  post({ type: "ready" });
} else {
  window.ot.receive({
    type: "init",
    data: {
      settings: {
        Language: "tr-TR", Theme: "System", Size: "Compact", Edge: "Bottom", DockBackground: "Glass", DockOpacity: 0.9,
        DockCornerRadius: null, ShowSearchButton: true, ShowTaskViewButton: true, ShowDesktopButton: true,
        ShowRecycleBin: true, CheckUpdates: true, Widgets: [], InstalledWidgets: []
      },
      widgets: [
        { id: "Clock", name: "Clock", description: "Time and date." },
        { id: "Weather", name: "Weather", description: "Current weather for your city." },
        { id: "Media", name: "Media", description: "Now playing, with controls." },
        { id: "SystemStats", name: "System stats", description: "CPU, memory and GPU at a glance." },
        { id: "NetSpeed", name: "Network speed", description: "Upload and download speed." },
        { id: "Battery", name: "Battery", description: "Charge level and time left." }
      ],
      systemTheme: matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light",
      canImport: true,
      dryRun: true,
      demo: new URLSearchParams(location.search).has("demo")
    }
  });
}
