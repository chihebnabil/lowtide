// Lowtide desktop UI. Plain JS on purpose: no build step, nothing to audit but this file.

// Set this to your waitlist form (Tally, Typeform, Google Forms...) before shipping.
const WAITLIST_URL = "https://tally.so/r/YOUR-FORM-ID";
const STALE_DAYS = 30;

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const appWindow = window.__TAURI__.window.getCurrentWindow();

// ── Helpers ───────────────────────────────────────────────────

const $ = (id) => document.getElementById(id);
const esc = (s) =>
  String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const icon = (name, cls = "") => `<svg class="i ${cls}"><use href="#i-${name}"/></svg>`;

function sizeParts(bytes) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = bytes, i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return [i === 0 ? String(bytes) : v.toFixed(v >= 100 ? 0 : 1), units[i]];
}
const human = (b) => sizeParts(b).join(" ");

function span(days) {
  if (days < 60) return `${days} days`;
  if (days < 730) return `${Math.floor(days / 30)} months`;
  return `${Math.floor(days / 365)} years`;
}

/// "built today" / "built 5 days ago" for recent work, "2 months idle" once it's stale.
function lastBuilt(days) {
  if (days <= 0) return "built today";
  if (days === 1) return "built yesterday";
  if (days < STALE_DAYS) return `built ${days} days ago`;
  return `${span(days)} idle`;
}

const RISK = {
  safe: { label: "Safe", hint: "Regenerates on its own" },
  redownload: { label: "Re-download", hint: "Comes back, after a download or rebuild" },
  review: { label: "Review", hint: "Permanent: look before you delete" },
};

const CATS = {
  "System": { tint: "#9fb3c8", icon: "monitor" },
  "JavaScript": { tint: "#f0d264", code: "JS" },
  "Python": { tint: "#6fa8ff", code: "Py" },
  "AI / ML": { tint: "#c39bff", icon: "sparkle" },
  "Rust": { tint: "#ff9f6b", code: "Rs" },
  "Go": { tint: "#5ed3e8", code: "Go" },
  "C# / .NET": { tint: "#a78bfa", code: "C#" },
  "Java": { tint: "#ff8a80", code: "Jv" },
  "PHP": { tint: "#8f9cff", code: "Php" },
  "Game dev": { tint: "#7ee08f", icon: "gamepad" },
  "Editors": { tint: "#4fc3f7", icon: "code" },
  "Browsers": { tint: "#ffb86b", icon: "globe" },
  "Apps": { tint: "#f48fb1", icon: "grid" },
  "Projects": { tint: "#2fd6b0", icon: "folder" },
};
const PROJECT_TINTS = ["#2fd6b0", "#3bb4ea", "#f1d39a", "#c39bff", "#ff9f6b", "#6fa8ff", "#f48fb1", "#7ee08f"];

function iconTile(ic, tint = "#2fd6b0", cls = "") {
  return `<span class="tile ${cls}" style="--tint:${tint}">${icon(ic)}</span>`;
}

function tile(cat, cls = "") {
  const c = CATS[cat] ?? { tint: "#9fb3c8", code: cat.slice(0, 2) };
  return `<span class="tile ${cls}" style="--tint:${c.tint}">${c.icon ? icon(c.icon) : esc(c.code)}</span>`;
}

function projectTile(name) {
  let h = 0;
  for (const ch of name) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  const letters = name.replace(/[^a-z0-9]/gi, "").slice(0, 2) || "?";
  return `<span class="tile" style="--tint:${PROJECT_TINTS[h % PROJECT_TINTS.length]}">${esc(letters[0].toUpperCase() + letters.slice(1).toLowerCase())}</span>`;
}

// Only touch the DOM when the markup actually changed, so clicks and hovers survive scan bursts.
const lastHTML = new WeakMap();
function setHTML(el, html) {
  if (!el || lastHTML.get(el) === html) return;
  lastHTML.set(el, html);
  el.innerHTML = html;
}

// ── State ─────────────────────────────────────────────────────

const S = {
  view: "overview",
  info: null,
  disk: null,
  vdisks: null,
  caches: [],
  artifacts: [],
  projScanning: false,
  projScanId: 0,
  expanded: new Set(),
  cacheFilter: "all",
  projFilter: "all",
  root: "",
  recycle: false,
  reclaimed: 0,
  recycled: 0,
  skipped: 0,
  cleaning: 0,
};

const cacheSelectable = (c) => c.status === "ready" && c.bytes > 0;
const artSelectable = (a) => a.status === "ready";
const cachesScanning = () => S.caches.filter((c) => c.status === "scanning").length;
const cacheBytes = () => S.caches.reduce((s, c) => s + (c.status === "ready" || c.status === "cleaning" ? c.bytes : 0), 0);
const safeCaches = () => S.caches.filter((c) => cacheSelectable(c) && c.risk === "safe");
const projectBytes = () => S.artifacts.reduce((s, a) => s + (a.status === "done" ? 0 : a.bytes), 0);
const adminCaches = () => S.caches.filter((c) => c.status === "admin");

