// ATEM IP Patchbay — control panel script.
// Polls /api/state every second, drives forms, runs live source preview
// in the browser via getUserMedia/getDisplayMedia.

const $  = (sel) => document.querySelector(sel);
const $$ = (sel) => Array.from(document.querySelectorAll(sel));

// -----------------------------------------------------------------
// Element refs
// -----------------------------------------------------------------
const els = {
  body: document.body,

  // Top nav
  brandBtn:   $('#brand-btn'),
  statusPill: $('#status-pill'),
  duration:   $('#duration'),
  monitorAux: $('#monitor-aux'),
  destAux:    $('#dest-aux'),
  connAux:    $('#conn-aux'),

  // Monitor
  previewFrame:   $('#preview-frame'),
  previewVideo:   $('#preview-video'),
  previewBars:    $('#preview-bars'),
  previewMessage: $('#preview-message'),
  liveBadge:      $('#live-badge'),
  ovlSource:      $('#ovl-source'),
  ovlRes:         $('#ovl-res'),
  ovlProfile:     $('#ovl-profile'),
  ovlBitrate:     $('#ovl-bitrate'),
  startBtn:       $('#btn-start'),
  stopBtn:        $('#btn-stop'),
  error:          $('#error'),

  // Telemetry
  tmBitrate:    $('#tm-bitrate'),
  tmBitrateBar: $('#tm-bitrate-bar'),
  tmFps:        $('#tm-fps'),
  tmFpsTarget:  $('#tm-fps-target'),
  tmSpeed:      $('#tm-speed'),
  tmSpeedNote:  $('#tm-speed-note'),
  tmFrames:     $('#tm-frames'),
  tmQuality:    $('#tm-quality'),
  tmDropped:    $('#tm-dropped'),
  tmDuration:   $('#tm-duration'),
  tmElapsed:    $('#tm-elapsed'),

  // Log
  log: $('#log'),
  cmd: $('#cmd'),

  // Source
  sourceTiles:    $('#source-tiles'),
  sourceHint:     $('#source-hint'),
  avAudio:        $('#av-audio'),
  videoMode:      $('#video-mode'),
  pipeOnly:       $$('.pipe-only'),
  pipePath:       $('#pipe-path'),
  rescanDevices:  $('#rescan-devices'),
  ndiRescan:      $('#ndi-rescan'),

  // Relay (incoming SRT/RTMP server)
  relayPanels:        $$('.relay-only'),
  relaySrtUrl:        $('#relay-srt-url'),
  relaySrtCopy:       $('#relay-srt-copy'),
  relaySrtPort:       $('#relay-srt-port'),
  relaySrtLatency:    $('#relay-srt-latency'),
  relaySrtPassphrase: $('#relay-srt-passphrase'),
  relayRtmpUrl:       $('#relay-rtmp-url'),
  relayRtmpCopy:      $('#relay-rtmp-copy'),
  relayRtmpPort:      $('#relay-rtmp-port'),
  relayRtmpApp:       $('#relay-rtmp-app'),
  relayRtmpKey:       $('#relay-rtmp-key'),

  // Destination wizard
  destAddress:    $('#dest-address'),
  destAux:        $('#dest-aux'),
  formatDecoded:  $('#format-decoded'),
  service:     $('#service'),
  server:      $('#server'),
  multiServiceRow: $('#multi-service-row'),
  destUrl:     $('#dest-url'),
  streamKey:   $('#stream-key'),
  passphrase:  $('#passphrase'),
  streamid:    $('#streamid'),
  rtmpUrl:     $('#rtmp-url'),
  srtOnly:     $$('.srt-only'),
  rtmpOnly:    $$('.rtmp-only'),
  protoSegs:   $$('input[name="dest-proto"]'),
  codecSegs:   $$('input[name="dest-codec"]'),

  // Paste (now inside Advanced)
  pasteText:   $('#paste-text'),
  pasteApply:  $('#paste-apply'),
  pasteClear:  $('#paste-clear'),
  pasteStatus: $('#paste-status'),

  // XML drop (in wizard)
  xmlDrop:        $('#xml-drop'),
  xmlFile:        $('#xml-file'),
  xmlLoaded:      $('#xml-loaded'),
  xmlLoadedName:  $('#xml-loaded-name'),
  xmlClear:       $('#xml-clear'),
  xmlStatus:      $('#xml-status'),

  // LAN discover
  lanDiscover:     $('#lan-discover'),
  discoverResults: $('#discover-results'),

  // Encoder
  // videoCodec used to be a <select id="video-codec"> in the old Encoder
  // card. The wizard replaced it with the codecSegs segmented control.
  quality:    $('#quality'),
  qualitySeg: $('#quality-seg'),
  label:      $('#label'),

  // Overlay
  ovTitle:    $('#ov-title'),
  ovSubtitle: $('#ov-subtitle'),
  ovLogo:     $('#ov-logo'),
  ovClock:    $('#ov-clock'),

  // SRT advanced
  srtMode:          $('#srt-mode'),
  srtLatency:       $('#srt-latency'),
  srtListenPort:    $('#srt-listen-port'),
  srtListenerOnly:  $$('.srt-listener-only'),
  streamidOverride: $('#streamid-override'),
  streamidLegacy:   $('#streamid-legacy'),
};

let lastSnapshot   = null;
let knownDevices   = { video: [], audio: [] };
let knownNdi       = [];                 // discovered NDI senders
let browserDevices = [];                 // navigator.mediaDevices results
let activeStream   = null;               // current MediaStream in preview
let previewKey     = '';                 // de-dupe preview switches
let perms          = { granted: false, prompted: false };
let lanIp          = '';                 // populated from /api/lan-ip on init

// -----------------------------------------------------------------
// SVG icons
// -----------------------------------------------------------------
const ICONS = {
  test_pattern: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><rect x="3" y="5" width="18" height="14" rx="1.5"/><path d="M7 5v14M11 5v14M15 5v14M19 5v14"/></svg>`,
  camera:       `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M4 8h3l1.5-2h7L17 8h3a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V9a1 1 0 0 1 1-1z"/><circle cx="12" cy="13.5" r="3.5"/></svg>`,
  capture_card: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><rect x="3" y="6" width="18" height="12" rx="1.5"/><circle cx="7" cy="12" r="1.2" fill="currentColor"/><circle cx="11" cy="12" r="1.2" fill="currentColor"/><circle cx="15" cy="12" r="1.2" fill="currentColor"/><path d="M19 10.5v3"/></svg>`,
  screen:       `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><rect x="3" y="4" width="18" height="13" rx="1.5"/><path d="M9 21h6M12 17v4"/></svg>`,
  ndi:          `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18"/></svg>`,
  iphone:       `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><rect x="7" y="2" width="10" height="20" rx="2"/><path d="M11 18h2"/></svg>`,
  virtual:      `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M4 12c0-4 3.5-7 8-7s8 3 8 7-3.5 7-8 7-8-3-8-7z"/><path d="M9 12h.01M15 12h.01M9.5 15c.8.6 1.7 1 2.5 1s1.7-.4 2.5-1"/></svg>`,
  pipe:         `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M10 14l-3 3a3 3 0 1 1-4-4l3-3M14 10l3-3a3 3 0 1 1 4 4l-3 3M8 16l8-8"/></svg>`,
  relay:        `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M3 12h6l2-3 2 6 2-3h6"/><circle cx="3" cy="12" r="1.4" fill="currentColor"/><circle cx="21" cy="12" r="1.4" fill="currentColor"/></svg>`,
};

const CATEGORY_LABEL = {
  test_pattern: 'Test',
  camera:       'Camera',
  capture_card: 'Capture',
  screen:       'Screen',
  ndi:          'NDI',
  iphone:       'iPhone',
  virtual:      'Virtual',
  pipe:         'URL / Pipe',
  relay:        'Receive',
};

// -----------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------
function setOptions(select, items, current) {
  const desired = items.map((v) => (typeof v === 'string' ? v : String(v.value))).join('|');
  const have = Array.from(select.options).map((o) => o.value).join('|');
  if (desired !== have) {
    select.innerHTML = '';
    for (const it of items) {
      const opt = document.createElement('option');
      if (typeof it === 'string') {
        opt.value = it;
        opt.textContent = it;
      } else {
        opt.value = String(it.value);
        opt.textContent = it.label;
      }
      select.appendChild(opt);
    }
  }
  if (current !== undefined && current !== null && String(select.value) !== String(current)) {
    select.value = String(current);
  }
}

