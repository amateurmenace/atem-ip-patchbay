/* alpha.50 system monitoring drawer — shared by single-view (index.html)
 * and multiview (multiview.html). Slides in from the right edge when the
 * operator clicks the system pill in either topbar.
 *
 * Renders:
 *   - global CPU + memory headline (matches the always-visible pill)
 *   - per-core CPU bar grid
 *   - swap usage (for paging warnings)
 *   - FFmpeg process list (sorted by CPU desc) with pid + cpu% + mem
 *   - available video encoders (so the operator can see "yes nvenc IS
 *     in this build" without digging through cargo features)
 *   - log tail (last 30 lines from /api/log)
 *
 * Polls /api/system-health at 1.5Hz (matches the backend's refresh
 * cadence) and /api/log on demand when the drawer opens. Cheap; one
 * fetch each, both cached `no-store`.
 *
 * Usage (called from each view's bootstrap):
 *   const drawer = createSystemDrawer({
 *     anchorEl: document.getElementById('sys-pill'),
 *     getLogUrl: () => '/api/log',
 *   });
 *   // Optional, automatic on click: drawer.show()
 */

(function () {
  "use strict";

  const POLL_INTERVAL_MS = 1500;

  // ---------- CSS injection ----------
  const STYLE_ID = "system-drawer-style";
  function ensureStyle() {
    if (document.getElementById(STYLE_ID)) return;
    const style = document.createElement("style");
    style.id = STYLE_ID;
    style.textContent = `
      .sys-drawer-scrim {
        position: fixed; inset: 0;
        background: rgba(0,0,0,0.35);
        opacity: 0; pointer-events: none;
        transition: opacity 0.18s ease;
        z-index: 998;
      }
      .sys-drawer-scrim.open { opacity: 1; pointer-events: auto; }
      .sys-drawer {
        position: fixed;
        top: 0; right: 0; bottom: 0;
        width: 440px; max-width: 92vw;
        background: #0e0f12;
        color: #e6e6ea;
        border-left: 1px solid #2a2e36;
        box-shadow: -8px 0 28px rgba(0,0,0,0.55);
        transform: translateX(110%);
        transition: transform 0.22s cubic-bezier(0.4, 0, 0.2, 1);
        z-index: 999;
        display: flex; flex-direction: column;
        font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
      }
      .sys-drawer.open { transform: translateX(0); }
      .sys-drawer-head {
        display: flex; align-items: center; gap: 8px;
        padding: 12px 16px;
        border-bottom: 1px solid #2a2e36;
        background: rgba(0,0,0,0.3);
      }
      .sys-drawer-head h2 {
        flex: 1;
        font-size: 13px;
        font-weight: 700;
        letter-spacing: 0.04em;
        text-transform: uppercase;
        color: #e6e6ea;
        margin: 0;
      }
      .sys-drawer-close {
        background: transparent;
        border: 1px solid #2a2e36;
        color: #8a8f9b;
        border-radius: 4px;
        padding: 4px 10px;
        cursor: pointer;
        font-family: inherit;
        font-size: 12px;
      }
      .sys-drawer-close:hover { color: #e6e6ea; border-color: #3a4250; }
      .sys-drawer-body {
        flex: 1; overflow-y: auto;
        padding: 12px 16px 24px;
        font-size: 12px;
      }
      .sys-section {
        margin-bottom: 18px;
      }
      .sys-section-title {
        font-size: 10px;
        text-transform: uppercase;
        letter-spacing: 0.08em;
        color: #66d3fa;
        font-weight: 700;
        margin-bottom: 8px;
      }
      .sys-headline {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: 10px;
      }
      .sys-headline-card {
        background: #15171c;
        border: 1px solid #2a2e36;
        border-radius: 6px;
        padding: 10px 12px;
      }
      .sys-headline-label {
        font-size: 10px;
        color: #8a8f9b;
        text-transform: uppercase;
        letter-spacing: 0.05em;
      }
      .sys-headline-value {
        font-size: 22px;
        font-weight: 700;
        font-variant-numeric: tabular-nums;
        margin-top: 2px;
      }
      .sys-headline-sub {
        font-size: 11px;
        color: #8a8f9b;
        margin-top: 2px;
        font-variant-numeric: tabular-nums;
      }
      .sys-headline-card.green .sys-headline-value { color: #5fd06b; }
      .sys-headline-card.yellow .sys-headline-value { color: #f0b84f; }
      .sys-headline-card.red .sys-headline-value { color: #e85c5c; }
      .sys-bar-row {
        display: grid;
        grid-template-columns: 28px 1fr 42px;
        align-items: center;
        gap: 6px;
        margin-bottom: 3px;
        font-size: 11px;
      }
      .sys-bar-row .lbl {
        color: #8a8f9b;
        font-variant-numeric: tabular-nums;
        text-align: right;
      }
      .sys-bar-row .pct {
        color: #e6e6ea;
        font-variant-numeric: tabular-nums;
        text-align: right;
      }
      .sys-bar-track {
        height: 8px;
        background: rgba(255,255,255,0.05);
        border-radius: 2px;
        overflow: hidden;
        position: relative;
      }
      .sys-bar-fill {
        height: 100%;
        background: linear-gradient(90deg, #5fd06b 0%, #5fd06b 70%, #f0b84f 70%, #f0b84f 90%, #e85c5c 90%);
        transition: width 0.18s ease;
      }
      .sys-cores-grid {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(50px, 1fr));
        gap: 4px;
      }
      .sys-core-cell {
        background: rgba(255,255,255,0.04);
        border-radius: 3px;
        padding: 4px 6px;
        text-align: center;
        font-size: 9px;
        position: relative;
        overflow: hidden;
      }
      .sys-core-fill {
        position: absolute;
        left: 0; right: 0; bottom: 0;
        background: linear-gradient(180deg, transparent 0%, rgba(95,208,107,0.35) 50%, rgba(240,184,79,0.5) 90%);
        z-index: 0;
        transition: height 0.18s ease;
      }
      .sys-core-cell.warn .sys-core-fill { background: linear-gradient(180deg, transparent 0%, rgba(240,184,79,0.6) 100%); }
      .sys-core-cell.hot .sys-core-fill  { background: linear-gradient(180deg, transparent 0%, rgba(232,92,92,0.6) 100%); }
      .sys-core-cell .cnum {
        position: relative; z-index: 1;
        color: #8a8f9b;
        font-weight: 700;
      }
      .sys-core-cell .cval {
        position: relative; z-index: 1;
        color: #e6e6ea;
        font-variant-numeric: tabular-nums;
        font-weight: 600;
        margin-top: 1px;
        display: block;
      }
      .sys-proc-table {
        width: 100%;
        border-collapse: collapse;
        font-size: 11px;
      }
      .sys-proc-table th, .sys-proc-table td {
        text-align: left;
        padding: 4px 6px;
        border-bottom: 1px solid rgba(255,255,255,0.05);
      }
      .sys-proc-table th {
        color: #8a8f9b;
        font-size: 9px;
        text-transform: uppercase;
        letter-spacing: 0.05em;
        font-weight: 700;
      }
      .sys-proc-table td.pid { font-variant-numeric: tabular-nums; color: #8a8f9b; }
      .sys-proc-table td.cpu { font-variant-numeric: tabular-nums; font-weight: 600; }
      .sys-proc-table td.cmd {
        font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
        font-size: 10px;
        color: #8a8f9b;
        max-width: 220px;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .sys-proc-empty {
        text-align: center;
        color: #8a8f9b;
        font-style: italic;
        padding: 12px 0;
      }
      .sys-encoder-chips {
        display: flex; flex-wrap: wrap; gap: 4px;
      }
      .sys-encoder-chip {
        background: rgba(102,211,250,0.08);
        border: 1px solid rgba(102,211,250,0.25);
        color: #66d3fa;
        font-size: 10px;
        padding: 3px 8px;
        border-radius: 10px;
        font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
      }
      .sys-encoder-chip.hw {
        background: rgba(95,208,107,0.10);
        border-color: rgba(95,208,107,0.40);
        color: #5fd06b;
      }
      .sys-log-tail {
        background: #050608;
        border: 1px solid #2a2e36;
        border-radius: 4px;
        padding: 8px 10px;
        font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
        font-size: 10px;
        color: #b4b8c0;
        max-height: 180px;
        overflow-y: auto;
        white-space: pre;
      }
    `;
    document.head.appendChild(style);
  }

  // ---------- DOM building ----------
  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    if (attrs) {
      for (const [k, v] of Object.entries(attrs)) {
        if (k === "class") node.className = v;
        else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
        else node.setAttribute(k, v);
      }
    }
    for (const c of children) {
      if (c === null || c === undefined || c === false) continue;
      if (typeof c === "string" || typeof c === "number")
        node.appendChild(document.createTextNode(String(c)));
      else node.appendChild(c);
    }
    return node;
  }

  function statusKlass(status) {
    switch ((status || "").toLowerCase()) {
      case "green":
        return "green";
      case "yellow":
        return "yellow";
      case "red":
        return "red";
      default:
        return "";
    }
  }

  function buildDrawer(opts) {
    ensureStyle();
    const scrim = el("div", { class: "sys-drawer-scrim", id: "sys-drawer-scrim" });
    const drawer = el(
      "div",
      { class: "sys-drawer", id: "sys-drawer", "aria-hidden": "true" },
      el(
        "div",
        { class: "sys-drawer-head" },
        el("h2", null, "System monitor"),
        el(
          "button",
          {
            class: "sys-drawer-close",
            type: "button",
            onclick: () => hide(),
          },
          "Close ×"
        )
      ),
      el(
        "div",
        { class: "sys-drawer-body" },
        // Headline cards (CPU + memory)
        el(
          "div",
          { class: "sys-section" },
          el(
            "div",
            { class: "sys-headline" },
            el(
              "div",
              { class: "sys-headline-card", id: "sd-cpu-card" },
              el("div", { class: "sys-headline-label" }, "CPU"),
              el("div", { class: "sys-headline-value", id: "sd-cpu-val" }, "—"),
              el("div", { class: "sys-headline-sub", id: "sd-cpu-sub" }, "—")
            ),
            el(
              "div",
              { class: "sys-headline-card", id: "sd-mem-card" },
              el("div", { class: "sys-headline-label" }, "Memory"),
              el("div", { class: "sys-headline-value", id: "sd-mem-val" }, "—"),
              el("div", { class: "sys-headline-sub", id: "sd-mem-sub" }, "—")
            )
          )
        ),
        // Per-core grid
        el(
          "div",
          { class: "sys-section" },
          el("div", { class: "sys-section-title" }, "Per-core CPU"),
          el("div", { class: "sys-cores-grid", id: "sd-cores" })
        ),
        // Swap usage row
        el(
          "div",
          { class: "sys-section" },
          el("div", { class: "sys-section-title" }, "Swap"),
          el(
            "div",
            { class: "sys-bar-row" },
            el("div", { class: "lbl" }, "Swap"),
            el(
              "div",
              { class: "sys-bar-track" },
              el("div", { class: "sys-bar-fill", id: "sd-swap-fill", style: "width:0%" })
            ),
            el("div", { class: "pct", id: "sd-swap-pct" }, "—")
          ),
          el("div", { class: "sys-headline-sub", id: "sd-swap-sub", style: "margin-top:4px;" }, "—")
        ),
        // FFmpeg processes
        el(
          "div",
          { class: "sys-section" },
          el("div", { class: "sys-section-title" }, "FFmpeg processes"),
          el(
            "table",
            { class: "sys-proc-table" },
            el(
              "thead",
              null,
              el(
                "tr",
                null,
                el("th", null, "PID"),
                el("th", null, "CPU"),
                el("th", null, "Memory"),
                el("th", null, "Command")
              )
            ),
            el("tbody", { id: "sd-procs" })
          )
        ),
        // Available video encoders (HW-accel inventory)
        el(
          "div",
          { class: "sys-section" },
          el("div", { class: "sys-section-title" }, "Available video encoders"),
          el(
            "div",
            { class: "sys-encoder-chips", id: "sd-encoders" },
            el("span", { class: "sys-encoder-chip" }, "loading…")
          )
        ),
        // Log tail
        el(
          "div",
          { class: "sys-section" },
          el(
            "div",
            { class: "sys-section-title", style: "display:flex;justify-content:space-between;align-items:center;" },
            el("span", null, "Recent FFmpeg log"),
            el(
              "button",
              {
                class: "sys-drawer-close",
                style: "padding:2px 6px;font-size:10px;",
                onclick: () => refreshLog(),
              },
              "↻"
            )
          ),
          el("div", { class: "sys-log-tail", id: "sd-log-tail" }, "click ↻ to load")
        )
      )
    );
    document.body.appendChild(scrim);
    document.body.appendChild(drawer);
    scrim.addEventListener("click", () => hide());

    let pollHandle = null;

    function show() {
      drawer.classList.add("open");
      drawer.setAttribute("aria-hidden", "false");
      scrim.classList.add("open");
      if (!pollHandle) {
        pollOnce();
        pollHandle = setInterval(pollOnce, POLL_INTERVAL_MS);
      }
      refreshLog();
      refreshEncoders();
    }

    function hide() {
      drawer.classList.remove("open");
      drawer.setAttribute("aria-hidden", "true");
      scrim.classList.remove("open");
      if (pollHandle) {
        clearInterval(pollHandle);
        pollHandle = null;
      }
    }

    async function pollOnce() {
      try {
        const r = await fetch("/api/system-health", { cache: "no-store" });
        if (!r.ok) return;
        const data = await r.json();
        render(data);
      } catch (e) {
        // Silent
      }
    }

    async function refreshLog() {
      const tailEl = document.getElementById("sd-log-tail");
      if (!tailEl) return;
      try {
        const url = opts.getLogUrl ? opts.getLogUrl() : "/api/log";
        const r = await fetch(url, { cache: "no-store" });
        if (!r.ok) {
          tailEl.textContent = `(/api/log → HTTP ${r.status})`;
          return;
        }
        const data = await r.json();
        const lines = (data.lines || data.log || []).slice(-30);
        tailEl.textContent = lines.length ? lines.join("\n") : "(no log lines)";
        // Scroll to bottom
        tailEl.scrollTop = tailEl.scrollHeight;
      } catch (e) {
        tailEl.textContent = "(error loading log: " + (e && e.message) + ")";
      }
    }

    async function refreshEncoders() {
      const chipsEl = document.getElementById("sd-encoders");
      if (!chipsEl) return;
      try {
        // /api/state has available_encoders (alpha.25).
        const r = await fetch("/api/state", { cache: "no-store" });
        if (!r.ok) return;
        const s = await r.json();
        const enc = s.available_encoders || s.snapshot?.available_encoders || [];
        if (!enc.length) {
          chipsEl.innerHTML = "";
          chipsEl.appendChild(
            el("span", { class: "sys-encoder-chip" }, "no encoders probed")
          );
          return;
        }
        const hwPattern = /(nvenc|qsv|videotoolbox|amf|cuvid|d3d11va|vaapi|mediafoundation)/i;
        chipsEl.innerHTML = "";
        for (const e of enc) {
          chipsEl.appendChild(
            el(
              "span",
              { class: "sys-encoder-chip" + (hwPattern.test(e) ? " hw" : "") },
              e
            )
          );
        }
      } catch (e) {
        // silent
      }
    }

    function render(data) {
      const status = data.status || "unknown";
      const cpu = Math.round(data.cpu_percent || 0);
      const memUsed = data.mem_used_mb || 0;
      const memTotal = data.mem_total_mb || 0;
      const memPct = Math.round(data.mem_percent || 0);
      const swapUsed = data.swap_used_mb || 0;
      const swapTotal = data.swap_total_mb || 0;
      const swapPct = swapTotal > 0 ? Math.round((swapUsed / swapTotal) * 100) : 0;
      const cpuCount = data.cpu_count || data.cpu_per_core?.length || 0;
      const cpuKlass = statusKlass(status);

      const cpuCard = document.getElementById("sd-cpu-card");
      cpuCard.className = "sys-headline-card " + cpuKlass;
      document.getElementById("sd-cpu-val").textContent = cpu + "%";
      document.getElementById("sd-cpu-sub").textContent = cpuCount + " cores";

      const memCard = document.getElementById("sd-mem-card");
      memCard.className = "sys-headline-card";
      if (memPct >= 85) memCard.classList.add("red");
      else if (memPct >= 70) memCard.classList.add("yellow");
      else memCard.classList.add("green");
      document.getElementById("sd-mem-val").textContent = memPct + "%";
      document.getElementById("sd-mem-sub").textContent =
        (memUsed / 1024).toFixed(1) + " / " + (memTotal / 1024).toFixed(1) + " GB";

      // Per-core grid
      const coresEl = document.getElementById("sd-cores");
      const cores = data.cpu_per_core || [];
      coresEl.innerHTML = "";
      cores.forEach((pct, i) => {
        const v = Math.round(pct);
        const cell = el(
          "div",
          { class: "sys-core-cell" + (v >= 90 ? " hot" : v >= 70 ? " warn" : "") },
          el("div", { class: "sys-core-fill", style: "height:" + Math.max(0, Math.min(100, v)) + "%" }),
          el("div", { class: "cnum" }, "c" + i),
          el("span", { class: "cval" }, v + "%")
        );
        coresEl.appendChild(cell);
      });

      // Swap
      const swapFillEl = document.getElementById("sd-swap-fill");
      swapFillEl.style.width = swapPct + "%";
      document.getElementById("sd-swap-pct").textContent = swapPct + "%";
      document.getElementById("sd-swap-sub").textContent =
        (swapUsed / 1024).toFixed(2) + " GB used of " + (swapTotal / 1024).toFixed(1) + " GB" +
        (swapPct > 30 ? " — paging activity may degrade encoder performance" : "");

      // FFmpeg processes
      const procsBody = document.getElementById("sd-procs");
      procsBody.innerHTML = "";
      const procs = data.ffmpeg_processes || [];
      if (procs.length === 0) {
        const tr = el(
          "tr",
          null,
          el("td", { colspan: "4", class: "sys-proc-empty" }, "No ffmpeg processes running")
        );
        procsBody.appendChild(tr);
      } else {
        for (const p of procs) {
          const cpuKlassP = p.cpu_percent >= 200 ? "color:#e85c5c" :
                          p.cpu_percent >= 100 ? "color:#f0b84f" : "color:#5fd06b";
          procsBody.appendChild(
            el(
              "tr",
              null,
              el("td", { class: "pid" }, p.pid),
              el("td", { class: "cpu", style: cpuKlassP }, p.cpu_percent.toFixed(0) + "%"),
              el("td", { class: "pid" }, p.mem_mb + " MB"),
              el("td", { class: "cmd", title: p.cmd_excerpt }, p.cmd_excerpt || "—")
            )
          );
        }
      }
    }

    // Wire the anchor element to toggle drawer.
    if (opts.anchorEl) {
      opts.anchorEl.style.cursor = "pointer";
      opts.anchorEl.addEventListener("click", () => {
        if (drawer.classList.contains("open")) hide();
        else show();
      });
    }

    // Keyboard: Esc closes
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && drawer.classList.contains("open")) hide();
    });

    return { show, hide };
  }

  // Public API
  window.createSystemDrawer = buildDrawer;
})();