function cacheGroups() {
  const filter = S.cacheFilter;
  const items = S.caches.filter(
    (c) =>
      (c.status !== "ready" || c.bytes > 0 || c.freed) &&
      (filter === "all" || (filter === "safe" ? c.risk === "safe" : c.risk !== "safe")),
  );
  const groups = new Map();
  for (const c of items) {
    if (!groups.has(c.category)) groups.set(c.category, []);
    groups.get(c.category).push(c);
  }
  return [...groups.entries()]
    .map(([cat, list]) => ({
      cat,
      items: list.sort((a, b) => b.bytes - a.bytes || a.name.localeCompare(b.name)),
      bytes: list.reduce((s, c) => s + c.bytes, 0),
    }))
    .sort((a, b) => b.bytes - a.bytes);
}

function relative(path) {
  const root = S.root.replace(/[\\/]+$/, "");
  return path.toLowerCase().startsWith(root.toLowerCase() + "\\") ? path.slice(root.length + 1) : path;
}

function projectGroups(filter = S.projFilter) {
  const map = new Map();
  for (const a of S.artifacts) {
    if (!map.has(a.project)) map.set(a.project, { key: a.project, name: a.name, artifacts: [] });
    map.get(a.project).artifacts.push(a);
  }
  let groups = [...map.values()].map((g) => {
    g.artifacts.sort((a, b) => b.bytes - a.bytes);
    g.bytes = g.artifacts.reduce((s, a) => s + (a.status === "done" ? 0 : a.bytes), 0);
    g.ageDays = Math.min(...g.artifacts.map((a) => a.ageDays));
    g.done = g.artifacts.every((a) => a.status === "done");
    return g;
  });
  if (filter === "idle") groups = groups.filter((g) => g.ageDays >= STALE_DAYS);
  return groups.sort((a, b) => a.done - b.done || b.bytes - a.bytes);
}

function selection(view = S.view) {
  const items = view === "projects" ? S.artifacts.filter((a) => a.selected && artSelectable(a)) : S.caches.filter((c) => c.selected && cacheSelectable(c));
  return { items, bytes: items.reduce((s, i) => s + i.bytes, 0) };
}

// ── Rendering ─────────────────────────────────────────────────

let queued = false;
function render() {
  if (queued) return;
  queued = true;
  requestAnimationFrame(() => {
    queued = false;
    renderChrome();
    VIEWS[S.view].update();
    renderSelbar();
  });
}

function renderChrome() {
  document.querySelectorAll(".nav-item").forEach((n) => n.classList.toggle("active", n.dataset.view === S.view));
  const cachesShown = S.caches.filter((c) => c.status === "ready" && c.bytes > 0).length;
  $("nc-caches").textContent = cachesShown || "";
  $("nc-projects").textContent = S.projScanning ? "…" : new Set(S.artifacts.map((a) => a.project)).size || "";

  const scanning = cachesScanning();
  const viewBusy = S.view === "projects" ? S.projScanning : scanning > 0;
  document.querySelectorAll('[data-action="rescan"]').forEach((b) => {
    b.disabled = viewBusy || S.cleaning > 0;
    b.classList.toggle("spin", viewBusy);
  });
  setHTML(
    $("status"),
    S.cleaning
      ? `<span class="pulse"></span>Cleaning… ${S.cleaning} left`
      : scanning || S.projScanning
        ? `<span class="pulse"></span>Scanning${scanning ? ` · ${S.caches.length - scanning} of ${S.caches.length} locations` : " project folders"}`
        : `<span class="dot-ok"></span>Scan complete`,
  );

  if (S.disk) {
    $("drive-mini").hidden = false;
    const used = (S.disk.total - S.disk.free) / S.disk.total;
    $("dm-label").textContent = `Drive ${S.disk.drive}`;
    $("dm-free").textContent = `${human(S.disk.free)} free`;
    $("dm-used").style.width = `${used * 100}%`;
    const low = S.disk.free / S.disk.total < 0.1;
    $("dm-used").classList.toggle("low", low);
    $("drive-mini").classList.toggle("low", low);
  }

  const line = $("admin-line");
  if (S.info?.elevated) {
    line.className = "admin-line on";
    setHTML(line, `${icon("shield-check", "sm")}<span>Admin mode</span>`);
  } else {
    const n = adminCaches().length;
    line.className = "admin-line can";
    setHTML(line, `${icon("shield", "sm")}<span>${n ? `Run as admin · ${n} locked` : "Run as admin"}</span>`);
  }
}

function renderSelbar() {
  const bar = $("selbar");
  const show = (S.view === "caches" || S.view === "projects") && selection().items.length > 0;
  bar.hidden = !show;
  if (!show) return;
  const { items, bytes } = selection();
  $("sel-n").textContent = items.length;
  $("sel-noun").textContent = S.view === "projects" ? (items.length === 1 ? "folder selected" : "folders selected") : "selected";
  $("sel-size").textContent = human(bytes);
  const clean = $("sel-clean");
  clean.disabled = S.cleaning > 0;
  clean.textContent = S.cleaning ? "Cleaning…" : S.view === "projects" && S.recycle ? "Move to Recycle Bin" : `Clean ${human(bytes)}`;
}