function escapeHtml(s) {
  return String(s == null ? '' : s)
    .replace(/&/g, '&amp;').replace(/</g, '&lt;')
    .replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function buildStreamidPreview(snap) {
  const key = snap.stream_key || '';
  const name = (snap.label || 'Streaming Encoder').replace(/[,=]/g, ' ');
  const uuid = snap.device_uuid || '';
  if (snap.streamid_legacy) return `#!::r=${key},m=publish,bmd_uuid=${uuid},bmd_name=${name}`;
  return `#!::bmd_uuid=${uuid},bmd_name=${name},u=${key}`;
}

function buildRtmpPreview(snap) {
  const base = (snap.current_url || '').replace(/\/$/, '');
  if (!base) return '—';
  if (!snap.stream_key) return base;
  if (base.endsWith('/' + snap.stream_key)) return base;
  return `${base}/${snap.stream_key}`;
}

function applyProtocolVisibility(protocol) {
  const isSrt = protocol === 'srt';
  const isRtmp = protocol === 'rtmp' || protocol === 'rtmps';
  els.srtOnly.forEach((e) => (e.hidden = !isSrt));
  els.rtmpOnly.forEach((e) => (e.hidden = !isRtmp));
}

function applySrtModeVisibility(mode) {
  els.srtListenerOnly.forEach((e) => (e.hidden = mode !== 'listener'));
}

async function fetchJSON(url, opts = {}) {
  const r = await fetch(url, opts);
  return r.json();
}

async function applySettings(patch) {
  try {
    const snap = await fetchJSON('/api/settings', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(patch),
    });
    render(snap);
  } catch (_e) { /* ignore */ }
}

// -----------------------------------------------------------------
// Browser-side device permission + enumeration
// -----------------------------------------------------------------
async function ensureBrowserDevicePerms() {
  // The first call to enumerateDevices() returns devices with empty labels
  // until the user grants media permission once. We open a tiny audio-only
  // stream to trigger the permission prompt, then immediately stop it.
  if (perms.granted || perms.prompted) return;
  perms.prompted = true;
  try {
    const s = await navigator.mediaDevices.getUserMedia({ audio: true, video: false });
    s.getTracks().forEach((t) => t.stop());
    perms.granted = true;
  } catch (_e) {
    // User declined; we'll show a hint instead of preview.
    perms.granted = false;
  }
}

async function refreshBrowserDevices() {
  try {
    if (!navigator.mediaDevices || !navigator.mediaDevices.enumerateDevices) return;
    browserDevices = await navigator.mediaDevices.enumerateDevices();
  } catch (_e) {
    browserDevices = [];
  }
}

function findBrowserDeviceByName(name) {
  if (!name) return null;
  // Try exact match, then case-insensitive substring match.
  const lower = name.toLowerCase();
  let exact = browserDevices.find((d) => d.kind === 'videoinput' && d.label === name);
  if (exact) return exact;
  return browserDevices.find((d) => d.kind === 'videoinput' && d.label.toLowerCase().includes(lower)) || null;
}

// -----------------------------------------------------------------
// Live preview management
// -----------------------------------------------------------------
function stopPreview() {
  if (activeStream) {
    activeStream.getTracks().forEach((t) => t.stop());
    activeStream = null;
  }
  if (ndiPreviewTimer) {
    clearInterval(ndiPreviewTimer);
    ndiPreviewTimer = null;
  }
  if (ndiPreviewImg) {
    if (ndiPreviewImg.dataset.objectUrl) {
      URL.revokeObjectURL(ndiPreviewImg.dataset.objectUrl);
      delete ndiPreviewImg.dataset.objectUrl;
    }
    ndiPreviewImg.src = '';
    ndiPreviewImg.hidden = true;
  }
  els.previewVideo.srcObject = null;
  els.previewVideo.hidden = true;
  els.previewBars.hidden = false;
  els.previewMessage.hidden = true;
}

function showPreviewMessage(html) {
  stopPreview();
  els.previewMessage.innerHTML = html;
  els.previewMessage.hidden = false;
  els.previewBars.hidden = true;
}

async function startCameraPreview(deviceLabel) {
  await ensureBrowserDevicePerms();
  if (!perms.granted) {
    showPreviewMessage(`<div><strong>Live preview needs media permission.</strong>Allow camera/microphone access for this page in your browser to see the live source preview here.</div>`);
    return;
  }
  await refreshBrowserDevices();
  const dev = findBrowserDeviceByName(deviceLabel);
  if (!dev) {
    showPreviewMessage(`<div><strong>${escapeHtml(deviceLabel)}</strong>Not exposed to the browser as a webcam — usually true for capture cards and some virtual cameras. Streaming will still work via FFmpeg + AVFoundation, just no in-browser preview.</div>`);
    return;
  }
  const key = `cam:${dev.deviceId}`;
  if (key === previewKey) return;
  stopPreview();
  previewKey = key;
  try {
    activeStream = await navigator.mediaDevices.getUserMedia({
      video: { deviceId: { exact: dev.deviceId } },
      audio: false,
    });
    els.previewVideo.srcObject = activeStream;
    els.previewVideo.hidden = false;
    els.previewBars.hidden = true;
    els.previewMessage.hidden = true;
  } catch (e) {
    showPreviewMessage(`<div><strong>${escapeHtml(deviceLabel)}</strong>Couldn't open the device: ${escapeHtml(String(e.message || e))}.</div>`);
    previewKey = '';
  }
}

async function startScreenPreview() {
  if (!navigator.mediaDevices || !navigator.mediaDevices.getDisplayMedia) {
    showPreviewMessage(`<div><strong>Screen preview not available in this browser.</strong>Streaming will still work via AVFoundation when you press Start Stream.</div>`);
    return;
  }
  if (previewKey === 'screen') return;
  stopPreview();
  previewKey = 'screen';
  try {
    activeStream = await navigator.mediaDevices.getDisplayMedia({ video: true, audio: false });
    els.previewVideo.srcObject = activeStream;
    els.previewVideo.hidden = false;
    els.previewBars.hidden = true;
    els.previewMessage.hidden = true;
  } catch (e) {
    showPreviewMessage(`<div><strong>Screen capture cancelled.</strong>Click the Screen tile again to try again.</div>`);
    previewKey = '';
  }
}

function showTestPatternPreview() {
  if (previewKey === 'test') return;
  stopPreview();
  previewKey = 'test';
  // SMPTE bars are static CSS — just make sure they're showing.
  els.previewBars.hidden = false;
  els.previewVideo.hidden = true;
  els.previewMessage.hidden = true;
}

function showPipePreview(path) {
  showPreviewMessage(`<div><strong>${escapeHtml(path || 'URL / Pipe')}</strong>FFmpeg will read this when you press Start Stream. Browser-side preview isn't available for arbitrary pipes/URLs.</div>`);
  previewKey = `pipe:${path}`;
}

function showNdiHint(senderName) {
  showPreviewMessage(`
    <div>
      <strong>NDI sender: ${escapeHtml(senderName)}</strong>
      To consume an NDI source: open <em>NDI Tools → NDI Virtual Camera</em>, select
      <em>${escapeHtml(senderName)}</em> there, then pick the
      <strong>NDI Virtual Camera</strong> tile above. Direct NDI input requires FFmpeg
      compiled with <code>libndi_newtek</code>, which this build doesn't have.
    </div>`);
  previewKey = `ndi:${senderName}`;
  renderNdiSourceHint(senderName);
}

