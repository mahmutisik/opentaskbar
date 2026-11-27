// OpenTaskbar's SettingsWindow pages, built from tables. The app sends `init` (settings, strings, displays, theme)
// and later `settings`, `theme` and `maximized`; the page sends `set` patches (PascalCase settings keys), `reset`,
// `hardware` and the window commands. Edits apply at once (`PreferenceEdited`); sliders save 250 ms after the last
// move (`_autoSaveTimer`), the dock scale when the mouse is released (`CommitDockScale`).
(() => {
  "use strict";

  const ICONS = {
    NavDock: "M5 3H19A2 2 0 0 1 21 5V19A2 2 0 0 1 19 21H5A2 2 0 0 1 3 19V5A2 2 0 0 1 5 3Z M3 16H21 M8 19H16",
    NavWidgets: "M4 3H9A1 1 0 0 1 10 4V9A1 1 0 0 1 9 10H4A1 1 0 0 1 3 9V4A1 1 0 0 1 4 3Z M15 3H20A1 1 0 0 1 21 4V9A1 1 0 0 1 20 10H15A1 1 0 0 1 14 9V4A1 1 0 0 1 15 3Z M4 14H9A1 1 0 0 1 10 15V20A1 1 0 0 1 9 21H4A1 1 0 0 1 3 20V15A1 1 0 0 1 4 14Z M15 14H20A1 1 0 0 1 21 15V20A1 1 0 0 1 20 21H15A1 1 0 0 1 14 20V15A1 1 0 0 1 15 14Z",
    NavThemes: "M12 3A9 9 0 1 0 12 21A9 9 0 1 0 12 3Z M12 3V21 M12 3C17 5 17 9 12 12C7 15 7 19 12 21",
    NavShortcuts: "M4.5 5H19.5A2 2 0 0 1 21.5 7V17A2 2 0 0 1 19.5 19H4.5A2 2 0 0 1 2.5 17V7A2 2 0 0 1 4.5 5Z M6 9H7 M10 9H11 M14 9H15 M18 9H19 M6 12H7 M10 12H11 M14 12H15 M18 12H19 M8 15H16",
    NavPreferences: "M12 3V6 M12 18V21 M3 12H6 M18 12H21 M5.6 5.6L7.7 7.7 M16.3 16.3L18.4 18.4 M18.4 5.6L16.3 7.7 M7.7 16.3L5.6 18.4 M12 8A4 4 0 1 0 12 16A4 4 0 1 0 12 8Z",
    NavAbout: "M12 3A9 9 0 1 0 12 21A9 9 0 1 0 12 3Z M12 11V16 M12 8H12.01",
    Rotate: "M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8 M21 3v5h-5 M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16 M8 16H3v5",
    Layers: "M12 2 2 7l10 5 10-5-10-5z M2 17l10 5 10-5 M2 12l10 5 10-5",
    Monitor: "M3 4h18a1 1 0 0 1 1 1v11a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1z M8 20h8 M12 16v4",
    EyeOff: "M9.88 9.88a3 3 0 1 0 4.24 4.24 M10.73 5.08A10.43 10.43 0 0 1 12 5c7 0 10 7 10 7a13.16 13.16 0 0 1-1.67 2.68 M6.61 6.61A13.53 13.53 0 0 0 2 12s3 7 10 7a9.74 9.74 0 0 0 5.39-1.61 M2 2l20 20",
    Download: "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4 M7 10l5 5 5-5 M12 15V3",
    Upload: "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4 M17 8l-5-5-5 5 M12 3v12",
    // Filled in the geometry (F1) but drawn by the button template with a stroke only, as in OpenTaskbar.
    GitHub: "M12 2C6.48 2 2 6.58 2 12.25c0 4.53 2.87 8.37 6.84 9.73.5.1.68-.22.68-.49v-1.9c-2.78.62-3.37-1.22-3.37-1.22-.45-1.18-1.11-1.49-1.11-1.49-.91-.64.07-.62.07-.62 1 .07 1.53 1.06 1.53 1.06.9 1.57 2.35 1.12 2.92.86.09-.66.35-1.12.63-1.38-2.22-.26-4.56-1.14-4.56-5.06 0-1.12.39-2.03 1.03-2.75-.1-.26-.45-1.3.1-2.71 0 0 .84-.28 2.75 1.05a9.36 9.36 0 0 1 5 0c1.91-1.33 2.75-1.05 2.75-1.05.55 1.41.2 2.45.1 2.71.64.72 1.03 1.63 1.03 2.75 0 3.93-2.34 4.8-4.57 5.05.36.32.68.94.68 1.9v2.82c0 .27.18.6.69.49A10.26 10.26 0 0 0 22 12.25C22 6.58 17.52 2 12 2z",
    FileText: "M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z M14 2v6h6 M8 13h8 M8 17h8 M8 9h2",
    Keyboard: "M4 6h16a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2z M6 10h.01 M10 10h.01 M14 10h.01 M18 10h.01 M8 14h8",
    Calendar: "M8 2v4 M16 2v4 M3 10h18 M5 4h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z",
    Mouse: "M12 2a7 7 0 0 0-7 7v6a7 7 0 0 0 14 0V9a7 7 0 0 0-7-7z M12 6v4",
    Sliders: "M4 21v-7 M4 10V3 M12 21v-9 M12 8V3 M20 21v-5 M20 12V3 M1 14h6 M9 8h6 M17 16h6",
    Plus: "M5 12h14 M12 5v14",
    Folder: "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2z",
    Refresh: "M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8 M21 3v5h-5 M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16 M8 16H3v5",
    Duplicate: "M9 9h11a1 1 0 0 1 1 1v11a1 1 0 0 1-1 1H9a1 1 0 0 1-1-1V10a1 1 0 0 1 1-1z M5 15H4a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1h11a1 1 0 0 1 1 1v1",
    Trash: "M3 6h18 M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6 M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2 M10 11v6 M14 11v6",
    Palette: "M12 3a9 9 0 1 0 0 18h1.5a2.5 2.5 0 0 0 0-5H13a2 2 0 0 1 0-4h5a3 3 0 0 0 3-3 6 6 0 0 0-9-6z M7.5 11h.01 M10.5 7.5h.01 M15 7.5h.01",
    Check: "M20 6L9 17l-5-5",
  };

  /// The widget catalog's icons (OpenTaskbar's geometries; "F0"/"F1" ones are filled logos).
  const WIDGET_ICONS = /*@ICONS*/;

  const state = {
    settings: {},
    strings: {},
    displays: [],
    hardware: false,
    startup: false,
    hotkeys: {},
    catalog: [],
    expanded: null,      // the widget whose drawer is open
    previewStamp: 0,
    recording: null,     // "dock" | "launcher" while a hotkey is recorded
    hotkeyError: "",
    languages: [],
    page: "dock",
    controls: [],   // { update(settings) } refreshed on `settings`
  };

  const post = (message) => window.ipc.postMessage(JSON.stringify(message));
  const t = (key, ...args) => {
    let s = state.strings[key] ?? key;
    args.forEach((a, i) => { s = s.split(`{${i}}`).join(String(a)); });
    return s;
  };
  const el = (tag, cls, text) => {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  };

  /// A stroke icon whose view box fits its path (`Stretch="Uniform"`), drawn `size` DIP square.
  function icon(name, size) {
    const ns = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(ns, "svg");
    svg.setAttribute("width", size);
    svg.setAttribute("height", size);
    const path = document.createElementNS(ns, "path");
    path.setAttribute("d", ICONS[name] || name);
    svg.appendChild(path);
    svg.dataset.fit = "1";
    return svg;
  }
  function fitIcons(root) {
    root.querySelectorAll('svg[data-fit="1"]').forEach((svg) => {
      const path = svg.querySelector("path");
      try {
        const b = path.getBBox();
        if (b.width > 0 || b.height > 0) {
          const side = Math.max(b.width, b.height);
          svg.setAttribute("viewBox", `${b.x - (side - b.width) / 2} ${b.y - (side - b.height) / 2} ${side} ${side}`);
          svg.dataset.fit = "2";
        }
      } catch (_) { /* not laid out yet */ }
    });
  }

  // ---------- Saving ----------
  let pending = {};
  let timer = 0;
  let previewTimer = 0;
  function set(patch, immediate) {
    Object.assign(state.settings, patch);
    Object.assign(pending, patch);
    clearTimeout(timer);
    if (immediate) flush();
    else timer = setTimeout(flush, 250);
    refresh();
    // `ApplySettingsToLivePreviews`: an open drawer's preview follows the edit once the dock has it.
    if (state.expanded) {
      clearTimeout(previewTimer);
      previewTimer = setTimeout(() => {
        const img = document.querySelector(".drawer-preview img");
        if (img) img.src = `http://ot.localhost/preview/${state.expanded}.png?${++state.previewStamp}`;
      }, immediate ? 150 : 450);
    }
  }
  function flush() {
    clearTimeout(timer);
    if (Object.keys(pending).length) post({ type: "set", patch: pending });
    pending = {};
  }

  // ---------- Controls ----------
  let openPopup = null;
  function closePopup() {
    if (openPopup) {
      openPopup.popup.remove();
      openPopup.host.classList.remove("open");
      openPopup = null;
    }
  }
  document.addEventListener("mousedown", (e) => {
    if (openPopup && !openPopup.popup.contains(e.target) && !openPopup.host.contains(e.target)) closePopup();
  }, true);
  window.addEventListener("blur", closePopup);

  /// A ComboBox: `options()` gives [{ value, label }], `value()` the current one.
  function select({ options, value, onChange, glyph, minWidth = 150, maxWidth = 230, width }) {
    const host = el("div", "select");
    host.appendChild(el("div", "surface"));
    const row = el("div", "value");
    if (glyph) {
      const g = icon(glyph, 14);
      g.classList.add("glyph");
      row.appendChild(g);
    }
    const badge = el("span", "badge");
    const flag = el("img");
    badge.appendChild(flag);
    const label = el("span");
    row.append(badge, label);
    host.appendChild(row);
    const arrow = el("div", "arrow");
    arrow.innerHTML = '<svg viewBox="0 0 10 9"><path d="M1 2.5 L5 6.5 L9 2.5"/></svg>';
    host.appendChild(arrow);
    if (width) host.style.width = width + "px";
    else { host.style.minWidth = minWidth + "px"; host.style.maxWidth = maxWidth + "px"; }
    const update = () => {
      const current = value();
      const match = options().find((o) => o.value === current) || options()[0];
      label.textContent = match ? match.label : "";
      badge.hidden = !(match && match.image);
      if (match && match.image && flag.src !== match.image) flag.src = match.image;
    };
    host.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      if (openPopup && openPopup.host === host) { closePopup(); return; }
      closePopup();
      const popup = el("div", "popup");
      const current = value();
      for (const o of options()) {
        const item = el("div", "option" + (o.value === current ? " selected" : ""));
        if (o.image) {
          const b = el("span", "badge");
          const img = el("img");
          img.src = o.image;
          b.appendChild(img);
          item.appendChild(b);
        }
        item.appendChild(el("span", "", o.label));
        item.insertAdjacentHTML("beforeend", '<svg viewBox="0 0 11 10"><path d="M0.9 5 L4.4 8.5 L10.1 1"/></svg>');
        item.addEventListener("mouseup", () => {
          closePopup();
          if (o.value !== value()) onChange(o.value);
        });
        popup.appendChild(item);
      }
      document.body.appendChild(popup);
      const r = host.getBoundingClientRect();
      popup.style.minWidth = r.width + "px";
      // A WPF popup is its own window and may leave the Settings window; the page cannot, so it stays inside.
      popup.style.left = Math.max(4, Math.min(r.left, window.innerWidth - popup.offsetWidth - 4)) + "px";
      let top = r.bottom + 6;
      if (top + popup.offsetHeight > window.innerHeight - 4) top = Math.max(4, r.top - 6 - popup.offsetHeight);
      popup.style.top = top + "px";
      host.classList.add("open");
      openPopup = { popup, host };
      // The list opens on its selected item.
      const chosen = popup.querySelector(".option.selected");
      if (chosen) chosen.scrollIntoView({ block: "nearest" });
    });
    state.controls.push({ update });
    update();
    return host;
  }

  function toggle({ value, onChange }) {
    const host = el("div", "switch");
    host.setAttribute("role", "switch");
    const track = el("div", "track");
    track.appendChild(el("div", "thumb"));
    host.appendChild(track);
    const update = () => host.classList.toggle("on", !!value());
    host.addEventListener("click", () => onChange(!value()));
    state.controls.push({ update });
    update();
    return host;
  }

  /// A WPF Slider (`SettingsOpacitySlider`): the thumb follows a drag; a press on the rail pages by `large`
  /// toward the pointer and repeats while held (`DecreaseLarge` / `IncreaseLarge`); values snap to `step`.
  function slider({ min, max, step, large, value, onInput, onCommit, width = 174 }) {
    const host = el("div", "slider");
    if (width) host.style.width = width + "px";
    const widthOf = () => width || host.clientWidth || 174;
    const rail = el("div", "rail");
    const low = el("div", "low");
    const high = el("div", "high");
    rail.append(low, high);
    const thumb = el("div", "thumb");
    host.append(rail, thumb);
    const range = () => [typeof min === "function" ? min() : min, typeof max === "function" ? max() : max];
    const stepOf = () => (typeof step === "function" ? step() : step);
    let shown = null;
    const draw = (v) => {
      const [lo, hi] = range();
      const f = hi > lo ? (Math.min(hi, Math.max(lo, v)) - lo) / (hi - lo) : 0;
      const w = widthOf();
      const x = Math.round(f * (w - 14));
      thumb.style.left = x + "px";
      low.style.width = x + 7 + "px";
      high.style.width = Math.max(0, w - x - 7) + "px";
    };
    const snap = (v) => {
      const [lo, hi] = range();
      const s = stepOf();
      return Math.min(hi, Math.max(lo, Math.round((v - lo) / s) * s + lo));
    };
    const update = () => { if (shown === null) draw(value()); };
    host.addEventListener("pointerdown", (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      host.setPointerCapture(e.pointerId);
      const r = host.getBoundingClientRect();
      const left = r.left + parseFloat(thumb.style.left || "0");
      if (e.clientX >= left && e.clientX < left + 14) {
        const startX = e.clientX;
        const startValue = value();
        const [lo, hi] = range();
        const move = (m) => {
          const v = snap(startValue + ((m.clientX - startX) / (widthOf() - 14)) * (hi - lo));
          if (v !== shown) { shown = v; draw(v); onInput(v); }
        };
        const up = () => {
          host.removeEventListener("pointermove", move);
          host.removeEventListener("pointerup", up);
          const v = shown;
          shown = null;
          if (onCommit && v !== null) onCommit(v);
        };
        host.addEventListener("pointermove", move);
        host.addEventListener("pointerup", up);
        return;
      }
      // Page toward the pointer, then repeat (WPF RepeatButton: 500 ms delay, 33 ms interval).
      const towards = e.clientX < left ? -1 : 1;
      let repeat = 0;
      const page = () => {
        const cur = shown ?? value();
        const r2 = host.getBoundingClientRect();
        const thumbX = r2.left + parseFloat(thumb.style.left || "0") + 7;
        if ((towards < 0 && e.clientX >= thumbX - 7) || (towards > 0 && e.clientX <= thumbX + 7)) return;
        const v = snap(cur + towards * large);
        if (v !== cur) { shown = v; draw(v); onInput(v); }
      };
      page();
      repeat = setTimeout(function again() { page(); repeat = setTimeout(again, 33); }, 500);
      const stop = () => {
        clearTimeout(repeat);
        host.removeEventListener("pointerup", stop);
        const v = shown;
        shown = null;
        if (onCommit && v !== null) onCommit(v);
      };
      host.addEventListener("pointerup", stop);
    });
    state.controls.push({ update });
    update();
    return host;
  }

  /// `SettingsButton`: a 14 DIP glyph 8 DIP before the label (`IconGlyph` defaults), or as given.
  function button(labelKey, glyph, onClick, { size = 14, gap = 8 } = {}) {
    const b = el("button", "settings-button");
    if (glyph) {
      const g = icon(glyph, size);
      g.style.marginRight = gap + "px";
      b.appendChild(g);
    }
    b.appendChild(el("span", "", t(labelKey)));
    b.addEventListener("click", onClick);
    return b;
  }

  function buttons(...list) {
    const host = el("div", "buttons");
    list.forEach((b) => host.appendChild(b));
    return host;
  }

  function valueText(width, text, margin = 8) {
    const v = el("div", "slider-value");
    v.style.width = width + "px";
    v.style.marginLeft = margin + "px";
    const update = () => { v.textContent = text(); };
    state.controls.push({ update });
    update();
    return v;
  }

  /// A settings row: label, optional description, the control on the right. `visible` / `enabled` follow edits.
  function row({ label, description, control, visible, enabled }) {
    const r = el("div", "row");
    const labels = el("div", "labels");
    const l = el("div", "row-label");
    const d = description ? el("div", "row-description") : null;
    labels.appendChild(l);
    if (d) labels.appendChild(d);
    const c = el("div", "control");
    for (const part of [].concat(control)) c.appendChild(part);
    r.append(labels, c);
    const update = () => {
      l.textContent = typeof label === "function" ? label() : t(label);
      if (d) d.textContent = typeof description === "function" ? description() : t(description);
      if (visible) r.hidden = !visible();
      if (enabled) r.classList.toggle("disabled", !enabled());
    };
    state.controls.push({ update });
    update();
    return r;
  }

  function group(rows) {
    const g = el("div", "group");
    rows.forEach((r) => g.appendChild(r));
    return g;
  }

  function heading(key, first) {
    return el("div", "section-heading" + (first ? " first" : ""), t(key));
  }

  function pageHeader(titleKey, subtitleKey, action) {
    const h = el("div", "page-header");
    const titles = el("div", "titles");
    titles.append(el("div", "page-title", t(titleKey)), el("div", "page-subtitle", t(subtitleKey)));
    h.appendChild(titles);
    if (action) h.appendChild(action);
    return h;
  }

  const s = () => state.settings;
  const vertical = () => s().Edge === "Left" || s().Edge === "Right";
  const choice = (key, items) => items.map(([value, label]) => ({ value, label: t(label) }));
  const settingSelect = (key, items, glyph, extra = {}) =>
    select({ options: () => choice(key, items), value: () => s()[key], onChange: (v) => set({ [key]: v }, true), glyph, ...extra });
  const settingToggle = (key) => toggle({ value: () => s()[key], onChange: (v) => set({ [key]: v }, true) });

  // ---------- Dock page ----------
  function dockPage() {
    const page = el("div", "page");
    const reset = el("button", "ghost");
    reset.append(icon("Rotate", 14), el("span", "", t("ResetDefaults")));
    reset.addEventListener("click", () => {
      flush();
      post({
        type: "reset",
        keys: ["Edge", "Position", "Layout", "ContentAlignment", "Size", "DockIconPadding", "DockScale", "DockWidthMode",
          "DockHeightMode", "DockTopGapUnit", "DockTopGapPixels", "DockTopGapPercent", "DockBottomGapUnit",
          "DockBottomGapPixels", "DockBottomGapPercent", "AutoHide", "AnimationsEnabled", "MagnificationEnabled",
          "PreviewsEnabled", "ShowRecycleBin", "CurrentDesktopOnly", "ShowSearchButton", "ShowTaskViewButton",
          "ShowTrayButton", "ShowDesktopButton", "TargetDisplay", "ShowOnAllMonitors", "SecondaryApps",
          "SecondaryWidgets", "AntigravityGroups"],
      });
    });
    // Rust: reset everything and run the setup wizard again (OpenTaskbar has no wizard).
    const setup = el("button", "ghost", t("ResetAndRunSetup"));
    setup.addEventListener("click", () => { flush(); post({ type: "resetSetup" }); });
    const actions = el("div", "header-actions");
    actions.append(setup, reset);
    page.appendChild(pageHeader("Dock", "SettingsPageDock", actions));

    // Layout
    page.appendChild(heading("SectionDockLayout", true));
    const alignment = select({
      options: () => choice("", [["Left", "Left"], ["Center", "Center"], ["Right", "Right"], ["FullWidth", "LayoutFullWidth"]]),
      value: () => (s().Layout === "FullWidth" ? "FullWidth" : s().Position),
      // Full width keeps the last floating alignment so switching back restores it.
      onChange: (v) => set(v === "FullWidth" ? { Layout: "FullWidth" } : { Layout: "Floating", Position: v }, true),
      glyph: "Monitor", maxWidth: 280,
    });
    page.appendChild(group([
      row({ label: "DockEdge", description: "DockEdgeDescription",
        control: settingSelect("Edge", [["Bottom", "EdgeBottom"], ["Left", "EdgeLeft"], ["Right", "EdgeRight"]], "Layers") }),
      row({ label: "DockAlignment", description: "DockAlignmentDescription", control: alignment }),
      row({ label: "ContentAlignment", description: "ContentAlignmentDescription",
        control: settingSelect("ContentAlignment", [["Left", "Left"], ["Center", "Center"], ["Right", "Right"]], "Monitor", { maxWidth: 280 }) }),
      row({ label: "DockSize", description: "DockSizeDescription",
        control: settingSelect("Size", [["Compact", "SizeCompact"], ["Medium", "SizeMedium"], ["Large", "SizeLarge"]], "Sliders") }),
      row({ label: "DockIconPadding", description: "DockIconPaddingHelp", control: [
        slider({ min: 0, max: 12, step: 1, large: 2, value: () => s().DockIconPadding,
          onInput: (v) => set({ DockIconPadding: v }) }),
        valueText(48, () => `${s().DockIconPadding} px`),
      ] }),
    ]));

    // Dimensions
    page.appendChild(heading("Dimensions"));
    const widthPercent = () => s().DockWidthMode === "Percent";
    const heightPercent = () => s().DockHeightMode === "Percent";
    const gap = (prefix) => {
      const unit = () => s()[`Dock${prefix}GapUnit`];
      const key = () => `Dock${prefix}Gap${unit() === "Percent" ? "Percent" : "Pixels"}`;
      return [
        select({ options: () => [{ value: "Pixels", label: "px" }, { value: "Percent", label: "%" }],
          value: unit, onChange: (v) => set({ [`Dock${prefix}GapUnit`]: v }, true), width: 58 }),
        (() => {
          const sl = slider({ min: 0, max: () => (unit() === "Percent" ? 100 : 200), step: 1, large: 1, width: 104,
            value: () => s()[key()], onInput: (v) => set({ [key()]: v }) });
          sl.style.marginLeft = "6px";
          return sl;
        })(),
        valueText(40, () => `${s()[key()]}${unit() === "Percent" ? "%" : "px"}`, 6),
      ];
    };
    page.appendChild(group([
      row({ label: () => t(vertical() ? "DockHeightMode" : "DockWidthMode"),
        description: () => t(vertical() ? "DockHeightModeDescription" : "DockWidthModeDescription"),
        control: settingSelect("DockWidthMode",
          [["Auto", "DockWidthAuto"], ["Pixels", "DockWidthPixels"], ["Percent", "DockWidthPercent"], ["Fill", "DockWidthFill"]], "Monitor") }),
      row({ label: () => t(vertical() ? "DockHeightValue" : "DockWidthValue"),
        description: () => t(vertical() ? "DockHeightValueDescription" : "DockWidthValueDescription"),
        visible: () => s().DockWidthMode === "Pixels" || s().DockWidthMode === "Percent",
        control: [
          slider({ min: () => (widthPercent() ? 10 : 100), max: () => (widthPercent() ? 100 : vertical() ? 3000 : 5000),
            step: () => (widthPercent() ? 1 : 50), large: 1,
            value: () => (widthPercent() ? s().DockWidthPercent : s().DockWidthPixels),
            onInput: (v) => set(widthPercent() ? { DockWidthPercent: v } : { DockWidthPixels: v }) }),
          valueText(48, () => (widthPercent() ? `${s().DockWidthPercent}%` : `${s().DockWidthPixels}px`)),
        ] }),
      // Side docks take their thickness from the width setting; the height setting is horizontal-only.
      row({ label: "DockHeightMode", description: "DockHeightModeDescription", visible: () => !vertical(),
        control: settingSelect("DockHeightMode",
          [["Auto", "DockHeightAuto"], ["Pixels", "DockHeightPixels"], ["Percent", "DockHeightPercent"], ["Fill", "DockHeightFill"]]) }),
      row({ label: "DockHeightValue", description: "DockHeightValueDescription",
        visible: () => !vertical() && (s().DockHeightMode === "Pixels" || s().DockHeightMode === "Percent"),
        control: [
          slider({ min: () => (heightPercent() ? 2 : 24), max: () => (heightPercent() ? 25 : 400),
            step: () => (heightPercent() ? 1 : 4), large: 1,
            value: () => (heightPercent() ? s().DockHeightPercent : s().DockHeightPixels),
            onInput: (v) => set(heightPercent() ? { DockHeightPercent: v } : { DockHeightPixels: v }) }),
          valueText(48, () => (heightPercent() ? `${s().DockHeightPercent}%` : `${s().DockHeightPixels}px`)),
        ] }),
      row({ label: "DockScale", description: "DockScaleHelp", control: [
        // Applying the scale reserves the work area again: commit once the mouse is released.
        slider({ min: 75, max: 150, step: 5, large: 10, value: () => Math.round(s().DockScale * 100),
          onInput: (v) => { state.settings.DockScale = v / 100; refresh(); },
          onCommit: (v) => set({ DockScale: v / 100 }, true) }),
        valueText(48, () => t("OpacityPercent", Math.round(s().DockScale * 100))),
      ] }),
      row({ label: "DockTopGap", description: "DockTopGapDescription", control: gap("Top") }),
      row({ label: "DockBottomGap", description: "DockBottomGapDescription", control: gap("Bottom") }),
    ]));

    // Behavior
    page.appendChild(heading("SectionBehavior"));
    const hardware = toggle({ value: () => state.hardware,
      onChange: (v) => { state.hardware = v; post({ type: "hardware", value: v }); refresh(); } });
    page.appendChild(group([
      row({ label: "AutoHide",
        control: settingSelect("AutoHide", [["Disabled", "AutoHideDisabled"], ["Smart", "AutoHideSmart"], ["Always", "AutoHideAlways"]], "EyeOff", { maxWidth: 250 }) }),
      row({ label: "Animations", description: "AnimationsDescription", control: settingToggle("AnimationsEnabled") }),
      row({ label: "Magnification", description: "MagnificationDescription", control: settingToggle("MagnificationEnabled") }),
      row({ label: "Previews", description: "PreviewsDescription", control: settingToggle("PreviewsEnabled") }),
      row({ label: "HardwareAcceleration", description: "HardwareAccelerationDescription", control: hardware }),
      row({ label: "ShowRecycleBin", description: "ShowRecycleBinDescription", control: settingToggle("ShowRecycleBin") }),
      row({ label: "CurrentDesktopOnly", description: "CurrentDesktopOnlyDescription", control: settingToggle("CurrentDesktopOnly") }),
      row({ label: "WidgetLauncher", description: "WidgetLauncherDescription", control: settingToggle("ShowSearchButton") }),
      row({ label: "WidgetDesktops", description: "TaskViewDescription", control: settingToggle("ShowTaskViewButton") }),
      row({ label: "TrayApps", description: "TrayButtonDescription", control: settingToggle("ShowTrayButton") }),
      row({ label: "ShowDesktop", description: "ShowDesktopDescription", control: settingToggle("ShowDesktopButton") }),
    ]));

    // Displays
    page.appendChild(heading("SectionDisplays"));
    const allMonitors = () => !!s().ShowOnAllMonitors;
    page.appendChild(group([
      row({ label: "TargetDisplay", description: "TargetDisplayDescription",
        control: select({ options: () => state.displays.map((d) => ({ value: d.value, label: d.label })),
          value: () => s().TargetDisplay ?? null, onChange: (v) => set({ TargetDisplay: v }, true), glyph: "Monitor" }) }),
      row({ label: "AllMonitors", description: "AllMonitorsDescription", control: settingToggle("ShowOnAllMonitors") }),
      row({ label: "SecondaryApps", description: "SecondaryAppsDescription", enabled: allMonitors,
        control: settingSelect("SecondaryApps", [["CurrentMonitorOnly", "SecondaryAppsCurrent"], ["AllMonitors", "SecondaryAppsAll"]], "Layers") }),
      row({ label: "SecondaryWidgets", description: "SecondaryWidgetsDescription", enabled: allMonitors,
        control: settingToggle("SecondaryWidgets") }),
    ]));
    return page;
  }

  // ---------- Preferences page ----------
  function preferencesPage() {
    const page = el("div", "page");
    page.appendChild(pageHeader("Preferences", "SettingsPagePreferences"));
    page.appendChild(heading("SectionSystem", true));
    const startup = toggle({ value: () => state.startup,
      onChange: (v) => { state.startup = v; post({ type: "startup", value: v }); refresh(); } });
    page.appendChild(group([
      row({ label: "Startup", description: "StartupDescription", control: startup }),
      row({ label: "CheckUpdatesSetting", description: "CheckUpdatesSettingDescription", control: settingToggle("CheckUpdates") }),
      row({ label: "FocusMode", description: "FocusModeDescription", control: settingToggle("FocusModeEnabled") }),
      row({ label: "Language", description: "LanguageDescription",
        control: select({ options: () => state.languages, value: () => s().Language,
          onChange: (v) => set({ Language: v }, true) }) }),
    ]));
    page.appendChild(heading("Backup"));
    page.appendChild(group([
      row({ label: "Backup", description: "BackupDescription", control: buttons(
        button("ExportSettings", "Download", () => { flush(); post({ type: "export" }); }),
        button("ImportSettings", "Upload", () => { flush(); post({ type: "import" }); }),
      ) }),
      row({ label: "FullBackup", description: "FullBackupDescription", control: buttons(
        button("FullBackupExport", "Download", () => { flush(); post({ type: "fullExport" }); }),
        button("FullBackupImport", "Upload", () => { flush(); post({ type: "fullImport" }); }),
      ) }),
    ]));
    return page;
  }

  // ---------- About page ----------
  function aboutPage() {
    const page = el("div", "page");
    page.appendChild(pageHeader("AboutTitle", "SettingsPageAbout"));
    page.appendChild(heading("Application", true));
    const version = el("div", "row-label secondary", "v" + state.version);
    page.appendChild(group([
      row({ label: "Version", control: version }),
      row({ label: "SourceCode", control: button("GitHubRepository", "GitHub", () => post({ type: "openUrl" }), { size: 16, gap: 7 }) }),
    ]));
    page.appendChild(heading("Diagnostics"));
    page.appendChild(group([
      row({ label: "OpenLogFile", description: "OpenLogFileDescription",
        control: button("OpenLogFile", "FileText", () => post({ type: "openLog" })) }),
    ]));
    return page;
  }

  // ---------- Shortcuts page ----------
  function shortcutsPage() {
    const page = el("div", "page");
    page.appendChild(pageHeader("Shortcuts", "SettingsPageShortcuts"));
    page.appendChild(heading("SectionHotkeys", true));
    /// The capture button (`SettingsButton`, at least 150 wide, centered) and the ghost reset button.
    const hotkey = (target) => {
      const capture = el("button", "settings-button capture");
      capture.appendChild(icon("Keyboard", 14));
      const text = el("span");
      capture.appendChild(text);
      capture.addEventListener("click", () => {
        state.recording = state.recording === target ? null : target;
        state.hotkeyError = "";
        refresh();
      });
      const reset = el("button", "ghost icon-only");
      reset.appendChild(icon("Rotate", 14));
      reset.title = t("HotkeyReset");
      reset.addEventListener("click", () => {
        state.recording = null;
        post({ type: "hotkeyReset", target });
      });
      const update = () => {
        const recording = state.recording === target;
        text.textContent = recording ? t("HotkeyRecording") : (state.hotkeys[target] ?? t("HotkeyDisabled"));
        capture.classList.toggle("recording", recording);
        reset.disabled = !state.hotkeys[target + "Resettable"];
      };
      state.controls.push({ update });
      update();
      return buttons(capture, reset);
    };
    page.appendChild(group([
      row({ label: "DockHotkey", description: "DockHotkeyDescription", control: hotkey("dock") }),
      row({ label: "LauncherHotkey", description: "LauncherHotkeyDescription", control: hotkey("launcher") }),
    ]));
    page.appendChild(el("div", "hotkey-hint", t("HotkeyCaptureHint")));
    const error = el("div", "hotkey-error");
    state.controls.push({ update: () => { error.textContent = state.hotkeyError; error.hidden = !state.hotkeyError; } });
    page.appendChild(error);

    page.appendChild(heading("SectionInteractions"));
    page.appendChild(group([
      row({ label: "StartPlacement", description: "StartPlacementDescription",
        control: settingSelect("StartPlacement", [["Start", "PlacementStart"], ["End", "PlacementEnd"], ["Hidden", "PlacementHidden"]], "Mouse", { maxWidth: 250 }) }),
      row({ label: "WheelVolume", description: "WheelVolumeDescription", control: settingToggle("WheelVolumeEnabled") }),
    ]));
    return page;
  }

  /// `Window_PreviewKeyDown` while a hotkey is recorded: Escape cancels, Backspace or Delete alone turns the hotkey
  /// off, anything else goes to the app, which checks it like OpenTaskbar (`TryFromKeyEvent`).
  document.addEventListener("keydown", (e) => {
    if (!state.recording) return;
    e.preventDefault();
    e.stopPropagation();
    const target = state.recording;
    const plain = !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey;
    if (e.key === "Escape") {
      state.recording = null;
      state.hotkeyError = "";
      refresh();
      return;
    }
    if (plain && (e.key === "Backspace" || e.key === "Delete")) {
      state.recording = null;
      post({ type: "hotkeyDisable", target });
      refresh();
      return;
    }
    post({ type: "hotkey", target, vk: e.keyCode, ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey, win: e.metaKey });
  }, true);
  // `Settings_Deactivated`: leaving the window ends the recording.
  window.addEventListener("blur", () => {
    if (state.recording) { state.recording = null; state.hotkeyError = ""; refresh(); }
  });

  // ---------- Widgets page ----------
  const METERS = ["Cpu", "Memory", "Gpu", "GpuTemperature"];
  /// `SystemMeters` flags as the settings file writes them ("All", "Cpu, Memory", "None").
  function meters() {
    const v = String(s().StatsMeters ?? "All");
    if (v === "All") return new Set(METERS);
    if (v === "None" || v === "") return new Set();
    return new Set(v.split(",").map((x) => x.trim()));
  }
  function setMeter(name, on) {
    const m = meters();
    if (on) m.add(name); else m.delete(name);
    const list = METERS.filter((x) => m.has(x));
    set({ StatsMeters: list.length === METERS.length ? "All" : list.length ? list.join(", ") : "None" }, true);
  }

  /// A catalog icon drawn like `WidgetIcon`: a 24-unit stroke icon (pen 2, scaled with it), or a filled logo.
  function catalogIcon(key, size) {
    const ns = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(ns, "svg");
    svg.setAttribute("width", size);
    svg.setAttribute("height", size);
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.classList.add("catalog-icon");
    const path = document.createElementNS(ns, "path");
    let d = WIDGET_ICONS[key] || "";
    if (/^F[01]/.test(d)) {
      path.setAttribute("fill-rule", d[1] === "0" ? "evenodd" : "nonzero");
      d = d.slice(2);
      svg.classList.add("filled");
    }
    path.setAttribute("d", d);
    svg.appendChild(path);
    return svg;
  }

  const installed = (id) => (s().InstalledWidgets || []).some((w) => w.toLowerCase() === id.toLowerCase());
  /// `WidgetInstallation_Click`: installing also enables; both lists follow the catalog order (`ReadSelection`).
  function toggleInstall(id) {
    const on = !installed(id);
    const has = (list, w) => (list || []).some((x) => x.toLowerCase() === w.toLowerCase());
    const order = state.catalog.map((w) => w.id);
    const installedNow = order.filter((w) => (w === id ? on : has(s().InstalledWidgets, w)));
    const enabledNow = order.filter((w) => (w === id ? on : has(s().InstalledWidgets, w) && has(s().Widgets, w)));
    if (!on && state.expanded === id) state.expanded = null;
    set({ InstalledWidgets: installedNow, Widgets: enabledNow }, true);
    state.previewStamp++;
    rebuildWidgets();
  }

  const densityOptions = (withDefault) => choice("", [
    ...(withDefault ? [["Default", "WidgetDensityFollowGlobal"]] : []),
    ["Auto", "WidgetDensityAuto"], ["Compact", "WidgetDensityCompact"], ["Full", "WidgetDensityFull"],
  ]);

  /// A drawer's small row (`CompactSettingsRow`): label (12) and the control.
  function compactRow(labelKey, control, last) {
    const r = el("div", "compact-row" + (last ? " last" : ""));
    r.append(el("div", "compact-label", t(labelKey)), control);
    return r;
  }

  /// The settings beside a widget's preview (`AttachWidgetSettingsPanels`).
  function widgetSettings(w) {
    const box = el("div", "widget-settings");
    // Density first (`AttachDensityRow`).
    const density = el("div", "inline-row");
    density.append(el("div", "inline-label", t("WidgetDensity")), select({
      options: () => densityOptions(true),
      value: () => (s().WidgetDensityOverrides || {})[w.id] ?? "Default",
      onChange: (v) => {
        const map = { ...(s().WidgetDensityOverrides || {}) };
        if (v === "Default") delete map[w.id]; else map[w.id] = v;
        set({ WidgetDensityOverrides: Object.keys(map).length ? map : null }, true);
      },
      minWidth: 128, maxWidth: 400,
    }));
    box.appendChild(density);
    if (w.id === "Clock") {
      density.classList.add("spaced");
      box.append(
        compactRow("ClockFormatLabel", select({ options: () => choice("", [["24", "ClockFormat24Hour"], ["12", "ClockFormat12Hour"]]),
          value: () => (s().Clock24Hour ? "24" : "12"), onChange: (v) => set({ Clock24Hour: v === "24" }, true), width: 150 })),
        compactRow("ClockShowSeconds", settingToggle("ClockShowSeconds")),
        compactRow("ClockDateStyleLabel", settingSelect("DateStyle", [["DayAndShortDate", "DateStyleDayShort"],
          ["ShortDateOnly", "DateStyleShort"], ["NumericDate", "DateStyleNumeric"], ["Hidden", "DateStyleHidden"]], "Calendar",
          { minWidth: 130, maxWidth: 170 })),
      );
    } else if (w.id === "SystemStats") {
      density.classList.add("spaced");
      const meter = (name) => toggle({ value: () => meters().has(name), onChange: (v) => setMeter(name, v) });
      box.append(
        compactRow("Cpu", meter("Cpu")),
        compactRow("Memory", meter("Memory")),
        compactRow("Gpu", meter("Gpu")),
        compactRow("GpuTemperature", meter("GpuTemperature"), true),
        el("div", "drawer-note", t("StatsAvailabilityNote")),
      );
    } else if (w.id === "Focus") {
      density.classList.add("spaced");
      const sliderRow = (labelKey, key, min, max, large, text, last) => {
        const r = el("div", "compact-row stacked" + (last ? " last" : ""));
        const head = el("div", "stacked-head");
        const value = el("div", "stacked-value");
        state.controls.push({ update: () => { value.textContent = text(s()[key]); } });
        head.append(el("div", "compact-label", t(labelKey)), value);
        const sl = slider({ min, max, step: 1, large, value: () => s()[key], onInput: (v) => set({ [key]: v }), width: 0 });
        sl.classList.add("stretch");
        r.append(head, sl);
        return r;
      };
      box.append(
        sliderRow("FocusDuration", "FocusMinutes", 1, 90, 5, (v) => t("MinutesFormat", v)),
        sliderRow("BreakDuration", "FocusBreakMinutes", 1, 30, 5, (v) => t("MinutesFormat", v)),
        sliderRow("FocusSessionCount", "FocusSessions", 1, 8, 2, (v) => String(v), true),
      );
    } else if (w.id.startsWith("AiUsage")) {
      // `AttachAiRingRow` (the account section comes with the AI widgets' later step).
      const ring = el("div", "inline-row");
      ring.append(el("div", "inline-label", t("AiUsageRingLabel")), settingSelect("AiUsageRing",
        [["Highest", "AiUsageRingHighest"], ["Session", "AiUsageRingSession"], ["Weekly", "AiUsageRingWeekly"]], null,
        { minWidth: 128, maxWidth: 400 }));
      density.classList.add("spaced");
      box.appendChild(ring);
    }
    return box;
  }

  function widgetCard(w) {
    const card = el("div", "widget-card");
    const head = el("div", "widget-head");
    const open = el("button", "widget-open");
    const ic = catalogIcon(w.icon, 16);
    const name = el("span", "widget-name", t(w.label));
    const unavailable = el("span", "widget-unavailable", t("WidgetUnavailable"));
    const chevron = el("span", "widget-chevron");
    open.append(ic, name, unavailable, chevron);
    open.addEventListener("click", () => {
      if (!w.available) return;
      state.expanded = state.expanded === w.id ? null : w.id;
      state.previewStamp++;
      rebuildWidgets();
    });
    const install = el("button", "widget-install");
    install.addEventListener("click", () => { if (w.available) toggleInstall(w.id); });
    head.append(open, install);
    card.appendChild(head);
    const expanded = state.expanded === w.id;
    const update = () => {
      const on = installed(w.id);
      card.classList.toggle("expanded", expanded);
      card.classList.toggle("installed", on);
      // The hardware note only for ported widgets (a desktop without a battery).
      unavailable.hidden = w.available || !w.ported;
      card.classList.toggle("unported", !w.ported);
      chevron.textContent = expanded ? "\uE70E" : "\uE70D";
      install.textContent = on ? "\uE73E" : "\uE710";
      install.title = t(on ? "WidgetRemove" : "WidgetInstall");
      install.disabled = !w.available;
    };
    state.controls.push({ update });
    if (expanded) {
      const drawer = el("div", "widget-drawer");
      const top = el("div", "drawer-top");
      const close = el("button", "ghost drawer-close", t("Close"));
      close.addEventListener("click", () => { state.expanded = null; rebuildWidgets(); });
      top.append(el("div", "drawer-title", t(w.label)), close);
      const grid = el("div", "drawer-grid");
      const previewHost = el("div", "drawer-preview");
      if (w.ported) {
        const img = el("img");
        img.src = `http://ot.localhost/preview/${w.id}.png?${state.previewStamp}`;
        img.addEventListener("load", () => { img.style.width = img.naturalWidth / devicePixelRatio + "px"; });
        img.addEventListener("error", () => img.remove());
        previewHost.appendChild(img);
      }
      const right = el("div", "drawer-right");
      right.appendChild(el("div", "drawer-heading", t("WidgetSettings")));
      const show = el("div", "drawer-show");
      show.append(el("div", "compact-label", t("WidgetShow")),
        toggle({ value: () => installed(w.id), onChange: () => toggleInstall(w.id) }));
      right.append(show, widgetSettings(w));
      grid.append(previewHost, right);
      drawer.append(top, grid);
      card.appendChild(drawer);
    }
    update();
    return card;
  }

  let widgetsHost = null;
  function rebuildWidgets() {
    if (!widgetsHost) return;
    widgetsHost.innerHTML = "";
    const groups = [];
    for (const w of state.catalog) {
      let g = groups.find((x) => x.key === w.category);
      if (!g) groups.push((g = { key: w.category, items: [] }));
      g.items.push(w);
    }
    for (const g of groups) {
      widgetsHost.appendChild(el("div", "widget-group", t(g.key)));
      const list = el("div", "widget-list");
      g.items.forEach((w) => list.appendChild(widgetCard(w)));
      widgetsHost.appendChild(list);
    }
    refresh();
  }

  function widgetsPage() {
    const page = el("div", "page");
    const header = pageHeader("Widgets", "WidgetsDescription");
    header.style.marginBottom = "22px";
    page.appendChild(header);
    widgetsHost = el("div", "widget-catalog");
    page.appendChild(widgetsHost);
    page.appendChild(heading("WidgetDensity"));
    page.appendChild(group([
      row({ label: "WidgetDensity", control: select({ options: () => densityOptions(false),
        value: () => s().GlobalWidgetDensity, onChange: (v) => set({ GlobalWidgetDensity: v }, true), glyph: "Sliders" }) }),
    ]));
    rebuildWidgets();
    return page;
  }

  // ---------- Themes ----------
  const themes = () => state.themes || { builtIn: [], custom: [], swatches: [] };

  /// `TextPrompt.Ask`: a small modal with a text box; resolves to the text, or null when cancelled.
  function promptText(title, initial) {
    return new Promise((resolve) => {
      closePopup();
      const shade = el("div", "prompt-shade");
      const box = el("div", "prompt");
      box.appendChild(el("div", "prompt-title", title));
      const input = el("input", "prompt-input");
      input.value = initial || "";
      box.appendChild(input);
      const actions = el("div", "prompt-actions");
      const ok = el("button", "settings-button primary", t("Ok"));
      const cancel = el("button", "settings-button", t("Cancel"));
      actions.append(ok, cancel);
      box.appendChild(actions);
      shade.appendChild(box);
      document.body.appendChild(shade);
      const done = (value) => { shade.remove(); resolve(value); };
      ok.addEventListener("click", () => done(input.value.trim() || null));
      cancel.addEventListener("click", () => done(null));
      shade.addEventListener("mousedown", (e) => { if (e.target === shade) done(null); });
      input.addEventListener("keydown", (e) => {
        if (e.key === "Enter") { e.preventDefault(); ok.click(); }
        if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); done(null); }
      });
      requestAnimationFrame(() => { input.focus(); input.select(); });
    });
  }

  // ---------- Brush picker (`ThemeBrushPicker`) ----------
  const argbOf = (hex) => {
    const h = String(hex || "").replace("#", "");
    const full = h.length === 6 ? "FF" + h : h;
    if (!/^[0-9a-fA-F]{8}$/.test(full)) return null;
    return [0, 2, 4, 6].map((i) => parseInt(full.slice(i, i + 2), 16));
  };
  const hexOf = (c) => "#" + c.map((v) => Math.round(v).toString(16).padStart(2, "0")).join("").toUpperCase();
  const cssOf = (c) => `rgba(${c[1]}, ${c[2]}, ${c[3]}, ${(c[0] / 255).toFixed(4)})`;
  function toHsv(c) {
    const [r, g, b] = [c[1] / 255, c[2] / 255, c[3] / 255];
    const max = Math.max(r, g, b), min = Math.min(r, g, b), d = max - min;
    let h = 0;
    if (d > 0) {
      if (max === r) h = 60 * ((((g - b) / d) % 6));
      else if (max === g) h = 60 * ((b - r) / d + 2);
      else h = 60 * ((r - g) / d + 4);
    }
    if (h < 0) h += 360;
    return [h, max <= 0 ? 0 : d / max, max];
  }
  function fromHsv(h, sat, v, a = 255) {
    h = ((h % 360) + 360) % 360; sat = Math.min(1, Math.max(0, sat)); v = Math.min(1, Math.max(0, v));
    const c = v * sat, x = c * (1 - Math.abs(((h / 60) % 2) - 1)), m = v - c;
    const [r, g, b] = h < 60 ? [c, x, 0] : h < 120 ? [x, c, 0] : h < 180 ? [0, c, x] : h < 240 ? [0, x, c] : h < 300 ? [x, 0, c] : [c, 0, x];
    return [a, Math.round((r + m) * 255), Math.round((g + m) * 255), Math.round((b + m) * 255)];
  }

  /// Resolves to "#AARRGGBB" (solid) or `{ stops, positions, angle }` (gradient), or null when cancelled.
  function openBrushPicker(initial) {
    return new Promise((resolve) => {
      closePopup();
      const saved = state.controls;
      state.controls = [];
      const isGradient = initial && typeof initial === "object" && Array.isArray(initial.stops);
      let solid = (!isGradient && argbOf(initial)) || argbOf("#FF708090");
      let stops = [];
      let angle = 90;
      if (isGradient) {
        stops = initial.stops.map((c, i) => ({ c: argbOf(c) || [255, 0, 0, 0],
          o: initial.positions ? initial.positions[i] / 100 : i / (initial.stops.length - 1) }));
        stops.sort((a, b) => a.o - b.o);
        angle = Math.round(initial.angle ?? 180);
      }
      if (stops.length < 2) stops = [{ c: solid.slice(), o: 0 }, { c: [255, 120, 145, 175], o: 1 }];
      let tab = isGradient ? 1 : 0;
      let sel = 0;
      let hue = 0, sat = 0, val = 0;
      const current = () => (tab === 1 ? stops[sel].c : solid);
      const setCurrent = (c) => { if (tab === 1) stops[sel].c = c; else solid = c; sync(); };
      const result = () => {
        if (tab !== 1) return hexOf(solid);
        const sorted = stops.slice().sort((a, b) => a.o - b.o);
        return { stops: sorted.map((x) => hexOf(x.c)), positions: sorted.map((x) => Math.round(x.o * 100000) / 1000), angle };
      };
      const previewCss = () => {
        if (tab !== 1) return cssOf(solid);
        const sorted = stops.slice().sort((a, b) => a.o - b.o);
        return `linear-gradient(${angle}deg, ${sorted.map((x) => `${cssOf(x.c)} ${(x.o * 100).toFixed(1)}%`).join(", ")})`;
      };

      const shade = el("div", "prompt-shade picker-shade");
      const win = el("div", "picker");
      const title = el("div", "picker-title");
      const mark = el("div", "picker-mark");
      title.append(mark, el("span", "", t("ColorTitle")));
      const close = el("button", "picker-close");
      close.innerHTML = "&#xE8BB;";
      title.appendChild(close);
      const body = el("div", "picker-body");
      const scroll = el("div", "picker-scroll");

      // Tabs: Solid / Gradient.
      const tabs = el("div", "picker-tabs");
      const tabButtons = ["SurfaceSolid", "SurfaceGradient"].map((key, i) => {
        const b = el("button", "picker-tab", t(key));
        b.addEventListener("click", () => { tab = i; sync(); });
        tabs.appendChild(b);
        return b;
      });
      const panel = el("div", "picker-panel");
      const solidPanel = el("div", "picker-pane");
      solidPanel.appendChild(el("div", "picker-text", t("ColorSelectSingle")));
      const gradPanel = el("div", "picker-pane");
      gradPanel.appendChild(el("div", "picker-text", t("ColorSelectStop")));
      const stopChoices = el("div", "picker-stops");
      const tools = el("div", "picker-tools");
      const add = el("button", "settings-button picker-button", t("ColorAddStop"));
      add.addEventListener("click", () => {
        if (stops.length >= 16) return;
        stops.push({ c: current().slice(), o: 0.5 });
        sel = stops.length - 1; sync();
      });
      const remove = el("button", "settings-button picker-button", t("ColorRemove"));
      remove.addEventListener("click", () => {
        if (stops.length <= 2) return;
        stops.splice(sel, 1); sel = 0; sync();
      });
      tools.append(add, remove);
      const positionRow = el("div", "picker-row grow");
      const position = slider({ min: 0, max: 100, step: 1, large: 1, width: 0, value: () => Math.round(stops[sel].o * 100),
        onInput: (v) => { stops[sel].o = v / 100; sync(); } });
      const positionValue = el("div", "picker-value");
      positionRow.append(position, positionValue);
      const angleRow = el("div", "picker-row angle");
      const angleValue = el("div", "picker-value");
      angleRow.append(el("div", "picker-label wide", t("ColorAngle")),
        slider({ min: 0, max: 360, step: 1, large: 1, width: 180, value: () => angle, onInput: (v) => { angle = v; sync(); } }),
        angleValue);
      gradPanel.append(stopChoices, tools, el("div", "picker-text position", t("ColorStopPosition")), positionRow, angleRow);
      panel.append(solidPanel, gradPanel);

      // Saturation/value field, hue rainbow and slider.
      const area = el("div", "picker-area");
      area.appendChild(el("div", "picker-heading", t("ColorTitle")));
      const field = el("div", "picker-field");
      const cursor = el("div", "picker-cursor");
      field.appendChild(cursor);
      const choose = (e) => {
        const r = field.getBoundingClientRect();
        sat = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
        val = Math.min(1, Math.max(0, 1 - (e.clientY - r.top) / r.height));
        setCurrent(fromHsv(hue, sat, val, current()[0]));
      };
      field.addEventListener("pointerdown", (e) => {
        field.setPointerCapture(e.pointerId); choose(e);
        const move = (m) => choose(m);
        const up = () => { field.removeEventListener("pointermove", move); field.removeEventListener("pointerup", up); };
        field.addEventListener("pointermove", move); field.addEventListener("pointerup", up);
      });
      const rainbow = el("div", "picker-rainbow");
      const hueSlider = slider({ min: 0, max: 360, step: 1, large: 1, width: 305, value: () => Math.round(hue),
        onInput: (v) => { hue = v; setCurrent(fromHsv(hue, sat, val, current()[0])); } });
      hueSlider.classList.add("picker-hue");
      area.append(field, rainbow, hueSlider);
      const preview = el("div", "picker-preview");

      const channels = ["ColorRed", "ColorGreen", "ColorBlue"].map((key, i) => {
        const r = el("div", "picker-row channel");
        r.append(el("div", "picker-label", t(key)), slider({ min: 0, max: 255, step: 1, large: 1, width: 230,
          value: () => current()[i + 1], onInput: (v) => { const c = current().slice(); c[i + 1] = v; setCurrent(c); } }));
        return r;
      });
      const alphaRow = el("div", "picker-row alpha");
      const alphaValue = el("div", "picker-value");
      alphaRow.append(el("div", "picker-label", t("ColorOpacity")), slider({ min: 0, max: 255, step: 1, large: 1, width: 200,
        value: () => current()[0], onInput: (v) => { const c = current().slice(); c[0] = v; setCurrent(c); } }), alphaValue);
      const hexRow = el("div", "picker-row hex");
      const hex = el("input", "picker-hex");
      const parseHex = () => {
        const text = hex.value.trim();
        const c = (text.length === 7 || text.length === 9) && text.startsWith("#") ? argbOf(text) : null;
        if (c) setCurrent(c); else sync();
      };
      hex.addEventListener("blur", parseHex);
      hex.addEventListener("keydown", (e) => {
        if (e.key === "Enter") parseHex();
        if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); done(null); }
      });
      hexRow.append(el("div", "picker-label", t("ColorHEX")), hex);

      scroll.append(tabs, panel, area, preview, ...channels, alphaRow, hexRow);
      const footer = el("div", "picker-footer");
      const cancel = el("button", "settings-button picker-button", t("ColorCancel"));
      const accept = el("button", "settings-button picker-button", t("ColorUse"));
      footer.append(cancel, accept);
      body.append(scroll, footer);
      win.append(title, body);
      shade.appendChild(win);
      document.body.appendChild(shade);
      const local = state.controls;
      state.controls = saved;

      function sync() {
        const c = current();
        tabButtons.forEach((b, i) => b.classList.toggle("selected", i === tab));
        solidPanel.hidden = tab !== 0;
        gradPanel.hidden = tab !== 1;
        stopChoices.innerHTML = "";
        stops.forEach((stop, i) => {
          const b = el("button", "settings-button picker-stop" + (i === sel ? " selected" : ""),
            `${t("ColorStopLabel", i + 1)} · ${hexOf(stop.c)} · ${Math.round(stop.o * 100)}%`);
          b.addEventListener("click", () => { sel = i; sync(); });
          stopChoices.appendChild(b);
        });
        positionValue.textContent = Math.round(stops[sel].o * 100) + "%";
        angleValue.textContent = angle + "\u00B0";
        alphaValue.textContent = Math.round((c[0] / 255) * 100) + "%";
        if (document.activeElement !== hex) hex.value = hexOf(c);
        const [h, sv, v] = toHsv(c);
        hue = sv === 0 || v === 0 ? hue : h;
        sat = sv; val = v;
        field.style.background = `linear-gradient(to bottom, rgba(0,0,0,0), #000), linear-gradient(to right, #fff, rgba(255,255,255,0)), ${cssOf(fromHsv(hue, 1, 1))}`;
        cursor.style.left = Math.round(sat * 305 - 6) + "px";
        cursor.style.top = Math.round((1 - val) * 126 - 6) + "px";
        preview.style.background = previewCss();
        local.forEach((x) => x.update());
      }
      function done(value) { shade.remove(); resolve(value); }
      cancel.addEventListener("click", () => done(null));
      close.addEventListener("click", () => done(null));
      accept.addEventListener("click", () => done(result()));
      shade.addEventListener("keydown", (e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); done(null); } });
      sync();
      requestAnimationFrame(() => win.focus());
      win.tabIndex = -1;
    });
  }

  /// "#AARRGGBB" mixed toward `tint` (`BlendColors`), opaque.
  function blend(base, tint, amount) {
    const p = (h) => { const x = h.replace("#", ""); const o = x.length === 8 ? 2 : 0;
      return [0, 2, 4].map((i) => parseInt(x.slice(o + i, o + i + 2), 16)); };
    const a = p(base), b = p(tint);
    return "#FF" + a.map((v, i) => Math.round(v * (1 - amount) + b[i] * amount).toString(16).padStart(2, "0")).join("");
  }
  const withAlpha = (hex, alpha) => css("#" + alpha + hex.replace("#", "").slice(-6));

  /// `CreateThemeEntryButton`: a miniature desktop with a dock pill, the name and the active badge or palette dots.
  function themeCard(choice) {
    const active = (themes().selected ?? null) === (choice.id ?? null)
      || (choice.id && themes().selected && choice.id.toLowerCase() === String(themes().selected).toLowerCase());
    const { bg, dock, accent, text } = choice.preview;
    const card = el("button", "theme-card" + (active ? " active" : ""));
    card.title = choice.name;
    const stage = el("div", "theme-stage");
    stage.style.background = `linear-gradient(135deg, ${css(blend(bg, accent, 0.14))} 0%, ${css(bg)} 65%, `
      + `${css(blend(bg, "#FF000000", state.dark ? 0.25 : 0.06))} 100%)`;
    const pill = el("div", "theme-pill");
    pill.style.background = withAlpha(dock, "EB");
    pill.style.borderColor = withAlpha(accent, "44");
    const tile = (alpha, color) => { const d = el("div", "theme-tile"); d.style.background = withAlpha(color, alpha); return d; };
    pill.appendChild(tile("CC", accent));
    const activeApp = el("div", "theme-active");
    activeApp.appendChild(tile("3D", text));
    const dot = el("div", "theme-dot");
    dot.style.background = css(accent);
    activeApp.appendChild(dot);
    pill.appendChild(activeApp);
    pill.append(tile("28", text), tile("28", text));
    const divider = el("div", "theme-divider");
    divider.style.background = withAlpha(text, "24");
    pill.appendChild(divider);
    const clock = el("div", "theme-clock");
    const bar1 = el("div", "theme-bar1"); bar1.style.background = withAlpha(text, "99");
    const bar2 = el("div", "theme-bar2"); bar2.style.background = withAlpha(text, "4D");
    clock.append(bar1, bar2);
    pill.appendChild(clock);
    stage.appendChild(pill);
    const footer = el("div", "theme-footer");
    footer.appendChild(el("span", "theme-name", choice.name));
    if (active) {
      const badge = el("div", "theme-badge");
      badge.appendChild(icon("Check", 10));
      footer.appendChild(badge);
    } else {
      const dots = el("div", "theme-dots");
      const a = el("div", "theme-swatch"); a.style.background = css(dock); a.style.borderColor = withAlpha(text, "40");
      const b = el("div", "theme-swatch accent"); b.style.background = css(accent);
      dots.append(a, b);
      footer.appendChild(dots);
    }
    card.append(stage, el("div", "theme-rule"), footer);
    card.addEventListener("click", () => {
      if (active) return;
      flush();
      post({ type: "themeSelect", id: choice.id ?? null });
    });
    return card;
  }

  /// `ThemePropertyEditor` for a custom theme: information fields and the token tabs.
  function themeEditor(ed, section) {
    const host = el("div", "editor-body");
    host.appendChild(section("ThemeInformation"));
    const info = { name: ed.name, author: ed.author, description: ed.description };
    let infoTimer = 0;
    const saveInfo = () => { clearTimeout(infoTimer); infoTimer = 0; post({ type: "themeInfo", ...info }); };
    const textRow = (labelKey, key) => {
      const r = el("div", "editor-row");
      r.appendChild(el("div", "editor-label", t(labelKey)));
      const input = el("input", "text-field");
      input.value = info[key] || "";
      input.dataset.focus = "info-" + key;
      input.addEventListener("input", () => {
        info[key] = input.value;
        clearTimeout(infoTimer);
        infoTimer = setTimeout(saveInfo, 350);
      });
      input.addEventListener("blur", () => { if (infoTimer) saveInfo(); });
      r.appendChild(input);
      return r;
    };
    host.append(textRow("ThemeFieldName", "name"), textRow("ThemeFieldAuthor", "author"), textRow("ThemeFieldDescription", "description"));

    const tabsHost = el("div", "editor-tabs");
    const header = el("div", "editor-tab-strip");
    const content = el("div", "editor-tab-content");
    tabsHost.append(header, content);
    const index = Math.min(state.editorTab || 0, ed.sections.length - 1);
    ed.sections.forEach((sec, i) => {
      const b = el("button", "editor-tab" + (i === index ? " selected" : ""), sec.title);
      b.addEventListener("click", () => { state.editorTab = i; rebuildThemes(); });
      header.appendChild(b);
    });
    for (const f of (ed.sections[index] || { fields: [] }).fields) content.appendChild(tokenRow(f));
    host.appendChild(tabsHost);
    return host;
  }

  const saveToken = (key, value) => post({ type: "themeToken", key, value });

  function tokenRow(f) {
    const r = el("div", "editor-row");
    r.appendChild(el("div", "editor-label", f.label));
    if (f.kind === "brush") {
      const b = el("button", "settings-button brush-field");
      const sw = el("div", "brush-swatch");
      sw.style.background = f.css;
      b.append(sw, el("span", "", t(f.gradient ? "SurfaceGradient" : "SurfaceSolid")));
      b.addEventListener("click", async () => {
        const chosen = await openBrushPicker(f.value);
        if (chosen) saveToken(f.key, chosen);
      });
      r.appendChild(b);
      return r;
    }
    const input = el("input", "text-field");
    input.value = f.text;
    input.dataset.focus = "token-" + f.key;
    if (f.kind === "numbers") input.title = t("ThemeFourValueFieldHelp");
    if (f.kind === "font") input.title = t("ThemeFontFieldHelp");
    const commit = () => {
      const text = input.value.trim();
      if (text === f.text) return;
      if (f.kind === "number") {
        const n = Number(text);
        if (text && Number.isFinite(n) && n >= 0 && n <= 2048) saveToken(f.key, n); else input.value = f.text;
      } else if (f.kind === "numbers") {
        const parts = text.split(",").map((x) => x.trim());
        const ok = (parts.length === 1 || parts.length === 4)
          && parts.every((x) => x !== "" && Number.isFinite(Number(x)) && Number(x) >= 0 && Number(x) <= 2048);
        // Always the full four-value form the theme pack expects.
        if (ok) saveToken(f.key, parts.length === 1 ? Array(4).fill(parts[0]).join(",") : parts.join(","));
        else input.value = f.text;
      } else if (text) {
        saveToken(f.key, text);
      } else {
        input.value = f.text;
      }
    };
    input.addEventListener("blur", commit);
    input.addEventListener("keydown", (e) => { if (e.key === "Enter") input.blur(); });
    r.appendChild(input);
    return r;
  }

  /// `AdaptiveGrid` (MinItemWidth 195): as many equal columns as fit.
  function gallery(list) {
    const grid = el("div", "theme-grid");
    for (const choice of list) grid.appendChild(themeCard(choice));
    const fit = () => {
      const w = grid.clientWidth;
      if (w > 0) grid.style.gridTemplateColumns = `repeat(${Math.max(1, Math.floor(w / 195))}, 1fr)`;
    };
    new ResizeObserver(fit).observe(grid);
    return grid;
  }

  let themesHost = null;
  let themesControls = [];
  function rebuildThemes() {
    if (!themesHost) return;
    const scroll = document.getElementById("content").scrollTop;
    state.controls = state.controls.filter((c) => !themesControls.includes(c));
    const old = themesHost;
    const focused = document.activeElement;
    const focusKey = focused && focused.dataset ? focused.dataset.focus : null;
    const typed = focusKey ? [focused.value, focused.selectionStart, focused.selectionEnd] : null;
    const fresh = themesPage();
    fresh.hidden = old.hidden;
    old.replaceWith(fresh);
    if (focusKey) {
      const again = fresh.querySelector(`[data-focus="${focusKey}"]`);
      if (again) { again.value = typed[0]; again.focus(); again.setSelectionRange(typed[1], typed[2]); }
    }
    const p = PAGES.find((x) => x.id === "themes");
    if (p) p.page = fresh;
    document.getElementById("content").scrollTop = scroll;
    requestAnimationFrame(() => fitIcons(fresh));
  }

  function surfaceRow(kind) {
    const bgKey = `${kind}Background`, opKey = `${kind}Opacity`;
    const box = el("div", "stacked-control");
    const sel = select({
      options: () => choice("", [["Glass", "SurfaceGlass"], ["Solid", "SurfaceSolid"], ["None", "SurfaceNone"]]),
      value: () => s()[bgKey], onChange: (v) => set({ [bgKey]: v }, true), glyph: "Layers" });
    const line = el("div", "surface-line");
    const sl = slider({ min: 0, max: 100, step: 5, large: 10, value: () => Math.round((s()[opKey] ?? 0.9) * 100),
      onInput: (v) => set({ [opKey]: v / 100 }) });
    const value = valueText(48, () => t("OpacityPercent", Math.round((s()[opKey] ?? 0.9) * 100)));
    line.append(sl, value);
    box.append(sel, line);
    const update = () => { line.classList.toggle("disabled", s()[bgKey] === "None"); };
    state.controls.push({ update });
    const labels = { Dock: "SurfaceDock", Menu: "SurfaceMenu", Widget: "SurfaceWidget", Settings: "SurfaceSettings" };
    return row({ label: labels[kind], description: labels[kind] + "Help", control: box });
  }

  function borderRow() {
    const box = el("div", "border-control");
    const keys = ["DockBorderThickness", "MenuBorderThickness", "WidgetBorderThickness", "SettingsBorderThickness"];
    const sl = slider({ min: 0, max: 6, step: 0.5, large: 1, width: 120,
      value: () => Math.max(0, Math.min(6, s().MenuBorderThickness ?? 1)),
      onInput: (v) => set(Object.fromEntries(keys.map((k) => [k, Math.round(v * 2) / 2]))) });
    const value = valueText(44, () => String(Math.round((s().MenuBorderThickness ?? 1) * 10) / 10), 6);
    const button = el("button", "border-color");
    button.title = t("BorderColorLabel");
    const swatch = el("div", "border-swatch");
    button.appendChild(swatch);
    button.addEventListener("click", async () => {
      flush();
      const chosen = await openBrushPicker(s().MenuBorderColor || themes().borderDefaultValue || "#70FFFFFF");
      if (!chosen) return;
      // `BrushToHex`: a gradient's first stop.
      post({ type: "borderColorSet", value: typeof chosen === "string" ? chosen : chosen.stops[0] });
    });
    const update = () => {
      const color = s().MenuBorderColor;
      swatch.style.background = color ? css(color) : (themes().borderDefault || "transparent");
    };
    state.controls.push({ update });
    box.append(sl, value, button);
    return row({ label: "BorderStyleLabel", description: "BorderStyleHelp", control: box });
  }

  function themesPage() {
    const start = state.controls.length;
    const page = el("div", "page");
    const add = button("ThemePackNew", "Plus", async () => {
      const name = await promptText(t("ThemePackNew"), t("ThemePackNewDefault"));
      if (name) post({ type: "themeNew", name });
    });
    page.appendChild(pageHeader("Themes", "SettingsPageThemes", add)).classList.add("themes");
    const th = themes();
    page.appendChild(el("div", "theme-label", t("BuiltInThemes")));
    page.appendChild(gallery(th.builtIn || []));
    page.appendChild(el("div", "theme-label mine", t("MyThemes")));
    if ((th.custom || []).length) page.appendChild(gallery(th.custom));
    else page.appendChild(el("div", "theme-empty", t("NoCustomThemes")));

    const actions = el("div", "theme-actions");
    if (th.details) actions.appendChild(el("div", "theme-details", th.details));
    const wrap = el("div", "theme-buttons");
    const ghost = (key, glyph, onClick, enabled = true, danger = false) => {
      const b = el("button", "ghost" + (danger ? " danger" : ""));
      b.append(icon(glyph, 14), el("span", "", t(key)));
      b.disabled = !enabled;
      b.addEventListener("click", onClick);
      return b;
    };
    const has = !!th.selected;
    wrap.append(
      ghost("ThemePackImport", "Upload", () => post({ type: "themeImport" })),
      ghost("ThemePackOpenFolder", "Folder", () => post({ type: "themeOpenFolder" })),
      ghost("ThemePackReload", "Refresh", () => post({ type: "themeReload" })),
      el("div", "theme-sep"),
      ghost("ThemePackDuplicate", "Duplicate", async () => {
        const current = [...(th.builtIn || []), ...(th.custom || [])]
          .find((c) => c.id && String(c.id).toLowerCase() === String(th.selected).toLowerCase());
        const name = await promptText(t("ThemePackDuplicate"), `${current ? current.name : th.selected} ${t("ThemePackCopySuffix")}`);
        if (name) post({ type: "themeDuplicate", name });
      }, has),
      ghost("ThemePackExport", "Download", () => post({ type: "themeExport" }), has),
      ghost("ThemePackDelete", "Trash", () => post({ type: "themeDelete" }), has && !th.selectedBuiltIn, true),
    );
    actions.appendChild(wrap);
    page.appendChild(actions);

    page.appendChild(heading("Theme", true)).classList.add("theme-heading");
    page.appendChild(group([
      row({ label: "ColorMode", description: "ThemeHelp",
        control: settingSelect("Theme", [["System", "Windows"], ["Dark", "Dark"], ["Light", "Light"]], "Palette") }),
    ]));

    // `ThemePropertyEditor` (read-only part): the current look's colors and, for a built-in look, a copy to edit.
    const editor = el("div", "theme-editor");
    const note = state.editorNote && state.editorNote.id === th.selected ? state.editorNote.text : null;
    editor.appendChild(el("div", "editor-message", note || th.message || ""));
    const section = (key) => {
      const h = el("div", "editor-section");
      h.append(el("div", "editor-heading", t(key)), el("div", "editor-rule"));
      return h;
    };
    editor.appendChild(section("ColorPalette"));
    const strip = el("div", "swatch-strip");
    for (const sw of th.swatches || []) {
      const cell = el("div", "swatch-cell");
      const block = el("div", "swatch-block");
      block.style.background = sw.css;
      cell.append(block, el("div", "swatch-caption", t(sw.label)));
      strip.appendChild(cell);
    }
    editor.appendChild(strip);
    if (th.editor) editor.appendChild(themeEditor(th.editor, section));
    if (!th.editable) {
      editor.appendChild(section("CustomizeAppearance"));
      const create = el("button", "settings-button primary", t("ThemeCreateEditable"));
      create.addEventListener("click", () => post({ type: "themeCreateEditable" }));
      editor.appendChild(create);
    }
    page.appendChild(editor);

    page.appendChild(heading("DockGeometry"));
    const corner = () => s().DockCornerRadius ?? th.cornerDefault ?? 16;
    page.appendChild(group([
      row({ label: "DockCornerRadius", description: "DockCornerRadiusDescription",
        control: [slider({ min: 0, max: 32, step: 1, large: 4, value: corner, onInput: (v) => set({ DockCornerRadius: v }) }),
          valueText(48, () => `${corner()} px`)] }),
    ]));

    page.appendChild(heading("SectionSurfaces"));
    page.appendChild(group(["Dock", "Menu", "Widget", "Settings"].map(surfaceRow).concat([borderRow()])));
    themesHost = page;
    themesControls = state.controls.slice(start);
    return page;
  }

  /// Pages that come with later steps show their heading only.
  function emptyPage(titleKey, subtitleKey) {
    const page = el("div", "page");
    page.appendChild(pageHeader(titleKey, subtitleKey));
    return page;
  }

  const PAGES = [
    { id: "dock", icon: "NavDock", label: "Dock", build: dockPage },
    { id: "widgets", icon: "NavWidgets", label: "Widgets", build: widgetsPage },
    { id: "themes", icon: "NavThemes", label: "Themes", build: themesPage },
    { id: "shortcuts", icon: "NavShortcuts", label: "Shortcuts", build: shortcutsPage },
    { id: "preferences", icon: "NavPreferences", label: "Preferences", bottom: true, build: preferencesPage },
    { id: "about", icon: "NavAbout", label: "About", bottom: true, build: aboutPage },
  ];

  function build() {
    state.controls = [];
    const content = document.getElementById("content");
    const top = document.getElementById("nav-top");
    const bottom = document.getElementById("nav-bottom");
    content.innerHTML = "";
    top.innerHTML = "";
    bottom.innerHTML = "";
    for (const p of PAGES) {
      const item = el("button", "nav-item" + (p.id === state.page ? " selected" : ""));
      item.append(icon(p.icon, 17), el("span", "", t(p.label)));
      item.addEventListener("click", () => showPage(p.id));
      (p.bottom ? bottom : top).appendChild(item);
      p.item = item;
      const page = p.build();
      page.hidden = p.id !== state.page;
      p.page = page;
      content.appendChild(page);
    }
    document.getElementById("search-placeholder").textContent = t("SettingsSearchPlaceholder");
    document.getElementById("btn-min").title = t("Minimize");
    document.getElementById("btn-close").title = t("Close");
    refresh();
    requestAnimationFrame(() => fitIcons(document.body));
  }

  function showPage(id) {
    closePopup();
    state.page = id;
    for (const p of PAGES) {
      p.item.classList.toggle("selected", p.id === id);
      p.page.hidden = p.id !== id;
    }
    document.getElementById("content").scrollTop = 0;
    requestAnimationFrame(() => fitIcons(document.body));
  }

  function refresh() {
    for (const c of state.controls) c.update(state.settings);
    requestAnimationFrame(() => fitIcons(document.body));
  }

  // ---------- Theme ----------
  /// "#AARRGGBB" (WPF) to CSS.
  function css(argb, alphaScale = 1) {
    const h = argb.replace("#", "");
    const a = h.length === 8 ? parseInt(h.slice(0, 2), 16) : 255;
    const rgb = h.length === 8 ? h.slice(2) : h;
    const r = parseInt(rgb.slice(0, 2), 16), g = parseInt(rgb.slice(2, 4), 16), b = parseInt(rgb.slice(4, 6), 16);
    return `rgba(${r}, ${g}, ${b}, ${((a / 255) * alphaScale).toFixed(4)})`;
  }
  function applyTheme(theme) {
    const dark = theme.dark;
    const root = document.documentElement.style;
    const v = (name, value) => root.setProperty(name, value);
    v("--text", dark ? "#F3F4F6" : "#1B1B1B");
    v("--text2", dark ? "#AAB2C0" : "#61666E");
    v("--stroke", dark ? "#3A4352" : "#E5E5E5");
    v("--control", dark ? "#252B37" : "#FFFFFF");
    v("--control-hover", css(dark ? "#14FFFFFF" : "#0D000000"));
    v("--track", dark ? "#141922" : "#E9E9E9");
    v("--danger-text", dark ? "#FF9DA7" : "#AF2338");
    v("--selected-strong", css(dark ? "#22FFFFFF" : "#16000000"));
    v("--surface-hover", css(dark ? "#14FFFFFF" : "#0D000000"));
    v("--surface-selected", css(dark ? "#14FFFFFF" : "#0D000000"));
    v("--focus", css(theme.focus || theme.accent));
    v("--popup-solid", dark ? "#252B37" : "#FFFFFF");
    v("--accent", css(theme.accent));
    v("--accent-text", css(theme.accentText));
    state.dark = dark;
    const o = theme.overrides || {};
    for (const [name, value] of Object.entries(o)) if (name.startsWith("--")) v(name, css(value));
    const page = o.page || (dark ? "#FF1B2029" : "#FFF3F3F3");
    v("--page-solid", css("#FF" + page.replace("#", "").slice(-6)));
    const sidebar = o.sidebar || (dark ? "#FF171C26" : "#FFEEEEEE");
    v("--page", css(page));
    // `SettingsGlassMaterial` / `SettingsSidebarGlass` / `SettingsContentGlass` by the Settings surface style.
    const background = theme.background;
    const opacity = Math.max(0, Math.min(1, theme.opacity ?? 0.9));
    if (background === "Solid") {
      v("--frame", css(page)); v("--sidebar", css(sidebar)); v("--content", css(page));
    } else if (background === "None") {
      v("--frame", "transparent"); v("--sidebar", "transparent"); v("--content", "transparent");
    } else {
      v("--frame", css(page, opacity)); v("--sidebar", css(sidebar, opacity)); v("--content", "transparent");
    }
    const stops = theme.borderColor
      ? [[theme.borderColor, 0]]
      : dark
        ? [["#70FFFFFF", 0], ["#25FFFFFF", 35], ["#10FFFFFF", 75], ["#4A0B1220", 100]]
        : [["#85FFFFFF", 0], ["#30000000", 35], ["#20000000", 75], ["#400F172A", 100]];
    v("--rim", background === "None" ? "transparent"
      : stops.length === 1 ? css(stops[0][0]) : `linear-gradient(${stops.map(([c, o]) => `${css(c)} ${o}%`).join(", ")})`);
    v("--rim-width", `${Math.max(0, Math.min(12, theme.borderThickness ?? 1))}px`);
  }

  // ---------- Window chrome ----------
  const titlebar = document.getElementById("titlebar");
  titlebar.addEventListener("mousedown", (e) => {
    if (e.button !== 0 || e.target.closest("button, .search")) return;
    if (e.detail === 2) { post({ type: "maximize" }); return; }
    post({ type: "drag" });
  });
  document.getElementById("btn-min").addEventListener("click", () => post({ type: "minimize" }));
  document.getElementById("btn-max").addEventListener("click", () => post({ type: "maximize" }));
  document.getElementById("btn-close").addEventListener("click", () => { flush(); post({ type: "close" }); });
  const searchInput = document.getElementById("search-input");
  const searchPlaceholder = document.getElementById("search-placeholder");
  const searchClear = document.getElementById("search-clear");
  searchInput.addEventListener("input", () => {
    searchPlaceholder.hidden = searchInput.value.length > 0;
    searchClear.hidden = searchInput.value.length === 0;
  });
  searchClear.addEventListener("click", () => { searchInput.value = ""; searchInput.dispatchEvent(new Event("input")); });
  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape" || state.recording) return;
    if (openPopup) { closePopup(); return; }
    if (searchInput.value) { searchClear.click(); return; }
    flush();
    post({ type: "close" });
  });
  document.addEventListener("contextmenu", (e) => e.preventDefault());

  window.ot = {
    receive(message) {
      const data = message.data;
      switch (message.type) {
        case "init":
          state.settings = data.settings || {};
          state.strings = data.strings || {};
          state.displays = data.displays || [];
          state.hardware = !!data.hardware;
          state.startup = !!data.startup;
          state.languages = data.languages || [];
          state.hotkeys = data.hotkeys || {};
          state.catalog = data.catalog || [];
          state.themes = data.themes || null;
          state.version = data.version;
          document.getElementById("version").textContent = "v" + data.version;
          applyTheme(data.theme);
          build();
          break;
        case "settings":
          state.settings = data;
          refresh();
          break;
        case "strings":
          state.strings = data;
          build();
          break;
        case "theme":
          applyTheme(data);
          break;
        case "editorMessage":
          state.editorNote = { id: state.themes && state.themes.selected, text: data };
          document.querySelectorAll(".editor-message").forEach((m) => { m.textContent = data; });
          break;
        case "themes":
          state.themes = data;
          rebuildThemes();
          refresh();
          break;
        case "hotkeys":
          state.hotkeys = data;
          refresh();
          break;
        case "hotkeyError":
          state.hotkeyError = data.text;
          if (!data.keepRecording) state.recording = null;
          refresh();
          break;
        case "startup":
          state.startup = !!data;
          refresh();
          break;
        case "maximized": {
          document.body.classList.toggle("maximized", !!data);
          const max = document.getElementById("btn-max");
          max.innerHTML = data ? "&#xE923;" : "&#xE922;";
          max.title = t(data ? "Restore" : "Maximize");
          break;
        }
      }
    },
  };

  // Script errors go to the app's log.
  window.addEventListener("error", (e) => post({ type: "log", text: `${e.message} (line ${e.lineno})` }));
  window.addEventListener("unhandledrejection", (e) => post({ type: "log", text: String(e.reason) }));
  window.addEventListener("beforeunload", flush);
  post({ type: "ready" });
})();