// Animated number for the hero.
const tween = { value: 0, target: 0, raf: 0 };
function animateNumber(target) {
  if (target === tween.target) return;
  tween.target = target;
  cancelAnimationFrame(tween.raf);
  const from = tween.value, start = performance.now(), dur = 700;
  const step = (now) => {
    const t = Math.min(1, (now - start) / dur);
    const eased = 1 - Math.pow(1 - t, 3);
    tween.value = from + (target - from) * eased;
    const [n, u] = sizeParts(Math.round(tween.value));
    const el = $("ov-num");
    if (el) el.innerHTML = `${n}<span class="unit">${u}</span>`;
    if (t < 1) tween.raf = requestAnimationFrame(step);
  };
  tween.raf = requestAnimationFrame(step);
}

const WAVES = `<div class="hero-waves"><svg viewBox="0 0 2400 70" preserveAspectRatio="none">
  <path class="w1" d="M0 38 C 200 18, 400 58, 600 38 S 1000 18, 1200 38 S 1600 58, 1800 38 S 2200 18, 2400 38 V70 H0 Z"/>
  <path class="w2" d="M0 50 C 300 34, 450 66, 600 50 S 900 34, 1200 50 S 1500 66, 1800 50 S 2100 34, 2400 50 V70 H0 Z"/></svg></div>`;