// Inline hint that lives right below the source-tile gallery — much
// more discoverable than the preview-area message in the left column,
// and exposes a one-click "use the bridge" button when the NDI Virtual
// Camera AVF device exists locally (which means NDI Tools is installed).
function renderNdiSourceHint(senderName) {
  const ndiVideo = knownDevices.video.find(
    (d) => /^ndi virtual (camera|input)$/i.test(d.name)
  );
  const ndiAudio = knownDevices.audio.find(
    (d) => /^ndi audio$/i.test(d.name)
  );

  let body;
  if (ndiVideo) {
    body = `
      <div class="ndi-hint-title">Bridge ${escapeHtml(senderName)} → this app</div>
      <ol class="ndi-hint-steps">
        <li>Open <strong>NDI Tools → NDI Virtual Camera</strong> in your menu bar.</li>
        <li>Set its source to <strong>${escapeHtml(senderName)}</strong>.</li>
        <li>Click the button below to switch this app to the NDI Virtual Camera input.</li>
      </ol>
      <button id="use-ndi-bridge" class="primary" type="button"
        data-video-index="${ndiVideo.index}"
        ${ndiAudio ? `data-audio-index="${ndiAudio.index}"` : ''}>
        Use NDI Virtual Camera${ndiAudio ? ' + NDI Audio' : ''}
      </button>`;
  } else {
    body = `
      <div class="ndi-hint-title">${escapeHtml(senderName)} is broadcasting on your network</div>
      <p>This build can't ingest NDI directly (FFmpeg lacks <code>libndi_newtek</code>).
      To use it, install <a href="https://ndi.video/tools/" target="_blank" rel="noopener">NDI
      Tools</a>, run <strong>NDI Virtual Camera</strong> from the menu bar pointed at
      <em>${escapeHtml(senderName)}</em>, then refresh the device list (the
      <a href="#" id="rescan-ndi-after-hint">rescan</a> link above the tiles) and pick
      the <strong>NDI Virtual Camera</strong> tile.</p>`;
  }

  els.sourceHint.innerHTML = body;
  els.sourceHint.hidden = false;
  els.sourceHint.classList.add('ndi-hint');

  const btn = document.getElementById('use-ndi-bridge');
  if (btn) {
    btn.addEventListener('click', () => {
      const v = parseInt(btn.dataset.videoIndex, 10);
      const patch = {
        source_id: 'avfoundation',
        av_video_index: v,
        av_video_name: ndiVideo.name,  // stable name beats AVF's shuffling indices
      };
      const a = parseInt(btn.dataset.audioIndex, 10);
      if (!isNaN(a)) {
        patch.av_audio_index = a;
        if (ndiAudio) patch.av_audio_name = ndiAudio.name;
      }
      applySettings(patch);
      // Clear the hint and switch the preview to the camera.
      els.sourceHint.hidden = true;
      const tile = { sourceId: 'avfoundation', category: 'ndi', name: ndiVideo.name };
      setPreviewFor(tile);
    });
  }
  const rescan = document.getElementById('rescan-ndi-after-hint');
  if (rescan) {
    rescan.addEventListener('click', (e) => {
      e.preventDefault();
      ensureDevicesLoaded(true);
    });
  }
}

// Decide which preview to show based on a clicked tile.
async function setPreviewFor(tile) {
  if (tile.sourceId === 'test_pattern') return showTestPatternPreview();
  if (tile.sourceId === 'pipe')         return showPipePreview(tile.name);
  if (tile.sourceId === 'ndi-sender')   return startNdiPreview(tile.name);
  if (tile.sourceId === 'ndi')          return startNdiPreview(tile.name);
  if (tile.sourceId === 'srt_listen' || tile.sourceId === 'rtmp_listen') {
    return showRelayWaiting(tile.sourceId);
  }
  if (tile.sourceId === 'avfoundation') {
    if (tile.category === 'screen') return startScreenPreview();
    return startCameraPreview(tile.name);
  }
}

// -----------------------------------------------------------------
// NDI native preview — polls /api/preview at ~2 Hz once an NDI
// source is active. The Rust NDI capture thread (Phase 7) samples
// every 15th frame, encodes JPEG via grafton-ndi, and stashes it;
// /api/preview returns the latest. Until streaming starts the
// endpoint returns 204 — we display the bridge-style waiting hint
// in that case.
// -----------------------------------------------------------------
let ndiPreviewTimer = null;
let ndiPreviewImg = null;

function startNdiPreview(senderName) {
  const key = `ndi:${senderName}`;
  if (previewKey === key) return;
  stopPreview();
  previewKey = key;

  if (!ndiPreviewImg) {
    ndiPreviewImg = document.createElement('img');
    ndiPreviewImg.id = 'preview-ndi';
    ndiPreviewImg.alt = 'NDI preview';
    // position:absolute so we overlay the SMPTE bars the same way
    // #preview-video does. Without this, the img would flow inline
    // and the bars would still be visible alongside it.
    ndiPreviewImg.style.cssText =
      'position:absolute;inset:0;width:100%;height:100%;object-fit:contain;background:#000;';
    els.previewFrame.appendChild(ndiPreviewImg);
  }
  ndiPreviewImg.hidden = false;
  els.previewBars.hidden = true;
  els.previewVideo.hidden = true;
  els.previewMessage.hidden = true;

  if (ndiPreviewTimer) clearInterval(ndiPreviewTimer);
  let nullStreak = 0;
  const tick = async () => {
    try {
      const r = await fetch(`/api/preview?ts=${Date.now()}`);
      if (r.status === 204 || !r.ok) {
        nullStreak += 1;
        // After ~6s without frames, swap to a "press Start Stream"
        // hint. Important: do NOT call showPreviewMessage() here —
        // that calls stopPreview() which would clear ndiPreviewTimer,
        // and then no more ticks would fire, so the user would never
        // see frames after they actually hit Start. Inline the DOM
        // updates so the polling loop keeps running underneath the
        // hint and can recover when the receiver comes up.
        if (nullStreak === 12 && ndiPreviewImg.hidden === false) {
          ndiPreviewImg.hidden = true;
          els.previewBars.hidden = true;
          els.previewMessage.hidden = false;
          els.previewMessage.innerHTML = `
            <div>
              <strong>NDI sender: ${escapeHtml(senderName)}</strong>
              Direct NDI ingest is selected — press <strong>Start Stream</strong>
              below to begin receiving. The first preview frame will appear
              here once the receiver connects.
            </div>`;
        }
        return;
      }
      nullStreak = 0;
      // Show the image again if the waiting-message fallback was rendered.
      if (ndiPreviewImg.hidden) {
        els.previewMessage.hidden = true;
        els.previewBars.hidden = true;
        ndiPreviewImg.hidden = false;
      }
      const blob = await r.blob();
      // Object URL avoids re-encoding the JPEG bytes through base64.
      const next = URL.createObjectURL(blob);
      const prev = ndiPreviewImg.dataset.objectUrl;
      ndiPreviewImg.src = next;
      if (prev) URL.revokeObjectURL(prev);
      ndiPreviewImg.dataset.objectUrl = next;
    } catch (_e) {
      // Network blip — try again on the next tick.
    }
  };
  // Kick off immediately + then every 500ms (~2 Hz). The Rust
  // sampler also does ~2 Hz, so the round-trip stays smooth.
  tick();
  ndiPreviewTimer = setInterval(tick, 500);
}

function showRelayWaiting(sid) {
  const proto = sid === 'srt_listen' ? 'SRT' : 'RTMP';
  showPreviewMessage(`
    <div>
      <strong>${proto} listener — waiting for a publisher</strong>
      Click <strong>Start Stream</strong> below to bind the listener. Then
      point your encoder (OBS, Larix, FFmpeg, an iPhone) at the publish URL
      shown in the Source panel. Live preview of incoming streams isn't
      available in the browser — once a publisher connects, watch the
      <em>Monitor</em> bitrate and FPS to confirm the stream is flowing.
    </div>`);
  previewKey = `relay:${sid}`;
}

