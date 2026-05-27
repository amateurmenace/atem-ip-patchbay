// alpha.15 multiview shell — handles layout toggle, fleet-level
// status polling, and the "+ New Window" spawn-instance button.
//
// Each tile is an iframe loading /static/index.html?tile=N. The
// shell polls each tile's /api/i/N/state independently to drive
// the per-tile chrome (label color + status text) so the operator
// can see at a glance which tiles are live without zooming into
// each iframe.

(function () {
  'use strict';

  const TILE_COUNT = 4;
  const STATUS_POLL_INTERVAL_MS = 2000;
  const LAYOUT_KEY = 'atem-multiview-layout';

  const body = document.body;
  const layoutToggle = document.getElementById('mv-layout-toggle');
  const spawnBtn = document.getElementById('mv-spawn');
  const tilesStatusEl = document.getElementById('mv-tiles-status');

  // ---------- Layout toggle ----------------------------------------

  function readInitialLayout() {
    // URL param wins (one-shot override), then localStorage, then
    // default to grid. ?layout=single in the URL is useful for
    // demos / screenshots / debugging without flipping the saved
    // preference.
    const params = new URLSearchParams(window.location.search);
    const urlLayout = params.get('layout');
    if (urlLayout === 'single' || urlLayout === 'grid') {
      return urlLayout;
    }
    try {
      const stored = localStorage.getItem(LAYOUT_KEY);
      if (stored === 'single' || stored === 'grid') return stored;
    } catch (_) {
      // Private mode / disabled storage — fall through to default.
    }
    return 'grid';
  }

  function applyLayout(layout) {
    if (layout === 'single') {
      body.classList.add('layout-single');
      if (layoutToggle) layoutToggle.textContent = 'Multi-View';
    } else {
      body.classList.remove('layout-single');
      if (layoutToggle) layoutToggle.textContent = 'Single View';
    }
    try {
      localStorage.setItem(LAYOUT_KEY, layout);
    } catch (_) { /* private mode */ }
  }

  applyLayout(readInitialLayout());

  if (layoutToggle) {
    layoutToggle.addEventListener('click', () => {
      const current = body.classList.contains('layout-single') ? 'single' : 'grid';
      applyLayout(current === 'single' ? 'grid' : 'single');
    });
  }

  // ---------- "+ New Window" — spawn a separate process -----------

  if (spawnBtn) {
    spawnBtn.addEventListener('click', async () => {
      // Tauri 2's invoke API lives at __TAURI__.core.invoke. When
      // we're running in cargo tauri dev or a bundled .app, this
      // is present. In a plain browser (testing the UI without
      // Tauri) it's not — fall back to instructions for the
      // manual CLI launcher.
      const invoke = window.__TAURI__?.core?.invoke;
      if (!invoke) {
        alert(
          'Spawn Window requires the Tauri runtime.\n\n' +
          'To launch another instance from a terminal:\n' +
          '  open -n /Applications/ATEM\\ IP\\ Patchbay.app --args --instance-name two'
        );
        return;
      }
      // Default instance name suggestion — short timestamp suffix
      // so back-to-back spawns get distinct names without the
      // user having to invent a label every time.
      const stamp = Date.now().toString(36).slice(-4);
      const name = prompt(
        'Instance name? (each instance is its own window with own state + ports)',
        `instance-${stamp}`
      );
      if (!name) return;
      try {
        await invoke('spawn_instance', { name: name.trim() });
      } catch (err) {
        alert('Failed to spawn instance: ' + (err && err.message || err));
      }
    });
  }

  // ---------- Per-tile status polling ------------------------------

  // Map snapshot status string to (CSS class, friendly label). The
  // status values come from streamer.rs's heuristic state machine —
  // see CLAUDE.md "Streamer status flow" for the canonical list.
  function statusInfo(raw) {
    const s = (raw || '').toLowerCase();
    if (s === 'streaming') return { cls: 'is-streaming', label: 'Live' };
    if (s === 'connecting') return { cls: 'is-connecting', label: 'Connecting…' };
    if (s === 'interrupted') return { cls: 'is-interrupted', label: 'Interrupted' };
    return { cls: '', label: 'Idle' };
  }

  async function pollTileStatus(idx) {
    try {
      const r = await fetch(`/api/i/${idx}/state`);
      if (!r.ok) return null;
      const snap = await r.json();
      // /api/state returns { snapshot: {...}, preview: {...} } in
      // alpha.13+ shape; older versions returned the snapshot at
      // the top level. Handle both.
      const stats = (snap.snapshot?.stats) || snap.stats || {};
      const info = statusInfo(stats.status);
      const el = document.getElementById(`mv-status-${idx}`);
      if (el) {
        el.textContent = info.label;
        el.className = `mv-tile-status ${info.cls}`;
      }
      const tileEl = document.querySelector(`.mv-tile[data-tile="${idx}"]`);
      if (tileEl) {
        tileEl.dataset.status = info.cls === 'is-streaming' ? 'live' : 'idle';
      }
      return info;
    } catch (_) {
      return null;
    }
  }

  async function pollAllTiles() {
    const results = await Promise.all(
      Array.from({ length: TILE_COUNT }, (_, i) => pollTileStatus(i))
    );
    // Aggregate: "2 tiles live, 1 connecting, 1 idle" — at-a-glance
    // fleet summary in the topbar.
    const counts = { 'Live': 0, 'Connecting…': 0, 'Interrupted': 0, 'Idle': 0 };
    for (const info of results) {
      if (info) counts[info.label] = (counts[info.label] || 0) + 1;
    }
    if (tilesStatusEl) {
      const parts = [];
      if (counts['Live'])        parts.push(`${counts['Live']} live`);
      if (counts['Connecting…']) parts.push(`${counts['Connecting…']} connecting`);
      if (counts['Interrupted']) parts.push(`${counts['Interrupted']} interrupted`);
      if (counts['Idle'])        parts.push(`${counts['Idle']} idle`);
      tilesStatusEl.textContent = parts.length ? parts.join(' · ') : 'fleet ready';
    }
  }

  pollAllTiles();
  setInterval(pollAllTiles, STATUS_POLL_INTERVAL_MS);
})();