const VIEWS = {
  overview: {
    mount() {
      tween.value = 0;
      tween.target = -1;
      return `
        <section class="hero">
          <div class="hero-top">
            <div>
              <div class="eyebrow" id="ov-eyebrow"></div>
              <div class="big-number" id="ov-num">0<span class="unit">B</span></div>
              <div class="hero-sub" id="ov-sub"></div>
              <div id="ov-reclaimed"></div>
            </div>
            <div class="hero-actions">
              <button class="btn ghost lg" data-go="caches">Review all ${icon("arrow", "sm")}</button>
              <button class="btn primary lg" id="ov-clean"></button>
            </div>
          </div>
          <div class="drive" id="ov-drive" hidden>
            <div class="drive-head"><span id="ov-drive-label"></span><span id="ov-drive-free"></span></div>
            <div class="drive-bar"><i class="seg-used" id="seg-used" style="width:0"></i><i class="seg-junk" id="seg-junk" style="width:0"></i></div>
            <div class="legend">
              <span><i class="l-used"></i>Your files &amp; apps</span>
              <span><i class="l-junk"></i>Junk Lowtide found</span>
              <span><i class="l-free"></i>Free</span>
            </div>
          </div>
          ${WAVES}
        </section>
        <div class="section-head"><h2>Where it's hiding</h2><span id="ov-cards-note"></span></div>
        <div class="cards" id="ov-cards"></div>
        <div id="ov-admin"></div>`;
    },
    update() {
      const scanning = cachesScanning();
      const busy = scanning || S.projScanning;
      const total = cacheBytes() + projectBytes();
      const safe = safeCaches();
      const safeBytes = safe.reduce((s, c) => s + c.bytes, 0);

      setHTML(
        $("ov-eyebrow"),
        busy ? `<span class="pulse"></span>Reading the shoreline…` : total ? "Washed up on this PC" : "Calm waters",
      );
      animateNumber(total);
      const folders = new Set(S.artifacts.filter((a) => a.status !== "done").map((a) => a.project)).size;
      setHTML(
        $("ov-sub"),
        !busy && total === 0
          ? "Nothing left to clean. Your tools will pile things up again; come back in a few weeks."
          : `<b>${human(safeBytes)}</b> is completely safe to clean right now${folders ? ` · <b>${human(projectBytes())}</b> in ${folders} project${folders > 1 ? "s" : ""}` : ""}`,
      );
      setHTML(
        $("ov-reclaimed"),
        S.reclaimed + S.recycled > 0
          ? `<span class="reclaimed">${icon("check", "xs")}${human(S.reclaimed + S.recycled)} back on your drive this session</span>`
          : "",
      );

      const btn = $("ov-clean");
      btn.disabled = S.cleaning > 0 || safeBytes === 0;
      btn.textContent = S.cleaning ? "Cleaning…" : safeBytes ? `Clean safe items · ${human(safeBytes)}` : scanning ? "Scanning…" : "Nothing safe to clean";

      if (S.disk) {
        const { total: t, free, drive } = S.disk;
        const junk = Math.min(total, t - free);
        $("ov-drive").hidden = false;
        $("ov-drive-label").innerHTML = `Drive ${esc(drive)} · <b>${human(t - free)}</b> used of ${human(t)}`;
        $("ov-drive-free").innerHTML = `<b>${human(free)}</b> free`;
        $("seg-used").style.width = `${((t - free - junk) / t) * 100}%`;
        $("seg-junk").style.width = `${Math.max(junk ? 0.6 : 0, (junk / t) * 100)}%`;
      }

      // Category cards, biggest first.
      const byCat = new Map();
      for (const c of S.caches) {
        if (c.status !== "ready" || !c.bytes) continue;
        const e = byCat.get(c.category) ?? { cat: c.category, bytes: 0, names: [] };
        e.bytes += c.bytes;
        e.names.push(c.name);
        byCat.set(c.category, e);
      }
      const cards = [...byCat.values()];
      if (projectBytes()) {
        const idle = projectGroups().filter((g) => !g.done && g.ageDays >= STALE_DAYS).length;
        cards.push({ cat: "Projects", bytes: projectBytes(), names: [`${folders} projects${idle ? `, ${idle} idle 30+ days` : ""}`], go: "projects", title: "Project build folders" });
      }
      cards.sort((a, b) => b.bytes - a.bytes);
      const max = Math.max(1, ...cards.map((c) => c.bytes));
      setHTML(
        $("ov-cards"),
        cards.length
          ? cards
              .map(
                (c) => `<button class="card" data-go="${c.go ?? "caches"}" data-cat="${esc(c.cat)}">
                  ${tile(c.cat)}
                  <span class="card-title">${esc(c.title ?? c.cat)}</span>
                  <span class="card-size">${human(c.bytes)}</span>
                  <span class="card-sub">${esc(c.names.join(", "))}</span>
                  <span class="card-bar"><i style="width:${(c.bytes / max) * 100}%"></i></span>
                </button>`,
              )
              .join("")
          : busy
            ? `<div class="skeleton"></div><div class="skeleton"></div><div class="skeleton"></div>`
            : "",
      );
      $("ov-cards-note").textContent = cards.length ? `${cards.length} places` : "";

      const locked = adminCaches();
      setHTML(
        $("ov-admin"),
        locked.length && !S.info?.elevated
          ? `<div class="note">${icon("lock", "sm")}<span>${locked.map((c) => esc(c.name)).join(", ")} ${locked.length > 1 ? "need" : "needs"} administrator rights to scan.</span><button class="link" data-action="admin">Restart as admin</button></div>`
          : "",
      );
    },
  },

  caches: {
    mount() {
      return `
        <header class="page-head">
          <div><h1>Caches</h1><p>Copies your tools downloaded or generated. Everything marked <b>Safe</b> regenerates on its own, so it's pre-selected.</p></div>
          <div class="head-actions">
            <div class="seg" id="cache-filter"></div>
            <button class="icon-btn" data-action="rescan" title="Rescan">${icon("refresh", "sm")}</button>
          </div>
        </header>
        <div id="cache-list"></div>`;
    },
    update() {
      const counts = {
        all: S.caches.filter((c) => c.bytes > 0).length,
        safe: S.caches.filter((c) => c.bytes > 0 && c.risk === "safe").length,
        other: S.caches.filter((c) => c.bytes > 0 && c.risk !== "safe").length,
      };
      setHTML(
        $("cache-filter"),
        [["all", "All", counts.all], ["safe", "Safe", counts.safe], ["other", "Re-download", counts.other]]
          .map(([k, l, n]) => `<button data-filter="${k}" class="${S.cacheFilter === k ? "on" : ""}">${l}<span>${n}</span></button>`)
          .join(""),
      );
      const groups = cacheGroups();
      if (!groups.length) {
        setHTML($("cache-list"), cachesScanning() ? skeletonList() : emptyState("layers", "All clear", "No caches worth cleaning right now."));
        return;
      }
      setHTML($("cache-list"), groups.map(cacheGroupHTML).join(""));
    },
  },

  projects: {
    mount() {
      return `
        <header class="page-head">
          <div><h1>Projects</h1><p>Build folders regenerate from your source code with one command. Projects you haven't touched in a while are the easy wins.</p></div>
          <div class="head-actions"><button class="icon-btn" data-action="rescan" title="Rescan">${icon("refresh", "sm")}</button></div>
        </header>
        <div class="toolbar">
          <button class="folder-chip" data-action="pick-folder" title="Folder to scan">${icon("folder", "sm")}<span id="proj-root"></span><span class="change">Change</span></button>
          <div class="seg" id="proj-filter"></div>
          <label class="toggle" title="Deleted folders go to the Recycle Bin, so you can restore them">
            <input type="checkbox" id="recycle-toggle" /><span class="track"></span>Move to Recycle Bin
          </label>
        </div>
        <div id="proj-list"></div>`;
    },
    update() {
      $("proj-root").textContent = S.root;
      $("recycle-toggle").checked = S.recycle;
      const all = projectGroups();
      const groupsAll = projectGroups("all");
      const idle = groupsAll.filter((g) => !g.done && g.ageDays >= STALE_DAYS).length;
      setHTML(
        $("proj-filter"),
        [["all", "All", groupsAll.length], ["idle", `Idle ${STALE_DAYS}d+`, idle]]
          .map(([k, l, n]) => `<button data-pfilter="${k}" class="${S.projFilter === k ? "on" : ""}">${l}<span>${n}</span></button>`)
          .join(""),
      );
      if (!all.length) {
        setHTML(
          $("proj-list"),
          S.projScanning
            ? skeletonList()
            : emptyState("folder", "No build folders found", `Nothing in ${esc(S.root)}. Point Lowtide at the folder where you keep your code.`, `<button class="btn ghost" data-action="pick-folder">Choose folder</button>`),
        );
        return;
      }
      setHTML($("proj-list"), `<div class="list">${all.map(projectHTML).join("")}</div>`);
    },
  },

  vdisks: {
    mount() {
      if (!S.vdisks) invoke("virtual_disks").then((d) => { S.vdisks = d; render(); });
      return `
        <header class="page-head"><div><h1>Docker &amp; WSL</h1><p>The biggest hidden space hog on a dev machine.</p></div></header>
        <section class="pro-hero" id="vd-hero"></section>
        <div id="vd-list"></div>`;
    },
    update() {
      const disks = S.vdisks;
      const total = (disks ?? []).reduce((s, d) => s + d.bytes, 0);
      const [n, u] = sizeParts(total);
      setHTML(
        $("vd-hero"),
        `<div class="pro-badge">${icon("sparkle", "xs")} Lowtide Pro</div>
         ${disks === null
           ? `<div class="big-number" style="font-size:40px">Measuring…</div>`
           : disks.length
             ? `<div class="big-number">${n}<span class="unit">${u}</span></div>`
             : `<div class="big-number" style="font-size:40px">No virtual disks</div>`}
         <p>${disks?.length
           ? `in ${disks.length} virtual disk${disks.length > 1 ? "s" : ""}. WSL and Docker Desktop store Linux in <b>.vhdx</b> files that grow as you work but <b>never shrink</b>, even after you delete files inside. Lowtide Pro will compact them safely in one click, often giving back tens of gigabytes.`
           : "Lowtide didn't find WSL or Docker Desktop disks on this PC. If you install them later, this is where their hidden growth shows up."}</p>
         <div class="pro-actions"><button class="btn sand" data-action="waitlist">Join the Pro waitlist</button><span>Everything you use today stays free.</span></div>`,
      );
      setHTML(
        $("vd-list"),
        disks?.length
          ? `<div class="section-head"><h2>Found on this PC</h2></div><div class="list">${disks
              .map(
                (d) => `<div class="vdisk">${tile("System")}<div><div class="item-title">${esc(d.name)}</div><div class="item-desc" title="${esc(d.path)}">${esc(d.path)}</div></div><span class="item-size">${human(d.bytes)}</span></div>`,
              )
              .join("")}</div>`
          : `<div class="features">
              <div class="feature">${iconTile("box", "#f1d39a")}<h4>One-click compaction</h4><p>Shrinks WSL and Docker disks back to what they actually hold.</p></div>
              <div class="feature">${iconTile("shield-check", "#f1d39a")}<h4>Safe by design</h4><p>Shuts WSL down cleanly first, and verifies the disk before and after.</p></div>
              <div class="feature">${iconTile("layers", "#f1d39a")}<h4>Image &amp; volume cleanup</h4><p>Finds dangling Docker images and build cache, with sizes, before anything is removed.</p></div>
            </div>`,
      );
    },
  },

  autopilot: {
    mount() {
      const f = (ic, t, d) => `<div class="feature">${iconTile(ic, "#f1d39a")}<h4>${t}</h4><p>${d}</p></div>`;
      return `
        <header class="page-head"><div><h1>Autopilot</h1><p>Set it once. Lowtide keeps the tide low without you thinking about it.</p></div></header>
        <section class="pro-hero">
          <div class="pro-badge">${icon("sparkle", "xs")} Lowtide Pro</div>
          <div class="big-number" style="font-size:40px">Never clean by hand again</div>
          <p>Autopilot runs quietly in the background, only touches what you allow, and tells you what it did.</p>
          <div class="pro-actions"><button class="btn sand" data-action="waitlist">Join the Pro waitlist</button><span>Everything you use today stays free.</span></div>
        </section>
        <div class="features">
          ${f("clock", "Weekly clean", "Safe caches cleaned every week, while you're away from the keyboard.")}
          ${f("bell", "Low-disk alerts", "A heads-up before C: fills up, with exactly what you can clean.")}
          ${f("folder", "Idle project rules", "Trim node_modules and target folders in projects untouched for 60 days.")}
          ${f("layers", "Space history", "See which tools grew this month, and by how much.")}
        </div>`;
    },
    update() {},
  },
};