// -----------------------------------------------------------------
// Source tile rendering
// -----------------------------------------------------------------
function buildSourceTiles(snap) {
  const tiles = [];

  tiles.push({
    sourceId: 'test_pattern', avIndex: null, name: 'Test Pattern',
    category: 'test_pattern', section: 'Test',
  });

  const groups = { camera: [], capture_card: [], screen: [], iphone: [], ndi: [], virtual: [] };
  for (const d of knownDevices.video) {
    const c = d.category || 'camera';
    (groups[c] || groups.camera).push(d);
  }
  const order = [
    ['camera',       'Cameras'],
    ['capture_card', 'Capture cards'],
    ['iphone',       'iPhone (Continuity)'],
    ['screen',       'Screens'],
    ['ndi',          'NDI Bridge (NDI Virtual Camera)'],
    ['virtual',      'Virtual cameras'],
  ];
  for (const [cat, label] of order) {
    for (const d of groups[cat]) {
      tiles.push({
        sourceId: 'avfoundation',
        avIndex: d.index,
        name: d.name,
        category: cat,
        section: label,
      });
    }
  }

  // NDI senders discovered via mDNS — these are NOT directly playable
  // (no libndi in FFmpeg), so they're informational tiles.
  for (const sender of knownNdi) {
    tiles.push({
      sourceId: 'ndi-sender',
      avIndex: null,
      name: `${sender.source || sender.name} · ${sender.machine || ''}`.replace(/\s·\s$/, ''),
      category: 'ndi',
      section: 'NDI senders on your network',
      discovered: true,
    });
  }

  tiles.push({
    sourceId: 'pipe', avIndex: null,
    name: snap.pipe_path ? snap.pipe_path.split('/').pop() : 'URL / Pipe',
    category: 'pipe', section: 'URL / Pipe',
  });

  // Relay listeners — turn this app into a server that an external
  // encoder publishes into.
  const r = snap.relay || {};
  tiles.push({
    sourceId: 'srt_listen', avIndex: null,
    name: `SRT in :${r.srt_port || 9710}`,
    category: 'relay', section: 'Receive a stream',
  });
  tiles.push({
    sourceId: 'rtmp_listen', avIndex: null,
    name: `RTMP in :${r.rtmp_port || 1935}`,
    category: 'relay', section: 'Receive a stream',
  });

  // Render with section headers.
  els.sourceTiles.innerHTML = '';
  let lastSection = null;
  // Pre-count tiles per section for the header counter.
  const counts = {};
  tiles.forEach((t) => { counts[t.section] = (counts[t.section] || 0) + 1; });

  for (const t of tiles) {
    if (t.section !== lastSection) {
      const sec = document.createElement('div');
      sec.className = 'tile-section';
      sec.innerHTML = `<span>${escapeHtml(t.section)}</span><span class="count">${counts[t.section]}</span>`;
      els.sourceTiles.appendChild(sec);
      lastSection = t.section;
    }
    // NDI tiles: state.source_id is "ndi" but the tile's sourceId is
    // "ndi-sender" (the discovery list); match by ndi_source_name so
    // the right sender within the discovered set highlights.
    const isActive =
      ((snap.source_id === t.sourceId) ||
       (snap.source_id === 'ndi' && t.sourceId === 'ndi-sender' && snap.ndi_source_name === t.name)) &&
      (t.sourceId !== 'avfoundation' || snap.av_video_index === t.avIndex);
    const div = document.createElement('div');
    div.className = 'tile' + (isActive ? ' active' : '') + (t.discovered ? ' discovered' : '');
    div.innerHTML = `
      <div class="tile-icon">${ICONS[t.category] || ICONS.camera}</div>
      <div class="tile-name" title="${escapeHtml(t.name)}">${escapeHtml(t.name)}</div>
      <div class="tile-cat">${CATEGORY_LABEL[t.category] || t.category}</div>
    `;
    div.addEventListener('click', () => selectSource(t));
    els.sourceTiles.appendChild(div);
  }
}

function selectSource(t) {
  if (t.sourceId === 'ndi-sender') {
    // Phase 4+: discovered NDI senders are now first-class direct-
    // ingest sources. Switch FFmpeg to source_id="ndi" with the
    // sender name; the preview poller will pick up frames from
    // /api/preview once Start Stream starts the receiver.
    applySettings({ source_id: 'ndi', ndi_source_name: t.name });
    setPreviewFor({ ...t, sourceId: 'ndi' });
    return;
  }
  // Hide the NDI inline hint when the user picks a real source.
  if (els.sourceHint && els.sourceHint.classList.contains('ndi-hint')) {
    els.sourceHint.hidden = true;
    els.sourceHint.classList.remove('ndi-hint');
  }
  const patch = { source_id: t.sourceId };
  if (t.sourceId === 'avfoundation' && t.avIndex !== null) {
    patch.av_video_index = t.avIndex;
    // Send the device NAME alongside the index — names are stable
    // across AVF rescans (indices reshuffle silently when devices
    // come/go), so the source factory uses the name as the canonical
    // identifier, falling back to index only if the name is unknown.
    if (t.name) patch.av_video_name = t.name;
  }
  applySettings(patch);
  els.pipeOnly.forEach((e) => (e.hidden = t.sourceId !== 'pipe'));
  setPreviewFor(t);
}

// -----------------------------------------------------------------
// Telemetry rendering
// -----------------------------------------------------------------
function renderTelemetry(snap) {
  const stats   = snap.stats || {};
  const cfg     = snap.active_config || null;
  const target  = cfg ? cfg.bitrate : 0;
  const actual  = stats.bitrate || 0;
  const isLive  = stats.status === 'Streaming';

  els.tmBitrate.textContent = `${Math.round(actual / 1000)} kbps`;
  if (target > 0) {
    const fill = Math.min(120, Math.round((actual / target) * 100));
    els.tmBitrateBar.style.setProperty('--fill', `${fill}%`);
  } else {
    els.tmBitrateBar.style.setProperty('--fill', `0%`);
  }

  const fps = isLive ? (stats.fps || 0) : 0;
  els.tmFps.textContent = isLive ? fps.toFixed(0) : '—';
  const targetFps = (snap.video_mode || '').match(/p([\d.]+)$/);
  els.tmFpsTarget.textContent = targetFps ? `target ${targetFps[1]}` : 'target —';

  const speed = stats.speed || 0;
  els.tmSpeed.textContent = isLive ? `${speed.toFixed(2)}×` : '—';
  let speedClass = '';
  let speedNote = isLive ? '' : 'idle';
  if (isLive) {
    if (speed >= 0.98)      { speedClass = 'healthy'; speedNote = 'realtime ✓'; }
    else if (speed >= 0.85) { speedClass = 'warn';    speedNote = 'falling slightly behind'; }
    else                    { speedClass = 'bad';     speedNote = 'falling behind — drop bitrate or fps'; }
  }
  setCellClass(els.tmSpeed.parentElement, speedClass);
  els.tmSpeedNote.textContent = speedNote;

  els.tmFrames.textContent = (stats.frames_sent || 0).toLocaleString();
  els.tmQuality.textContent = stats.quality ? `q=${stats.quality.toFixed(1)}` : 'q=—';

  const dropped = stats.frames_dropped || 0;
  els.tmDropped.textContent = dropped.toLocaleString();
  setCellClass(els.tmDropped.parentElement, dropped === 0 ? (isLive ? 'healthy' : '') : (dropped < 30 ? 'warn' : 'bad'));

  els.tmDuration.textContent = stats.duration || '00:00:00:00';
  els.tmElapsed.textContent = isLive ? 'streaming' : (stats.status === 'Connecting' ? 'connecting…' : 'idle');

  // Aux text in card title
  if (isLive) {
    els.connAux.textContent = `${Math.round(actual / 1000)} kbps · ${fps.toFixed(0)} fps · ${speed.toFixed(2)}×`;
  } else if (stats.status === 'Connecting') {
    els.connAux.textContent = 'opening SRT handshake…';
  } else if (stats.status === 'Interrupted') {
    els.connAux.textContent = 'interrupted';
  } else {
    els.connAux.textContent = 'idle';
  }
}

function setCellClass(cell, cls) {
  cell.classList.remove('healthy', 'warn', 'bad');
  if (cls) cell.classList.add(cls);
}

