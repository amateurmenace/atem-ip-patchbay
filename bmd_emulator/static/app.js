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

  // Destination
  service:    $('#service'),
  server:     $('#server'),
  destUrl:    $('#dest-url'),
  streamKey:  $('#stream-key'),
  passphrase: $('#passphrase'),
  streamid:   $('#streamid'),
  rtmpUrl:    $('#rtmp-url'),
  srtOnly:    $$('.srt-only'),
  rtmpOnly:   $$('.rtmp-only'),

  // Paste
  pasteText:   $('#paste-text'),
  pasteApply:  $('#paste-apply'),
  pasteClear:  $('#paste-clear'),
  pasteStatus: $('#paste-status'),

  // XML import
  xmlDrop:   $('#xml-drop'),
  xmlFile:   $('#xml-file'),
  xmlText:   $('#xml-text'),
  xmlApply:  $('#xml-apply'),
  xmlStatus: $('#xml-status'),

  // LAN discover
  lanDiscover:     $('#lan-discover'),
  discoverResults: $('#discover-results'),

  // Encoder
  videoCodec: $('#video-codec'),
  quality:    $('#quality'),
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
}

// Decide which preview to show based on a clicked tile.
async function setPreviewFor(tile) {
  if (tile.sourceId === 'test_pattern') return showTestPatternPreview();
  if (tile.sourceId === 'pipe')         return showPipePreview(tile.name);
  if (tile.sourceId === 'ndi-sender')   return showNdiHint(tile.name);
  if (tile.sourceId === 'avfoundation') {
    if (tile.category === 'screen') return startScreenPreview();
    return startCameraPreview(tile.name);
  }
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
    const isActive =
      snap.source_id === t.sourceId &&
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
    // Informational only — show hint, don't change FFmpeg state.
    setPreviewFor(t);
    return;
  }
  const patch = { source_id: t.sourceId };
  if (t.sourceId === 'avfoundation' && t.avIndex !== null) {
    patch.av_video_index = t.avIndex;
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

  setOptions(els.service, snap.available_services, snap.current_service_name);
  const serverOptions = (snap.available_servers || []).map((s) => ({
    value: s.name,
    label: `${s.name}  ·  ${s.protocol.toUpperCase()}`,
  }));
  setOptions(els.server, serverOptions, snap.current_server_name);

  els.destUrl.value = snap.current_url || '';
  if (document.activeElement !== els.streamKey) els.streamKey.value = snap.stream_key;
  if (document.activeElement !== els.passphrase) els.passphrase.value = snap.passphrase;
  els.streamid.textContent = buildStreamidPreview(snap);
  els.rtmpUrl.textContent = buildRtmpPreview(snap);
  applyProtocolVisibility((snap.current_protocol || '').toLowerCase());

  if (snap.current_url) {
    const proto = (snap.current_protocol || '').toUpperCase();
    els.destAux.textContent = `${proto} → ${snap.current_url.replace(/^[a-z]+:\/\//, '')}`;
  } else {
    els.destAux.textContent = 'no destination';
  }

  if (document.activeElement !== els.srtMode) els.srtMode.value = snap.srt_mode || 'caller';
  if (document.activeElement !== els.srtLatency) els.srtLatency.value = Math.round((snap.srt_latency_us || 500000) / 1000);
  if (document.activeElement !== els.srtListenPort) els.srtListenPort.value = snap.srt_listen_port || 9710;
  if (document.activeElement !== els.streamidOverride) els.streamidOverride.value = snap.streamid_override || '';
  if (els.streamidLegacy && document.activeElement !== els.streamidLegacy) els.streamidLegacy.checked = !!snap.streamid_legacy;
  if (els.videoCodec && document.activeElement !== els.videoCodec) els.videoCodec.value = snap.video_codec || 'h265';
  applySrtModeVisibility(snap.srt_mode || 'caller');

  buildSourceTiles(snap);

  if (knownDevices.audio.length) {
    setOptions(els.avAudio, [
      ...(snap.source_id === 'avfoundation' ? [] : [{ value: '-1', label: '— (auto / not used)' }]),
      ...knownDevices.audio.map((d) => ({ value: d.index, label: `[${d.index}] ${d.name}` })),
    ], snap.av_audio_index);
  }
  setOptions(els.videoMode, snap.available_video_modes, snap.video_mode);
  setOptions(els.quality, snap.available_quality_levels || [], snap.quality_level);
  if (document.activeElement !== els.pipePath) els.pipePath.value = snap.pipe_path || '';
  els.pipeOnly.forEach((e) => (e.hidden = snap.source_id !== 'pipe'));

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
  return snap.source_id;
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
// Tabs
// -----------------------------------------------------------------
function showTab(name) {
  $$('.tab').forEach((t) => t.classList.toggle('active', t.dataset.tab === name));
  $$('.tab-panel').forEach((p) => (p.hidden = p.dataset.panel !== name));
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
// XML import flow
// -----------------------------------------------------------------
async function applyXmlText(text) {
  if (!text.trim()) {
    showStatus(els.xmlStatus, false, 'Drop a file or paste XML first.');
    return;
  }
  els.xmlApply.disabled = true;
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
    showStatus(els.xmlStatus, true, `Loaded service: ${j.service}`);
    if (j.snapshot) render(j.snapshot);
    showTab('saved');
  } catch (e) {
    showStatus(els.xmlStatus, false, 'Error: ' + e);
  } finally {
    els.xmlApply.disabled = false;
  }
}

function setupXmlDrop() {
  const dz = els.xmlDrop;
  dz.addEventListener('click', () => els.xmlFile.click());
  els.xmlFile.addEventListener('change', async (e) => {
    const f = e.target.files && e.target.files[0];
    if (!f) return;
    const text = await f.text();
    els.xmlText.value = text;
    applyXmlText(text);
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
    const text = await f.text();
    els.xmlText.value = text;
    applyXmlText(text);
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
        showTab('saved');
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
  $$('.tab').forEach((t) => t.addEventListener('click', () => showTab(t.dataset.tab)));

  els.brandBtn.addEventListener('click', () => {
    window.scrollTo({ top: 0, behavior: 'smooth' });
  });

  // Saved tab
  els.label.addEventListener('change', () => applySettings({ label: els.label.value }));
  els.streamKey.addEventListener('change', () => applySettings({ stream_key: els.streamKey.value }));
  els.passphrase.addEventListener('change', () => applySettings({ passphrase: els.passphrase.value }));
  els.service.addEventListener('change', () => applySettings({ current_service_name: els.service.value }));
  els.server.addEventListener('change', () => applySettings({ current_server_name: els.server.value }));

  // Paste tab
  els.pasteApply.addEventListener('click', applyPaste);
  els.pasteClear.addEventListener('click', () => {
    els.pasteText.value = '';
    els.pasteStatus.hidden = true;
  });

  // XML tab
  els.xmlApply.addEventListener('click', () => applyXmlText(els.xmlText.value));
  setupXmlDrop();

  // LAN tab
  els.lanDiscover.addEventListener('click', runLanDiscover);

  // Source devices
  els.avAudio.addEventListener('change', () => applySettings({ av_audio_index: parseInt(els.avAudio.value, 10) }));
  els.pipePath.addEventListener('change', () => applySettings({ pipe_path: els.pipePath.value }));
  els.rescanDevices.addEventListener('click', (e) => { e.preventDefault(); ensureDevicesLoaded(true); });
  els.ndiRescan.addEventListener('click', (e) => { e.preventDefault(); ensureNdiLoaded(true); });
  els.videoMode.addEventListener('change', () => applySettings({ video_mode: els.videoMode.value }));
  els.quality.addEventListener('change', () => applySettings({ quality_level: els.quality.value }));

  // Encoder
  els.videoCodec.addEventListener('change', () => applySettings({ video_codec: els.videoCodec.value }));

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
}

bind();
ensureDevicesLoaded();
ensureNdiLoaded();
poll();
setInterval(poll, 1000);
// Refresh NDI senders every 30s — they come and go.
setInterval(() => ensureNdiLoaded(true), 30000);