function skeletonList() {
  return `<div class="list">${'<div class="item"><div class="item-main" style="cursor:default"><div class="skeleton" style="height:14px;grid-column:1/-1;border:0"></div></div></div>'.repeat(5)}</div>`;
}

function emptyState(ic, title, text, action = "") {
  return `<div class="empty">${iconTile(ic)}<h3>${title}</h3><p>${text}</p>${action}</div>`;
}

function sizeCell(item) {
  if (item.status === "scanning") return `<span class="item-size dim"><span class="spin">${icon("refresh", "xs")}</span></span>`;
  if (item.status === "cleaning") return `<span class="item-size dim"><span class="spin">${icon("refresh", "xs")}</span>Cleaning</span>`;
  if (item.status === "admin") return `<span class="item-size dim">${icon("lock", "xs")}Admin</span>`;
  if (item.status === "done") return `<span class="item-size ok">${icon("check", "xs")} ${human(item.freed.freed + item.freed.recycled)}</span>`;
  if (item.bytes === 0) return `<span class="item-size ok">Clean</span>`;
  return `<span class="item-size">${human(item.bytes)}</span>`;
}

function cbClass(on, enabled) {
  return enabled ? (on ? "cb on" : "cb") : "cb off";
}

function cacheGroupHTML(g) {
  const selectable = g.items.filter(cacheSelectable);
  const allOn = selectable.length && selectable.every((c) => c.selected);
  return `<section class="group" id="cat-${esc(g.cat.replace(/\W/g, ""))}">
    <div class="group-head">${tile(g.cat, "sm")}<h3>${esc(g.cat)}</h3><span class="group-total">${human(g.bytes)}</span>
      ${selectable.length ? `<button class="link" data-action="select-group" data-cat="${esc(g.cat)}">${allOn ? "Deselect all" : "Select all"}</button>` : ""}
    </div>
    <div class="list">${g.items.map(cacheItemHTML).join("")}</div>
  </section>`;
}