// -----------------------------------------------------------------
// Render full snapshot
// -----------------------------------------------------------------
function render(snap) {
  lastSnapshot = snap;

  if (document.activeElement !== els.label) els.label.value = snap.label;

  // Multi-service / multi-server selectors only appear in Advanced
  // when there are more than one of each. Most users with a single
  // loaded XML never see these.
  setOptions(els.service, snap.available_services, snap.current_service_name);
  const serverOptions = (snap.available_servers || []).map((s) => ({
    value: s.name,
    label: `${s.name}  ·  ${s.protocol.toUpperCase()}`,
  }));
  setOptions(els.server, serverOptions, snap.current_server_name);
  const showMultiService = (snap.available_services || []).length > 1
    || (snap.available_servers || []).length > 1;
  if (els.multiServiceRow) els.multiServiceRow.hidden = !showMultiService;

  // Address field — show whichever URL is active (custom_url override
  // wins; otherwise the resolved current_url from the loaded XML).
  // Don't clobber while the user is mid-typing.
  if (document.activeElement !== els.destAddress) {
    els.destAddress.value = snap.custom_url || snap.current_url || '';
  }

  els.destUrl.value = snap.current_url || '';
  if (document.activeElement !== els.streamKey) els.streamKey.value = snap.stream_key;
  if (document.activeElement !== els.passphrase) els.passphrase.value = snap.passphrase;
  els.streamid.textContent = buildStreamidPreview(snap);
  els.rtmpUrl.textContent = buildRtmpPreview(snap);

  // Protocol segmented control — reflects whatever's actually active.
  const activeProto = (snap.current_protocol || 'srt').toLowerCase();
  setSegmentedValue(els.protoSegs, activeProto === 'rtmps' ? 'rtmp' : activeProto);
  applyProtocolVisibility(activeProto);

  // Codec segmented control.
  setSegmentedValue(els.codecSegs, snap.video_codec || 'h265');

  // XML-loaded chip + service-name display.
  const loadedName = snap.current_service_name || '';
  if (loadedName) {
    els.xmlLoadedName.textContent = loadedName;
    els.xmlLoaded.hidden = false;
  } else {
    els.xmlLoaded.hidden = true;
  }

  if (snap.current_url || snap.custom_url) {
    const proto = (snap.current_protocol || '').toUpperCase();
    const url = snap.current_url || snap.custom_url;
    els.destAux.textContent = `${proto} → ${url.replace(/^[a-z]+:\/\//, '')}`;
  } else {
    els.destAux.textContent = 'no destination';
  }

  if (document.activeElement !== els.srtMode) els.srtMode.value = snap.srt_mode || 'caller';
  if (document.activeElement !== els.srtLatency) els.srtLatency.value = Math.round((snap.srt_latency_us || 500000) / 1000);
  if (document.activeElement !== els.srtListenPort) els.srtListenPort.value = snap.srt_listen_port || 9710;
  if (document.activeElement !== els.streamidOverride) els.streamidOverride.value = snap.streamid_override || '';
  if (els.streamidLegacy && document.activeElement !== els.streamidLegacy) els.streamidLegacy.checked = !!snap.streamid_legacy;
  applySrtModeVisibility(snap.srt_mode || 'caller');

  buildSourceTiles(snap);

  // Auto-start NDI preview whenever state shows we're on an NDI
  // source. Without this, hitting Start Stream without first
  // clicking the NDICAM tile (e.g. because the source was already
  // selected from a prior session, or set via the API) leaves the
  // preview area showing the default SMPTE bars even while the
  // backend is streaming and producing JPEGs. previewKey de-dupes
  // so this is a no-op when the right preview is already running.
  if (snap.source_id === 'ndi' && snap.ndi_source_name) {
    const wantKey = `ndi:${snap.ndi_source_name}`;
    if (previewKey !== wantKey) {
      setPreviewFor({ sourceId: 'ndi', name: snap.ndi_source_name, category: 'ndi' });
    }
  }

  if (knownDevices.audio.length) {
    setOptions(els.avAudio, [
      ...(snap.source_id === 'avfoundation' ? [] : [{ value: '-1', label: '— (auto / not used)' }]),
      ...knownDevices.audio.map((d) => ({ value: d.index, label: `[${d.index}] ${d.name}` })),
    ], snap.av_audio_index);
  }
  setOptions(els.videoMode, snap.available_video_modes, snap.video_mode);
  if (els.formatDecoded) els.formatDecoded.textContent = decodeVideoMode(snap.video_mode);
  setOptions(els.quality, snap.available_quality_levels || [], snap.quality_level);
  renderQualitySegmented(snap);
  if (document.activeElement !== els.pipePath) els.pipePath.value = snap.pipe_path || '';
  els.pipeOnly.forEach((e) => (e.hidden = snap.source_id !== 'pipe'));

  renderRelayPanels(snap);

  const ov = snap.overlay || {};
  if (document.activeElement !== els.ovTitle) els.ovTitle.value = ov.title || '';
  if (document.activeElement !== els.ovSubtitle) els.ovSubtitle.value = ov.subtitle || '';
  if (document.activeElement !== els.ovLogo) els.ovLogo.value = ov.logo_path || '';
  if (document.activeElement !== els.ovClock) els.ovClock.checked = !!ov.clock;

  // Status pill + body class for streaming-state animations
  const stats = snap.stats;
  els.statusPill.textContent = stats.status.toUpperCase();
  els.statusPill.classList.remove('streaming', 'connecting', 'interrupted');
  if (stats.status === 'Streaming') els.statusPill.classList.add('streaming');
  else if (stats.status === 'Connecting') els.statusPill.classList.add('connecting');
  else if (stats.status === 'Interrupted') els.statusPill.classList.add('interrupted');

  els.body.classList.toggle('is-streaming', stats.status === 'Streaming');
  els.liveBadge.hidden = stats.status !== 'Streaming';

  els.duration.textContent = stats.duration;
  els.monitorAux.textContent = stats.status === 'Streaming'
    ? `live · ${Math.round((stats.bitrate || 0) / 1000)} kbps`
    : (stats.status === 'Connecting' ? 'connecting…' : sourceLabel(snap));

  const cfg = snap.active_config;
  els.ovlSource.textContent  = sourceLabel(snap);
  els.ovlRes.textContent     = snap.video_mode || '—';
  els.ovlProfile.textContent = cfg ? `${snap.quality_level} · ${Math.round(cfg.bitrate / 1000)} kbps` : '—';
  els.ovlBitrate.textContent = `${Math.round((stats.bitrate || 0) / 1000)} kbps`;

  renderTelemetry(snap);

  if (stats.error) {
    els.error.hidden = false;
    els.error.textContent = stats.error;
  } else {
    els.error.hidden = true;
    els.error.textContent = '';
  }
}

function sourceLabel(snap) {
  if (snap.source_id === 'test_pattern') return 'Test Pattern';
  if (snap.source_id === 'avfoundation') {
    const v = snap.av_video_index ?? 0;
    const dev = knownDevices.video.find((d) => d.index === v);
    return dev ? dev.name : `AV[${v}]`;
  }
  if (snap.source_id === 'pipe') return snap.pipe_path || 'URL / Pipe';
  if (snap.source_id === 'srt_listen') return `SRT in :${snap.relay?.srt_port ?? 9710}`;
  if (snap.source_id === 'rtmp_listen') return `RTMP in :${snap.relay?.rtmp_port ?? 1935}`;
  return snap.source_id;
}

// -----------------------------------------------------------------
// Relay panel rendering — show/hide + populate URLs and config
// -----------------------------------------------------------------
function renderRelayPanels(snap) {
  const sid = snap.source_id;
  const r = snap.relay || {};

  els.relayPanels.forEach((el) => {
    el.hidden = el.dataset.relay !== sid;
  });

  // Host shown in the publish URL: prefer the LAN IP we fetched at
  // boot; fall back to the page hostname (which is usually 127.0.0.1
  // or localhost). Either way the bind address remains 0.0.0.0
  // server-side, so any interface accepts connections.
  const host = lanIp || window.location.hostname || '127.0.0.1';

  if (document.activeElement !== els.relaySrtPort)
    els.relaySrtPort.value = r.srt_port ?? 9710;
  if (document.activeElement !== els.relaySrtLatency)
    els.relaySrtLatency.value = Math.round((r.srt_latency_us ?? 200_000) / 1000);
  if (document.activeElement !== els.relaySrtPassphrase)
    els.relaySrtPassphrase.value = r.srt_passphrase || '';
  els.relaySrtUrl.value = `srt://${host}:${r.srt_port ?? 9710}`;

  if (document.activeElement !== els.relayRtmpPort)
    els.relayRtmpPort.value = r.rtmp_port ?? 1935;
  if (document.activeElement !== els.relayRtmpApp)
    els.relayRtmpApp.value = r.rtmp_app || 'live';
  if (document.activeElement !== els.relayRtmpKey)
    els.relayRtmpKey.value = r.rtmp_key || 'stream';
  els.relayRtmpUrl.value = `rtmp://${host}:${r.rtmp_port ?? 1935}/${r.rtmp_app || 'live'}`;
}

async function copyToClipboard(text, btn) {
  try {
    await navigator.clipboard.writeText(text);
    const orig = btn.textContent;
    btn.textContent = 'Copied';
    setTimeout(() => { btn.textContent = orig; }, 1200);
  } catch (_e) {
    // navigator.clipboard fails over plain http on some browsers — fall
    // back to a manual select so the user can ⌘C.
    const input = btn.previousElementSibling?.querySelector('input');
    if (input) { input.select(); }
  }
}

// -----------------------------------------------------------------
// Devices + polling
// -----------------------------------------------------------------
async function ensureDevicesLoaded(force = false) {
  try {
    const u = force ? '/api/devices?force=1' : '/api/devices';
    const j = await fetchJSON(u);
    knownDevices.video = j.video || [];
    knownDevices.audio = j.audio || [];
    if (lastSnapshot) render(lastSnapshot);
  } catch (_e) { /* ignore */ }
}

