// alpha.16 monitor window — companion to the main configuration UI.
// Polls /api/state for status + bitrate + source/dest labels and
// /api/preview for the live JPEG. Stays small; the operator
// positions one of these per running instance on their screen to
// build a video-switcher-style multiview output.

(function () {
  'use strict';

  // Poll intervals — preview slightly faster than state since it's
  // the visual hook. Both are local-network fetches against our own
  // embedded Axum server, so cost is trivial.
  const STATE_POLL_MS = 1000;
  const PREVIEW_POLL_MS = 500;

  const statusEl   = document.getElementById('mon-status');
  const bitrateEl  = document.getElementById('mon-bitrate');
  const sourceEl   = document.getElementById('mon-source');
  const destEl     = document.getElementById('mon-dest');
  const previewEl  = document.getElementById('mon-preview');
  const placeholderEl = document.getElementById('mon-preview-placeholder');
  const expandBtn  = document.getElementById('mon-expand');

  // -----------------------------------------------------------------
  // State polling
  // -----------------------------------------------------------------

  // Returns one of: 'idle' | 'connecting' | 'streaming' | 'interrupted'.
  // Mirrors the streamer.rs status taxonomy (lowercased for CSS classes).
  function normalizeStatus(raw) {
    const s = (raw || '').toLowerCase();
    if (s === 'streaming') return 'streaming';
    if (s === 'connecting') return 'connecting';
    if (s === 'interrupted') return 'interrupted';
    return 'idle';
  }

  function statusLabel(status) {
    if (status === 'streaming') return 'Live';
    if (status === 'connecting') return 'Connecting';
    if (status === 'interrupted') return 'Interrupted';
    return 'Idle';
  }

  // Truncate long URLs to fit the small monitor footer. Keep the
  // scheme + host + a hint of the path so the operator can tell
  // distinct destinations apart at a glance.
  function shortenUrl(url, max) {
    if (!url) return '—';
    if (url.length <= max) return url;
    // Prefer keeping the head + tail with ellipsis in the middle so
    // both the scheme and the unique stream-key suffix survive.
    const head = Math.ceil((max - 1) / 2);
    const tail = Math.floor((max - 1) / 2);
    return url.slice(0, head) + '…' + url.slice(url.length - tail);
  }

  function describeSource(snap) {
    const id = snap.source_id || '';
    if (id === 'ndi') return `NDI: ${snap.ndi_source_name || '—'}`;
    if (id === 'omt') return `OMT: ${snap.omt_source_name || '—'}`;
    if (id === 'avfoundation') return `Camera: ${snap.av_video_name || `[${snap.av_video_index}]`}`;
    if (id === 'dshow') return `Camera: ${snap.av_video_name || `[${snap.av_video_index}]`}`;
    if (id === 'srt_listen') return `SRT receiver`;
    if (id === 'rtmp_listen') return `RTMP receiver`;
    if (id === 'pipe') return `Pipe / URL`;
    if (id === 'test_pattern') return `Test pattern`;
    if (id === 'gdigrab' || id === 'avf_screen') return `Screen capture`;
    return id || '—';
  }

  function describeDest(snap) {
    // Prefer the active resolved URL the streamer is actually using;
    // fall back to the custom override the user typed in if no
    // service XML is loaded.
    const url = snap.current_url || snap.custom_url || '';
    return shortenUrl(url, 38);
  }

  let lastState = null;

  async function pollState() {
    try {
      const r = await fetch('/api/state');
      if (!r.ok) return;
      const data = await r.json();
      // /api/state envelope shape (alpha.13+): { snapshot, preview }.
      // Older shape had the snapshot at top level; handle both.
      const snap = data.snapshot || data;
      const stats = snap.stats || {};
      lastState = snap;

      // Status pill — update class + label.
      const status = normalizeStatus(stats.status);
      statusEl.textContent = statusLabel(status);
      statusEl.className = `mon-status mon-status-${status}`;

      // Bitrate — kbps. Streamer reports raw bps; round to a clean
      // integer. Marked stale (greyed) when not actively streaming
      // so the operator doesn't mistake a leftover number for a
      // current reading.
      const kbps = Math.round((stats.bitrate || 0) / 1000);
      if (kbps > 0 && status === 'streaming') {
        bitrateEl.textContent = `${kbps.toLocaleString()} kbps`;
        bitrateEl.classList.remove('is-stale');
      } else if (status === 'connecting') {
        bitrateEl.textContent = 'Connecting…';
        bitrateEl.classList.add('is-stale');
      } else {
        bitrateEl.textContent = '— kbps';
        bitrateEl.classList.add('is-stale');
      }

      // Source + destination labels.
      sourceEl.textContent = describeSource(snap);
      sourceEl.title = describeSource(snap); // full text on hover
      const dest = describeDest(snap);
      destEl.textContent = dest;
      destEl.title = snap.current_url || snap.custom_url || '';

      // Update window title — useful when several monitors are open
      // because the OS task switcher / Dock shows them by title.
      const winTitle = `${statusLabel(status)} · ${describeSource(snap)} — Monitor`;
      if (document.title !== winTitle) {
        document.title = winTitle;
      }
    } catch (_) {
      // Backend down — leave the last good values visible. The next
      // poll will recover if it comes back.
    }
  }

  // -----------------------------------------------------------------
  // Preview polling
  // -----------------------------------------------------------------

  // Cache-bust each fetch with a query param so the WebView doesn't
  // serve a stale frame from its image cache.
  function previewUrl() {
    return `/api/preview?_=${Date.now()}`;
  }

  // Track whether the preview img has ever loaded successfully so we
  // can show/hide the placeholder cleanly.
  let previewVisible = false;

  function showPreview() {
    if (!previewVisible) {
      previewEl.classList.add('is-visible');
      placeholderEl.classList.add('is-hidden');
      previewVisible = true;
    }
  }
  function hidePreview() {
    if (previewVisible) {
      previewEl.classList.remove('is-visible');
      placeholderEl.classList.remove('is-hidden');
      previewVisible = false;
    }
  }

  function refreshPreview() {
    // HEAD-then-GET would let us skip the load cost on 404, but the
    // /api/preview endpoint returns 200 with image bytes when a
    // preview is available and 404 otherwise. So just set src and
    // let the img.onload / .onerror handlers do the work.
    const url = previewUrl();
    // Use a transient Image to test before swapping into the visible
    // <img>, avoiding a flash of the broken-image icon between polls.
    const test = new Image();
    test.onload = () => {
      previewEl.src = url;
      showPreview();
    };
    test.onerror = () => {
      hidePreview();
    };
    test.src = url;
  }

  // -----------------------------------------------------------------
  // Expand button — Tauri command to focus the main window
  // -----------------------------------------------------------------

  if (expandBtn) {
    expandBtn.addEventListener('click', async () => {
      const invoke = window.__TAURI__?.core?.invoke;
      if (!invoke) {
        // Browser preview (no Tauri runtime) — fall back to alerting
        // the user. Production builds always have Tauri injected
        // via withGlobalTauri=true in tauri.conf.json.
        alert('Expand requires the Tauri runtime — main window is in the same app.');
        return;
      }
      try {
        await invoke('focus_main_window');
      } catch (err) {
        console.warn('focus_main_window failed:', err);
      }
    });
  }

  // -----------------------------------------------------------------
  // Boot
  // -----------------------------------------------------------------

  pollState();
  refreshPreview();
  setInterval(pollState, STATE_POLL_MS);
  setInterval(refreshPreview, PREVIEW_POLL_MS);
})();