function cacheItemHTML(c) {
  const open = S.expanded.has(c.id);
  const sel = cacheSelectable(c);
  const freed = c.freed
    ? `<div class="kv"><h4>This session</h4><p class="freed-line">Freed ${human(c.freed.freed)}</p>${c.freed.skipped ? `<p>${c.freed.skipped} file(s) were in use and skipped. Close the app and clean again.</p>` : ""}</div>`
    : "";
  const admin = c.status === "admin"
    ? `<div class="kv full"><h4>Locked</h4><p>Windows only lets administrators read this folder. <button class="link" data-action="admin">Restart as admin</button></p></div>`
    : "";
  return `<div class="item${open ? " open" : ""}${!sel && c.status !== "cleaning" ? " dim" : ""}" data-key="${esc(c.id)}">
    <div class="item-main" data-action="expand">
      <button class="${cbClass(c.selected && sel, sel)}" data-action="toggle" aria-label="Select ${esc(c.name)}"></button>
      <div><div class="item-title">${esc(c.name)}</div><div class="item-desc">${esc(c.what)}</div></div>
      <span class="risk ${c.risk}" title="${RISK[c.risk].hint}">${RISK[c.risk].label}</span>
      ${sizeCell(c)}
      <svg class="i sm chev"><use href="#i-chevron"/></svg>
    </div>
    <div class="item-more"><div><div class="more-body">
      <div class="kv"><h4>If you delete it</h4><p>${esc(c.after)}</p></div>
      ${c.status === "ready" && c.bytes ? `<div class="kv"><h4>Size</h4><p>${human(c.bytes)} in ${c.files.toLocaleString()} files</p></div>` : freed}
      ${c.status === "ready" && c.bytes ? freed : ""}
      ${admin}
      ${c.paths.length ? `<div class="kv full"><h4>Location</h4><div class="paths">${c.paths
        .map((p) => `<div class="path"><span title="${esc(p)}">${esc(p)}</span><button class="icon-btn sm" data-action="reveal" data-path="${esc(p)}" title="Show in Explorer">${icon("external", "xs")}</button></div>`)
        .join("")}</div></div>` : ""}
    </div></div></div>
  </div>`;
}

function projectHTML(g) {
  const open = S.expanded.has(g.key);
  const selectable = g.artifacts.filter(artSelectable);
  const on = selectable.filter((a) => a.selected).length;
  const cb = !selectable.length ? "cb off" : on === 0 ? "cb" : on === selectable.length ? "cb on" : "cb mixed";
  const stale = g.ageDays >= STALE_DAYS;
  const size = g.done
    ? `<span class="item-size ok">${icon("check", "xs")} ${human(g.artifacts.reduce((s, a) => s + (a.freed ? a.freed.freed + a.freed.recycled : 0), 0))}</span>`
    : g.artifacts.some((a) => a.status === "cleaning")
      ? `<span class="item-size dim"><span class="spin">${icon("refresh", "xs")}</span>Cleaning</span>`
      : `<span class="item-size">${human(g.bytes)}</span>`;
  return `<div class="item proj${open ? " open" : ""}${g.done ? " dim" : ""}" data-key="${esc(g.key)}">
    <div class="item-main" data-action="expand">
      <button class="${cb}" data-action="toggle-project" aria-label="Select ${esc(g.name)}"></button>
      ${projectTile(g.name)}
      <div><div class="item-title">${esc(g.name)}</div><div class="item-desc">${esc(relative(g.key))} · ${g.artifacts.map((a) => esc(a.kind)).join(", ")}</div></div>
      <span class="idle${stale && !g.done ? " stale" : ""}">${lastBuilt(g.ageDays)}</span>
      ${size}
      <svg class="i sm chev"><use href="#i-chevron"/></svg>
    </div>
    <div class="item-more"><div><div class="more-body">
      ${g.artifacts
        .map(
          (a) => `<div class="artifact">
            <button class="${cbClass(a.selected && artSelectable(a), artSelectable(a))}" data-action="toggle-artifact" data-path="${esc(a.path)}" aria-label="Select ${esc(a.kind)}"></button>
            <span class="kind">${esc(a.kind)}</span>
            <code title="Rebuild with: ${esc(a.restore)}">${esc(a.restore)}</code>
            ${sizeCell(a)}
            <button class="icon-btn sm" data-action="reveal" data-path="${esc(a.path)}" title="Show in Explorer">${icon("external", "xs")}</button>
          </div>`,
        )
        .join("")}
    </div></div></div>
  </div>`;
}