async function ensureNdiLoaded(force = false) {
  try {
    const u = force ? '/api/ndi-senders?force=1' : '/api/ndi-senders';
    const j = await fetchJSON(u);
    knownNdi = j.senders || [];
    if (lastSnapshot) render(lastSnapshot);
  } catch (_e) { /* ignore */ }
}

async function poll() {
  try {
    const snap = await fetchJSON('/api/state');
    render(snap);
    if (snap.stats.status !== 'Idle') {
      const log = await fetchJSON('/api/log');
      els.log.textContent = (log.lines || []).join('\n') || '(no output yet)';
      els.cmd.textContent = log.command || '';
    }
  } catch (_e) { /* ignore */ }
}

// -----------------------------------------------------------------
// Decode "1080p59.94" -> "1920 × 1080 @ 59.94 fps"
// -----------------------------------------------------------------
function decodeVideoMode(mode) {
  if (!mode || mode === 'Auto') return '— × — @ — fps';
  const m = mode.match(/^(\d+)p([\d.]+)$/);
  if (!m) return mode;
  const height = parseInt(m[1], 10);
  const width = height === 1080 ? 1920 : 1280;
  return `${width} × ${height} @ ${m[2]} fps`;
}

// -----------------------------------------------------------------
// Segmented-control helpers (Protocol / Codec)
// -----------------------------------------------------------------
function setSegmentedValue(inputs, value) {
  for (const r of inputs) r.checked = (r.value === value);
}

function getSegmentedValue(inputs) {
  for (const r of inputs) if (r.checked) return r.value;
  return null;
}

// Phase 8b: render the Quality segmented control from snap.quality_options.
// Each option's label includes the projected Mbps at the current video_mode
// (the backend computes per-mode bitrates so the labels update when the
// user changes Format). Falls back to no-op when no XML is loaded.
function renderQualitySegmented(snap) {
  if (!els.qualitySeg) return;
  const opts = snap.quality_options || [];
  const current = snap.quality_level || '';
  if (opts.length === 0) {
    els.qualitySeg.innerHTML = '<span class="seg-empty">no quality options (load a service XML first)</span>';
    return;
  }
  const html = opts.map((o) => {
    const mbps = (o.bitrate / 1_000_000).toFixed(1).replace(/\.0$/, '');
    const checked = o.name === current ? ' checked' : '';
    const short = qualityShortName(o.name);
    return `<label class="seg seg-quality">
      <input type="radio" name="dest-quality" value="${escapeHtml(o.name)}"${checked} />
      <span><strong>${escapeHtml(short)}</strong><em>${mbps} Mbps</em></span>
    </label>`;
  }).join('');
  els.qualitySeg.innerHTML = html;
  // (Re)bind listeners — innerHTML wipes them.
  els.qualitySeg.querySelectorAll('input[name="dest-quality"]').forEach((r) => {
    r.addEventListener('change', () => {
      if (r.checked) applySettings({ quality_level: r.value });
    });
  });
}

// "Streaming High" -> "High" for the segmented-control label.
function qualityShortName(name) {
  if (/high/i.test(name))   return 'High';
  if (/medium/i.test(name)) return 'Medium';
  if (/low/i.test(name))    return 'Low';
  return name;
}

// Parse what the user typed in the Address field.
//   "1.2.3.4:1935"        -> add scheme from current Protocol toggle
//   "srt://1.2.3.4:1935"  -> use as-is, sync Protocol toggle
//   "rtmp://srv/live"     -> use as-is, sync Protocol toggle
//   ""                    -> clear custom_url (let XML drive)
function applyAddressInput(raw) {
  let addr = (raw || '').trim();
  if (!addr) {
    applySettings({ custom_url: '' });
    return;
  }
  const schemeMatch = addr.match(/^([a-z]+):\/\//i);
  let proto = schemeMatch ? schemeMatch[1].toLowerCase() : null;
  if (!proto) {
    proto = getSegmentedValue(els.protoSegs) || 'srt';
    addr = `${proto}://${addr}`;
  }
  // Sync the Protocol toggle to whatever scheme we end up with so the
  // UI stays consistent. rtmps falls back to "rtmp" for the toggle.
  setSegmentedValue(els.protoSegs, proto === 'rtmps' ? 'rtmp' : proto);
  applySettings({ custom_url: addr });
}

// Switching the Protocol toggle has two effects: pick the matching
// server out of a loaded XML (if any), AND if a custom URL is in
// effect, swap its scheme so the user's typed address still works.
function applyProtocolToggle(proto) {
  const snap = lastSnapshot || {};
  const patches = {};
  const matching = (snap.available_servers || []).find((s) => s.protocol === proto);
  if (matching) patches.current_server_name = matching.name;
  if (snap.custom_url) {
    patches.custom_url = snap.custom_url.replace(/^[a-z]+:\/\//i, `${proto}://`);
  }
  if (Object.keys(patches).length) applySettings(patches);
}

// -----------------------------------------------------------------
// Paste flow
// -----------------------------------------------------------------
async function applyPaste() {
  const text = (els.pasteText.value || '').trim();
  if (!text) {
    showStatus(els.pasteStatus, false, 'Paste something first.');
    return;
  }
  els.pasteApply.disabled = true;
  try {
    const r = await fetch('/api/destination/paste', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text }),
    });
    const j = await r.json();
    if (!r.ok || j.error) {
      showStatus(els.pasteStatus, false, j.error || 'Could not parse pasted settings.');
      return;
    }
    const p = j.parsed || {};
    const summary = [];
    if (p.url)        summary.push(`URL: ${p.url}`);
    if (p.stream_key) summary.push(`Key: ${p.stream_key}`);
    if (p.passphrase) summary.push('Passphrase set');
    if (p.name)       summary.push(`Name: ${p.name}`);
    showStatus(els.pasteStatus, true, 'Applied. ' + summary.join(' · '));
    if (j.snapshot) render(j.snapshot);
  } catch (e) {
    showStatus(els.pasteStatus, false, 'Error: ' + e);
  } finally {
    els.pasteApply.disabled = false;
  }
}

function showStatus(el, ok, msg) {
  el.hidden = false;
  el.textContent = msg;
  el.classList.toggle('error', !ok);
}

// -----------------------------------------------------------------
// XML import flow — wizard exposes drop-zone only; the dedicated
// "paste XML directly" textarea was dropped from the simplified UI.
// Power users who need to paste XML can drop a .xml file straight in.
// -----------------------------------------------------------------
async function applyXmlText(text) {
  if (!text.trim()) {
    showStatus(els.xmlStatus, false, 'Drop an XML file first.');
    return;
  }
  try {
    const r = await fetch('/api/load_xml_text', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text }),
    });
    const j = await r.json();
    if (!r.ok || j.error) {
      showStatus(els.xmlStatus, false, j.error || 'Could not load XML.');
      return;
    }
    // Backend now returns {service, snapshot}. Each load implicitly
    // replaces the existing service registry so dropping a fresh XML
    // wipes the previous one cleanly (the boot loader passes
    // replace=false to preserve the accumulate-on-startup behavior).
    showStatus(els.xmlStatus, true, `Loaded service: ${j.service || '(unnamed)'}`);
    if (j.snapshot) {
      render(j.snapshot);
      // Refresh the loaded-XML chip with the just-loaded service name.
      if (els.xmlLoaded) {
        els.xmlLoaded.hidden = false;
        if (els.xmlLoadedName) els.xmlLoadedName.textContent = j.service || '(unnamed)';
      }
    }
  } catch (e) {
    showStatus(els.xmlStatus, false, 'Error: ' + e);
  }
}

function setupXmlDrop() {
  const dz = els.xmlDrop;
  dz.addEventListener('click', () => els.xmlFile.click());
  els.xmlFile.addEventListener('change', async (e) => {
    const f = e.target.files && e.target.files[0];
    if (!f) return;
    applyXmlText(await f.text());
  });
  ['dragenter', 'dragover'].forEach((ev) => dz.addEventListener(ev, (e) => {
    e.preventDefault(); e.stopPropagation(); dz.classList.add('drag');
  }));
  ['dragleave', 'drop'].forEach((ev) => dz.addEventListener(ev, (e) => {
    e.preventDefault(); e.stopPropagation(); dz.classList.remove('drag');
  }));
  dz.addEventListener('drop', async (e) => {
    const f = e.dataTransfer.files && e.dataTransfer.files[0];
    if (!f) return;
    applyXmlText(await f.text());
  });
}