// ── Navigation ────────────────────────────────────────────────

function go(view, anchor) {
  S.view = view;
  const page = $("page");
  lastHTML.delete(page);
  page.innerHTML = VIEWS[view].mount();
  page.scrollTop = 0;
  renderChrome();
  VIEWS[view].update();
  renderSelbar();
  if (anchor) requestAnimationFrame(() => document.getElementById(anchor)?.scrollIntoView({ block: "start" }));
}

// ── Actions ───────────────────────────────────────────────────

async function scanCaches() {
  const targets = await invoke("scan_caches");
  S.caches = targets.map((t) => ({ ...t, status: "scanning", bytes: 0, files: 0, selected: false, freed: null }));
  render();
}

async function scanProjects() {
  S.artifacts = [];
  S.projScanning = true;
  render();
  S.projScanId = await invoke("scan_projects", { root: S.root });
}

async function refreshDisk() {
  S.disk = await invoke("disk_space").catch(() => null);
  render();
}

function confirmClean(kind, items) {
  if (!items.length) return;
  const bytes = items.reduce((s, i) => s + i.bytes, 0);
  const recycle = kind === "projects" && S.recycle;
  const later = kind === "projects" ? items.length : items.filter((i) => i.risk !== "safe").length;
  $("confirm-title").textContent = recycle ? `Move ${human(bytes)} to the Recycle Bin?` : `Clean ${human(bytes)}?`;
  $("confirm-body").textContent =
    kind === "projects"
      ? recycle
        ? "You can restore these folders from the Recycle Bin. The space comes back when you empty it."
        : "These build folders are deleted for good. Your source code isn't touched, and each one comes back with its rebuild command."
      : later
        ? `${later} of these will need a re-download or rebuild the next time you use them. Everything else regenerates on its own.`
        : "Everything selected regenerates on its own. Files in use are skipped automatically.";
  $("dlg-icon").className = later && !recycle ? "dlg-icon warn" : "dlg-icon";
  const rows = items.slice(0, 40).map((i) => `<li><span>${esc(kind === "projects" ? `${i.name} · ${i.kind}` : i.name)}</span><span>${human(i.bytes)}</span></li>`);
  if (items.length > 40) rows.push(`<li><span>and ${items.length - 40} more</span><span></span></li>`);
  $("confirm-list").innerHTML = rows.join("");
  $("confirm-ok").textContent = recycle ? "Move to Recycle Bin" : `Clean ${human(bytes)}`;
  pendingClean = { kind, items };
  $("confirm").returnValue = "";
  $("confirm").showModal();
}

let pendingClean = null;
async function runClean() {
  if (!pendingClean) return;
  const { kind, items } = pendingClean;
  pendingClean = null;
  S.skipped = 0;
  for (const i of items) i.status = "cleaning";
  S.cleaning += items.length;
  render();
  if (kind === "projects") await invoke("clean_projects", { paths: items.map((i) => i.path), recycle: S.recycle });
  else await invoke("clean_caches", { ids: items.map((i) => i.id) });
}

function finishOne(stats) {
  S.cleaning = Math.max(0, S.cleaning - 1);
  S.reclaimed += stats.freed;
  S.recycled += stats.recycled;
  S.skipped += stats.skipped;
  if (S.cleaning === 0) {
    refreshDisk();
    let msg = `<b>Tide's out.</b> ${human(S.reclaimed + S.recycled)} back on your drive this session.`;
    if (S.recycled) msg += ` ${human(S.recycled)} is waiting in the Recycle Bin.`;
    if (S.skipped) msg += ` ${S.skipped} file(s) were in use and skipped.`;
    toast(msg);
  }
}

let toastTimer;
function toast(html) {
  const t = $("toast");
  t.innerHTML = `${icon("sparkle")}<div>${html}</div>`;
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), 7000);
}

async function saveSettings() {
  await invoke("save_settings", { projectsRoot: S.root, recycleProjects: S.recycle }).catch(() => {});
}

function toggleExpand(itemEl) {
  const key = itemEl.dataset.key;
  const open = !S.expanded.has(key);
  open ? S.expanded.add(key) : S.expanded.delete(key);
  // Animate in place, then let the next render reconcile.
  itemEl.classList.toggle("open", open);
  const list = itemEl.closest("#cache-list, #proj-list");
  if (list) lastHTML.set(list, S.view === "caches" ? cacheGroups().map(cacheGroupHTML).join("") : `<div class="list">${projectGroups().map(projectHTML).join("")}</div>`);
}

// ── Events ────────────────────────────────────────────────────