// -----------------------------------------------------------------
// LAN discover flow
// -----------------------------------------------------------------
async function runLanDiscover() {
  els.discoverResults.innerHTML = 'Scanning mDNS for <code>_blackmagic._tcp</code>… (3 s)';
  els.lanDiscover.disabled = true;
  try {
    const r = await fetch('/api/discover?force=1');
    const j = await r.json();
    const devs = j.devices || [];
    if (!devs.length) {
      els.discoverResults.innerHTML =
        'No BMD devices found on this LAN. (Bridge may be remote-only, or mDNS blocked by network.)';
      return;
    }
    els.discoverResults.innerHTML = '<strong>Found:</strong>' + devs.map((d) => {
      const tx = d.txt || {};
      const meta = Object.entries(tx).slice(0, 4).map(([k, v]) => `${k}=${v}`).join(' · ');
      const safeName = escapeHtml(d.name || '');
      const safeHost = escapeHtml(d.host || '');
      const useUrl = d.host ? `srt://${d.host}:${d.port || 1935}` : '';
      const useBtn = useUrl ? ` <a href="#" data-url="${useUrl}" class="use-discovered">use as destination</a>` : '';
      return `<br>• <strong>${safeName}</strong> — ${safeHost || '<em>unresolved</em>'}:${d.port || '?'} <span class="muted">[${escapeHtml(d.service_type || '')}]</span>${useBtn}<br><span class="muted">  ${meta}</span>`;
    }).join('');
    els.discoverResults.querySelectorAll('.use-discovered').forEach((a) => {
      a.addEventListener('click', (ev) => {
        ev.preventDefault();
        applySettings({ custom_url: a.getAttribute('data-url') });
      });
    });
  } catch (err) {
    els.discoverResults.textContent = 'Discover failed: ' + err;
  } finally {
    els.lanDiscover.disabled = false;
  }
}