document.addEventListener("click", async (e) => {
  const nav = e.target.closest(".nav-item");
  if (nav) return go(nav.dataset.view);

  const goEl = e.target.closest("[data-go]");
  if (goEl) {
    const cat = goEl.dataset.cat;
    return go(goEl.dataset.go, cat && goEl.dataset.go === "caches" ? `cat-${cat.replace(/\W/g, "")}` : null);
  }

  const el = e.target.closest("[data-action], [data-filter], [data-pfilter]");
  if (!el) return;
  if (el.dataset.filter) { S.cacheFilter = el.dataset.filter; return render(); }
  if (el.dataset.pfilter) { S.projFilter = el.dataset.pfilter; return render(); }

  const item = el.closest(".item");
  switch (el.dataset.action) {
    case "expand": return toggleExpand(item);
    case "toggle": {
      const c = S.caches.find((c) => c.id === item.dataset.key);
      if (c && cacheSelectable(c)) c.selected = !c.selected;
      return render();
    }
    case "toggle-project": {
      const arts = S.artifacts.filter((a) => a.project === item.dataset.key && artSelectable(a));
      const next = !arts.every((a) => a.selected);
      arts.forEach((a) => (a.selected = next));
      return render();
    }
    case "toggle-artifact": {
      const a = S.artifacts.find((a) => a.path === el.dataset.path);
      if (a && artSelectable(a)) a.selected = !a.selected;
      return render();
    }
    case "select-group": {
      const items = S.caches.filter((c) => c.category === el.dataset.cat && cacheSelectable(c));
      const next = !items.every((c) => c.selected);
      items.forEach((c) => (c.selected = next));
      return render();
    }
    case "rescan":
      if (S.cleaning) return;
      return S.view === "projects" ? scanProjects() : scanCaches();
    case "reveal": return invoke("reveal", { path: el.dataset.path });
    case "admin": return relaunchAdmin();
    case "waitlist": return openWaitlist();
    case "pick-folder": {
      const picked = await invoke("pick_folder", { current: S.root });
      if (picked && picked !== S.root) {
        S.root = picked;
        await saveSettings();
        scanProjects();
      }
      return;
    }
  }
});

document.addEventListener("change", (e) => {
  if (e.target.id === "recycle-toggle") {
    S.recycle = e.target.checked;
    saveSettings();
    render();
  }
});

document.addEventListener("keydown", (e) => {
  if ($("confirm").open) return;
  if (e.ctrlKey && ["1", "2", "3"].includes(e.key)) {
    e.preventDefault();
    go(["overview", "caches", "projects"][Number(e.key) - 1]);
  }
});

async function relaunchAdmin() {
  if (S.info?.elevated) return;
  const ok = await invoke("relaunch_as_admin");
  if (!ok) toast("Administrator restart was cancelled.");
}

function openWaitlist() {
  if (WAITLIST_URL.includes("YOUR-FORM-ID")) return toast("The Lowtide Pro waitlist opens soon. Thanks for the interest!");
  invoke("open_url", { url: WAITLIST_URL });
}

$("admin-line").addEventListener("click", relaunchAdmin);
$("sel-clear").addEventListener("click", () => {
  if (S.view === "projects") S.artifacts.forEach((a) => (a.selected = false));
  else S.caches.forEach((c) => (c.selected = false));
  render();
});
$("sel-clean").addEventListener("click", () => confirmClean(S.view, selection().items));
$("page").addEventListener("click", (e) => {
  if (e.target.closest("#ov-clean")) confirmClean("caches", safeCaches());
});
$("confirm").addEventListener("close", () => {
  if ($("confirm").returnValue === "ok") runClean();
  else pendingClean = null;
});

$("wc-min").addEventListener("click", () => appWindow.minimize());
$("wc-max").addEventListener("click", () => appWindow.toggleMaximize());
$("wc-close").addEventListener("click", () => appWindow.close());

listen("cache:measured", ({ payload: m }) => {
  const c = S.caches.find((c) => c.id === m.id);
  if (!c) return;
  c.status = m.needsAdmin ? "admin" : "ready";
  c.bytes = m.bytes;
  c.files = m.files;
  // Pre-select what's risk-free; everything else is opt-in.
  c.selected = c.risk === "safe" && c.bytes > 0;
  render();
});

listen("cache:cleaned", ({ payload: { stats, after } }) => {
  const c = S.caches.find((c) => c.id === after.id);
  if (c) {
    c.status = after.needsAdmin ? "admin" : "ready";
    c.bytes = after.bytes;
    c.files = after.files;
    c.selected = false;
    c.freed = c.freed ? { ...stats, freed: c.freed.freed + stats.freed } : stats;
  }
  finishOne(stats);
  render();
});

listen("project:found", ({ payload: a }) => {
  S.artifacts.push({ ...a, status: "ready", selected: false, freed: null });
  render();
});

listen("projects:done", ({ payload: id }) => {
  if (id >= S.projScanId) S.projScanning = false;
  render();
});

listen("project:cleaned", ({ payload: { path, stats } }) => {
  const a = S.artifacts.find((a) => a.path === path);
  if (a) {
    a.status = stats.skipped && !stats.freed && !stats.recycled ? "ready" : "done";
    a.freed = stats;
    a.selected = false;
  }
  finishOne(stats);
  render();
});

// ── Start ─────────────────────────────────────────────────────

(async () => {
  S.info = await invoke("get_info");
  S.root = S.info.projectsRoot;
  S.recycle = S.info.recycleProjects;
  $("version").textContent = `Lowtide ${S.info.version} · Free`;
  go("overview");
  refreshDisk();
  await scanCaches();
  scanProjects();
})();