// -----------------------------------------------------------------
// Bind everything
// -----------------------------------------------------------------
function bind() {
  els.brandBtn.addEventListener('click', () => {
    window.scrollTo({ top: 0, behavior: 'smooth' });
  });

  // Destination wizard — primary inputs
  els.destAddress.addEventListener('change', () => applyAddressInput(els.destAddress.value));
  els.streamKey.addEventListener('change', () => applySettings({ stream_key: els.streamKey.value }));
  els.passphrase.addEventListener('change', () => applySettings({ passphrase: els.passphrase.value }));

  // Segmented controls — Protocol + Codec
  els.protoSegs.forEach((r) => r.addEventListener('change', () => {
    if (r.checked) applyProtocolToggle(r.value);
  }));
  els.codecSegs.forEach((r) => r.addEventListener('change', () => {
    if (r.checked) applySettings({ video_codec: r.value });
  }));

  // XML drop-zone (in wizard) + clear button
  setupXmlDrop();
  if (els.xmlClear) {
    els.xmlClear.addEventListener('click', async () => {
      // POST /api/services/clear (with default body) wipes loaded
      // services AND custom_url so the destination is fully blank.
      // The user can then type a manual address into the Address
      // field, or drop a fresh XML to load a new service.
      try {
        const r = await fetch('/api/services/clear', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: '{}',
        });
        if (r.ok) {
          const snap = await r.json();
          render(snap);
          // Hide the loaded-XML chip + clear status text.
          if (els.xmlLoaded) els.xmlLoaded.hidden = true;
          if (els.xmlStatus) els.xmlStatus.hidden = true;
        }
      } catch (_e) { /* ignore */ }
    });
  }

  // Advanced — multi-service / multi-server selectors
  els.label.addEventListener('change', () => applySettings({ label: els.label.value }));
  els.service.addEventListener('change', () => applySettings({ current_service_name: els.service.value }));
  els.server.addEventListener('change', () => applySettings({ current_server_name: els.server.value }));

  // Advanced — paste-anything fallback (kept for power users)
  els.pasteApply.addEventListener('click', applyPaste);
  els.pasteClear.addEventListener('click', () => {
    els.pasteText.value = '';
    els.pasteStatus.hidden = true;
  });

  // Advanced — LAN discover button
  els.lanDiscover.addEventListener('click', runLanDiscover);

  // Source devices
  els.avAudio.addEventListener('change', () => {
    const idx = parseInt(els.avAudio.value, 10);
    const patch = { av_audio_index: idx };
    // Look up the audio device's name from the most-recent device list
    // and store it alongside the index — stable across AVF rescans.
    const dev = knownDevices.audio.find((d) => d.index === idx);
    patch.av_audio_name = dev ? dev.name : '';
    applySettings(patch);
  });
  els.pipePath.addEventListener('change', () => applySettings({ pipe_path: els.pipePath.value }));
  els.rescanDevices.addEventListener('click', (e) => { e.preventDefault(); ensureDevicesLoaded(true); });
  els.ndiRescan.addEventListener('click', (e) => { e.preventDefault(); ensureNdiLoaded(true); });
  els.videoMode.addEventListener('change', () => applySettings({ video_mode: els.videoMode.value }));
  els.quality.addEventListener('change', () => applySettings({ quality_level: els.quality.value }));

  // Relay panels — config inputs + copy buttons
  const relayPatch = (patch) => applySettings({ relay: patch });
  els.relaySrtPort.addEventListener('change', () => {
    const p = parseInt(els.relaySrtPort.value, 10);
    if (!isNaN(p)) relayPatch({ srt_port: p });
  });
  els.relaySrtLatency.addEventListener('change', () => {
    const ms = parseInt(els.relaySrtLatency.value, 10);
    if (!isNaN(ms)) relayPatch({ srt_latency_us: ms * 1000 });
  });
  els.relaySrtPassphrase.addEventListener('change', () => relayPatch({ srt_passphrase: els.relaySrtPassphrase.value }));
  els.relaySrtCopy.addEventListener('click', () => copyToClipboard(els.relaySrtUrl.value, els.relaySrtCopy));
  els.relayRtmpPort.addEventListener('change', () => {
    const p = parseInt(els.relayRtmpPort.value, 10);
    if (!isNaN(p)) relayPatch({ rtmp_port: p });
  });
  els.relayRtmpApp.addEventListener('change', () => relayPatch({ rtmp_app: els.relayRtmpApp.value }));
  els.relayRtmpKey.addEventListener('change', () => relayPatch({ rtmp_key: els.relayRtmpKey.value }));
  els.relayRtmpCopy.addEventListener('click', () => copyToClipboard(els.relayRtmpUrl.value, els.relayRtmpCopy));

  // Encoder
  // Codec wiring is now via els.codecSegs above (segmented control).

  // SRT advanced
  els.srtMode.addEventListener('change', () => {
    applySrtModeVisibility(els.srtMode.value);
    applySettings({ srt_mode: els.srtMode.value });
  });
  els.srtLatency.addEventListener('change', () => {
    const ms = parseInt(els.srtLatency.value, 10);
    if (!isNaN(ms)) applySettings({ srt_latency_us: ms * 1000 });
  });
  els.srtListenPort.addEventListener('change', () => {
    const p = parseInt(els.srtListenPort.value, 10);
    if (!isNaN(p)) applySettings({ srt_listen_port: p });
  });
  els.streamidOverride.addEventListener('change', () => applySettings({ streamid_override: els.streamidOverride.value }));
  els.streamidLegacy.addEventListener('change', () => applySettings({ streamid_legacy: els.streamidLegacy.checked }));

  // Overlay
  els.ovTitle.addEventListener('change',    () => applySettings({ overlay: { title:     els.ovTitle.value } }));
  els.ovSubtitle.addEventListener('change', () => applySettings({ overlay: { subtitle:  els.ovSubtitle.value } }));
  els.ovLogo.addEventListener('change',     () => applySettings({ overlay: { logo_path: els.ovLogo.value } }));
  els.ovClock.addEventListener('change',    () => applySettings({ overlay: { clock:     els.ovClock.checked } }));

  // Start / stop
  els.startBtn.addEventListener('click', async () => {
    els.startBtn.disabled = true;
    // Release the browser's getUserMedia hold on the camera before
    // FFmpeg tries to open it. Some virtual cameras (NDI Virtual
    // Camera in particular) serialize frame delivery to one consumer
    // — when both the browser preview and FFmpeg try to read at the
    // same time, only the browser gets frames and FFmpeg sits at
    // frame=0 forever despite reporting a successful AVF open. The
    // tradeoff: live preview disappears the moment streaming starts.
    // Acceptable since the Monitor card switches to telemetry once
    // the stream is up anyway.
    stopPreview();
    try {
      const r = await fetch('/api/start', { method: 'POST' });
      const j = await r.json();
      if (j.error) {
        els.error.hidden = false;
        els.error.textContent = j.error;
      } else {
        render(j);
      }
    } finally {
      setTimeout(() => (els.startBtn.disabled = false), 600);
    }
  });
  els.stopBtn.addEventListener('click', async () => {
    const r = await fetch('/api/stop', { method: 'POST' });
    render(await r.json());
  });

  // -----------------------------------------------------------------
  // Phase 8b: receive-stream mini-wizard handlers
  // -----------------------------------------------------------------
  const rwProto = $$('input[name="rw-proto"]');
  const rwUrl = $('#rw-publish-url');
  const rwCopy = $('#rw-copy');
  const rwAppPick = $('#rw-app-pick');
  const rwAppBody = $('#rw-app-instructions');
  const rwStart = $('#rw-start-receiver');

  function getRwProto() {
    for (const r of rwProto) if (r.checked) return r.value;
    return 'srt_listen';
  }

  function refreshRwUrl() {
    if (!rwUrl) return;
    const proto = getRwProto();
    const host = lanIp || window.location.hostname || '0.0.0.0';
    const r = lastSnapshot?.relay || {};
    if (proto === 'srt_listen') {
      rwUrl.value = `srt://${host}:${r.srt_port ?? 9710}`;
    } else {
      rwUrl.value = `rtmp://${host}:${r.rtmp_port ?? 1935}/${r.rtmp_app || 'live'}/${r.rtmp_key || 'stream'}`;
    }
  }

  function renderRwApp(app) {
    if (!rwAppBody) return;
    if (!app) { rwAppBody.hidden = true; rwAppBody.innerHTML = ''; return; }
    const proto = getRwProto();
    const url = rwUrl?.value || '';
    let html = '';
    if (app === 'obs' && proto === 'srt_listen') {
      html = `<strong>OBS → Settings → Stream</strong>
        <ol>
          <li>Service: <code>Custom...</code></li>
          <li>Server: <code>${escapeHtml(url)}?streamid=publish</code></li>
          <li>Stream Key: leave blank</li>
          <li>Output → Encoder: x264 or HEVC, Keyframe Interval 2s, Bitrate to match what your network can carry</li>
        </ol>`;
    } else if (app === 'obs' && proto === 'rtmp_listen') {
      html = `<strong>OBS → Settings → Stream</strong>
        <ol>
          <li>Service: <code>Custom...</code></li>
          <li>Server: <code>rtmp://${escapeHtml(lanIp || '0.0.0.0')}:${(lastSnapshot?.relay?.rtmp_port) ?? 1935}/${escapeHtml(lastSnapshot?.relay?.rtmp_app || 'live')}</code></li>
          <li>Stream Key: <code>${escapeHtml(lastSnapshot?.relay?.rtmp_key || 'stream')}</code></li>
        </ol>`;
    } else if (app === 'larix') {
      html = `<strong>Larix Broadcaster (iPhone / Android)</strong>
        <ol>
          <li>Settings → Connections → New connection</li>
          <li>Mode: <code>Caller</code></li>
          <li>URL: <code>${escapeHtml(url)}</code></li>
          <li>Format: MPEG-TS / FLV (matches the protocol you picked)</li>
          <li>Encoder: H.264 or HEVC, Keyframe interval 2s</li>
        </ol>`;
    } else if (app === 'dji') {
      html = `<strong>DJI drone (RC Plus / Mini 4 Pro / Mavic 3)</strong>
        <ol>
          <li>In the Fly app: <em>Camera View → Transmission → Live Streaming Platform → RTMP Custom</em></li>
          <li>RTMP URL: <code>rtmp://${escapeHtml(lanIp || '0.0.0.0')}:${(lastSnapshot?.relay?.rtmp_port) ?? 1935}/${escapeHtml(lastSnapshot?.relay?.rtmp_app || 'live')}/${escapeHtml(lastSnapshot?.relay?.rtmp_key || 'stream')}</code></li>
          <li>(SRT isn't supported natively on most DJI consumer drones — pick RTMP above for these)</li>
        </ol>`;
    } else if (app === 'ffmpeg') {
      const cmd = proto === 'srt_listen'
        ? `ffmpeg -re -i input.mp4 -c:v libx264 -preset veryfast -tune zerolatency -c:a aac -f mpegts '${url}'`
        : `ffmpeg -re -i input.mp4 -c:v libx264 -preset veryfast -tune zerolatency -c:a aac -f flv '${url}'`;
      html = `<strong>FFmpeg from a file or device</strong>
        <pre style="white-space:pre-wrap;font-size:11px;color:#b6e1c1;background:rgba(0,0,0,0.25);padding:8px 10px;border-radius:5px;">${escapeHtml(cmd)}</pre>`;
    } else if (app === 'iphone-bm') {
      html = `<strong>Blackmagic Camera app (iPhone)</strong>
        <ol>
          <li>Tap the gear icon → <em>Stream</em></li>
          <li>Service: <code>Custom RTMP</code> (the BMD app speaks RTMP only)</li>
          <li>Server: <code>rtmp://${escapeHtml(lanIp || '0.0.0.0')}:${(lastSnapshot?.relay?.rtmp_port) ?? 1935}/${escapeHtml(lastSnapshot?.relay?.rtmp_app || 'live')}</code></li>
          <li>Key: <code>${escapeHtml(lastSnapshot?.relay?.rtmp_key || 'stream')}</code></li>
          <li>Pick RTMP above (Blackmagic Camera doesn't do SRT yet)</li>
        </ol>`;
    } else {
      html = `<em>Pick your encoder app above for tailored instructions.</em>`;
    }
    rwAppBody.innerHTML = html;
    rwAppBody.hidden = false;
  }

  rwProto.forEach((r) => r.addEventListener('change', () => {
    refreshRwUrl();
    renderRwApp(rwAppPick?.value || '');
  }));
  if (rwAppPick) rwAppPick.addEventListener('change', () => renderRwApp(rwAppPick.value));
  if (rwCopy) rwCopy.addEventListener('click', () => copyToClipboard(rwUrl.value, rwCopy));
  if (rwStart) rwStart.addEventListener('click', async () => {
    rwStart.disabled = true;
    try {
      // Switch the source to the chosen relay listener, then start the
      // stream. The relay panel below the source tiles becomes visible
      // automatically once source_id flips.
      await applySettings({ source_id: getRwProto() });
      const r = await fetch('/api/start', { method: 'POST' });
      const j = await r.json();
      if (j.error) {
        els.error.hidden = false;
        els.error.textContent = j.error;
      } else {
        render(j);
      }
    } finally {
      setTimeout(() => (rwStart.disabled = false), 600);
    }
  });

  // Refresh URL whenever the snapshot changes (port/app may move).
  // The poll loop already calls render(snap) on every tick; we just
  // need to re-render the URL when the wizard is open.
  const urlRefreshTimer = setInterval(refreshRwUrl, 1000);
  // No teardown needed — runs for the lifetime of the page.
  void urlRefreshTimer;
}

bind();
ensureDevicesLoaded();
ensureNdiLoaded();
fetchJSON('/api/lan-ip').then((j) => {
  lanIp = j.ip || '';
  if (lastSnapshot) renderRelayPanels(lastSnapshot);
}).catch(() => {});
poll();
setInterval(poll, 1000);
// Refresh NDI senders every 30s — they come and go.
setInterval(() => ensureNdiLoaded(true), 30000);

// Tauri's WebView ships with no menu bar and no built-in reload
// shortcut, which is friction during dev iteration. Bind the same
// keys a normal browser would: Cmd/Ctrl-R, Cmd-Shift-R (force),
// and F5. No-op in real browsers (they handle it themselves first).
document.addEventListener('keydown', (e) => {
  const isReload = (e.metaKey || e.ctrlKey) && (e.key === 'r' || e.key === 'R');
  if (isReload || e.key === 'F5') {
    e.preventDefault();
    location.reload();
  }
});
