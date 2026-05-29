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
  refreshApp: $('#refresh-app'),
  killOrphans: $('#kill-orphans'),
  forceStopAll: $('#force-stop-all'),
  openNetDiag: $('#open-net-diag'),
  // alpha.42: openMultiview button removed; the view-mode toggle in
  // the topbar (.view-toggle) handles Single ↔ Multi navigation now.
  // alpha.52: openMonitor button removed too — multiview supersedes
  // the alpha.16 single-tile companion window pattern.
  heroHideBtn: $('#hero-hide-btn'),
  introToggleBtn: $('#intro-toggle-btn'),
  omtOutputEnabled: $('#omt-output-enabled'),
  omtOutputName:    $('#omt-output-name'),
  omtOutputStatus:  $('#omt-output-status'),
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
  previewBtn:     $('#btn-preview'),
  stopBtn:        $('#btn-stop'),
  receiveJumpBtn: $('#btn-receive-jump'),
  error:          $('#error'),

  // Receiver-active banner near the player. Shows when an SRT/RTMP
  // listener is running so the user has highly-visible feedback even
  // when the receive-stream wizard is collapsed at the bottom of the
  // page. JS toggles its hidden flag + classes from updateReceiverUi().
  receiverBanner:        $('#receiver-banner'),
  receiverBannerTitle:   $('#receiver-banner-title'),
  receiverBannerSubtitle:$('#receiver-banner-subtitle'),
  receiverBannerUrl:     $('#receiver-banner-url'),
  receiverBannerStop:    $('#receiver-banner-stop'),
  receiveWizard:         $('#receive-wizard'),

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
  audioPanRow:    $('#audio-pan-row'),
  audioPanL:      $('#audio-pan-l'),
  audioPanR:      $('#audio-pan-r'),
  audioCustom:    $('#audio-custom'),
  audioModeRadios:    $$('input[name="audio-mode"]'),
  audioOutputRadios:  $$('input[name="audio-output"]'),
  videoMode:      $('#video-mode'),
  pipeOnly:       $$('.pipe-only'),
  pipePath:       $('#pipe-path'),
  rescanDevices:  $('#rescan-devices'),
  ndiRescan:      $('#ndi-rescan'),
  omtRescan:      $('#omt-rescan'),

  // Relay (incoming SRT/RTMP server)
  // Old per-tile relay-config panels removed in favor of the
  // receive-stream wizard above (rw-* IDs). Settings (port,
  // latency, passphrase, RTMP app/key) stay at their server-side
  // defaults; advanced overrides can come back as a collapsible
  // <details> in the wizard if a user actually needs them.

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

  // Session 12 — destination-type picker + DeckLink output config
  destTypeRow:        $('#dest-type-row'),
  destTypeSegs:       $$('input[name="dest-type"]'),
  destAtemBody:       $('#dest-atem-body'),
  destDecklinkBody:   $('#dest-decklink-body'),
  destDecklinkUnsupported: $('#dest-decklink-unsupported'),
  decklinkDevice:     $('#decklink-device'),
  decklinkMode:       $('#decklink-mode'),
  decklinkDeviceStatus: $('#decklink-device-status'),
  decklinkRefresh:    $('#decklink-refresh'),

  // alpha.22's UDM config dialog elements removed in alpha.25 — UDM
  // is configured inside the net-diag dashboard now.

  // alpha.25: Advanced encoding controls.
  videoEncoder:       $('#video-encoder'),
  videoEncoderHint:   $('#video-encoder-hint'),
  autoReconnect:      $('#auto-reconnect'),
  audioCodec:         $('#audio-codec'),
  audioBitrateKbps:   $('#audio-bitrate-kbps'),
  audioSampleRate:    $('#audio-sample-rate'),
  audioChannelsSel:   $('#audio-channels'),
  metersEnabled:      $('#meters-enabled'),
  encoderExtraFlags:  $('#encoder-extra-flags'),
};

let knownDecklinkDevices = []; // populated by fetchDecklinkDevices()

let lastSnapshot   = null;
let knownDevices   = { video: [], audio: [] };
let knownNdi       = [];                 // discovered NDI senders
let knownOmt       = [];                 // discovered OMT senders (alpha.9)
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
  omt:          `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><circle cx="12" cy="12" r="9"/><path d="M5 8h14M5 16h14M9 5l-2 14M15 5l2 14"/></svg>`,
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
  omt:          'OMT',
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
// Session 12 — DeckLink output destination
// -----------------------------------------------------------------

/// Fetch the current DeckLink output devices + per-device modes from
/// the backend. Empty result is the operator's "missing prerequisite"
/// signal — distinguished from "FFmpeg lacks --enable-decklink" by
/// the ffmpeg_decklink_available flag on /api/state.
async function fetchDecklinkDevices(force = false) {
  try {
    const url = force ? '/api/decklink-outputs?force=1' : '/api/decklink-outputs';
    const list = await fetchJSON(url);
    knownDecklinkDevices = Array.isArray(list) ? list : [];
  } catch (_e) {
    knownDecklinkDevices = [];
  }
  // Re-render against the last-seen snapshot so the dropdowns update
  // immediately after a refresh click without waiting for the next
  // /api/state poll tick.
  if (lastSnapshot) renderDecklinkDestination(lastSnapshot);
}

/// Update the DeckLink picker (device + mode dropdowns) from the
/// current snapshot. Also gates visibility of the DeckLink body block
/// and toggles the "FFmpeg lacks DeckLink support" warning.
function renderDecklinkDestination(snap) {
  if (!els.destDecklinkBody) return;

  const ffmpegHasDecklink = !!snap.ffmpeg_decklink_available;
  const destType = snap.destination_type || 'atem';
  const isDecklink = destType === 'decklink';

  // Toggle which body block is visible. The ATEM body is the existing
  // dest-wizard div; the DeckLink body is the new dest-decklink-body
  // sibling.
  if (els.destAtemBody) els.destAtemBody.hidden = isDecklink;
  els.destDecklinkBody.hidden = !isDecklink;

  // Segmented control reflects current state. Disable the DeckLink
  // radio when the FFmpeg build lacks support — surfaced with the
  // dest-decklink-unsupported hint below the segments.
  for (const seg of els.destTypeSegs) {
    if (seg.value === 'decklink') {
      seg.disabled = !ffmpegHasDecklink;
    }
    if (document.activeElement !== seg) {
      seg.checked = (seg.value === destType);
    }
  }
  if (els.destDecklinkUnsupported) {
    els.destDecklinkUnsupported.hidden = ffmpegHasDecklink;
  }

  // Populate the device dropdown. Always include the current pick at
  // the top even if it's not in the latest scan — operator might have
  // a card temporarily unplugged that they want to re-plug; we don't
  // erase their state.
  const devicePicks = knownDecklinkDevices.map((d) => ({
    value: d.name,
    label: d.name,
  }));
  const currentDevice = snap.decklink_device_name || '';
  if (currentDevice && !devicePicks.some((d) => d.value === currentDevice)) {
    devicePicks.unshift({
      value: currentDevice,
      label: `${currentDevice} (not connected)`,
    });
  }
  if (devicePicks.length === 0) {
    devicePicks.push({ value: '', label: '— No DeckLink devices found —' });
  }
  setOptions(els.decklinkDevice, devicePicks, currentDevice);

  // Populate the mode dropdown from the picked device's modes. Empty
  // when no device picked OR when the picked device has no probed
  // modes yet (the per-device probe is lazy — happens server-side on
  // the first /api/decklink-outputs call for the named device).
  const pickedDevice = knownDecklinkDevices.find((d) => d.name === currentDevice);
  const modePicks = (pickedDevice?.modes || []).map((m) => ({
    value: m.format_code,
    label: humanizeDecklinkMode(m),
  }));
  if (modePicks.length === 0) {
    modePicks.push({ value: '', label: '— Pick a device first —' });
  }
  setOptions(els.decklinkMode, modePicks, snap.decklink_format_code || '');

  // Status hint below the device select — surfaces driver/build state.
  if (els.decklinkDeviceStatus) {
    if (!ffmpegHasDecklink) {
      els.decklinkDeviceStatus.textContent =
        'FFmpeg build lacks DeckLink support — reinstall the app.';
    } else if (knownDecklinkDevices.length === 0) {
      els.decklinkDeviceStatus.textContent =
        'No DeckLink devices found. Install the Blackmagic DeckLink Driver and plug in a card.';
    } else {
      els.decklinkDeviceStatus.textContent =
        `${knownDecklinkDevices.length} device${knownDecklinkDevices.length === 1 ? '' : 's'} available.`;
    }
  }

  // Destination aux label in the card title — switch to DeckLink-flavored
  // when DeckLink is active.
  if (isDecklink) {
    if (currentDevice) {
      const modeLabel = snap.decklink_output_mode || snap.decklink_format_code || '—';
      els.destAux.textContent = `DECKLINK → ${currentDevice} · ${modeLabel}`;
    } else {
      els.destAux.textContent = 'DECKLINK · pick a device';
    }
  }
}

/// Build a human-friendly mode label from a DecklinkMode object.
/// Example: "1080p59.94 (Hp59)".
function humanizeDecklinkMode(m) {
  const fps = m.fps_num / Math.max(1, m.fps_den);
  // Trim trailing zeros: 59.94 -> 59.94, 60.00 -> 60.
  const fpsLabel = fps.toFixed(2).replace(/0+$/, '').replace(/\.$/, '');
  const scan = m.interlaced ? 'i' : 'p';
  return `${m.height}${scan}${fpsLabel}  (${m.format_code})`;
}

// alpha.22's UDM config dialog functions removed in alpha.25 — UDM
// is configured inside the net-diag dashboard now (see its UDM
// panel form, POSTs to its own /api/config).

// -----------------------------------------------------------------
// alpha.25 — Advanced encoding controls (encoder picker + audio knobs)
// -----------------------------------------------------------------

/// Populate the Advanced disclosure pickers from the current snapshot
/// + the available_encoders list. Called from the main render() path
/// every poll tick. Idempotent — checks document.activeElement to
/// avoid clobbering an input the operator is mid-typing.
function renderEncodingAdvanced(snap) {
  if (!els.videoEncoder) return;

  // Encoder dropdown — always offer "auto" + whatever the bundled
  // FFmpeg actually has. Filter against snap.available_encoders so
  // we don't list nvenc on a build without it. Encoder names that
  // start with h264_ / hevc_ are surfaced under H.264 / H.265 labels.
  const avail = Array.isArray(snap.available_encoders) ? snap.available_encoders : [];
  const wantedEncoders = [
    'libx264', 'libx265',
    'h264_videotoolbox', 'hevc_videotoolbox',
    'h264_nvenc', 'hevc_nvenc',
    'h264_qsv', 'hevc_qsv',
    'h264_amf', 'hevc_amf',
  ];
  const present = wantedEncoders.filter((e) => avail.includes(e));
  const options = [{ value: 'auto', label: 'Auto (best for platform)' }];
  for (const enc of present) {
    options.push({ value: enc, label: encoderLabel(enc) });
  }
  setOptions(els.videoEncoder, options, snap.video_encoder || 'auto');

  // Hint below the encoder dropdown — describes what auto would
  // pick on this platform / FFmpeg build.
  if (els.videoEncoderHint) {
    if ((snap.video_encoder || 'auto') === 'auto') {
      const autoGuess = guessAutoEncoder(snap.video_codec || 'h265', avail);
      els.videoEncoderHint.textContent = autoGuess
        ? `Auto will pick ${encoderLabel(autoGuess)} for current codec/platform.`
        : 'No matching encoder available in bundled FFmpeg.';
    } else {
      els.videoEncoderHint.textContent = 'Manual override — falls back to auto if not available.';
    }
  }

  // Auto-reconnect toggle (state field exists; supervisor loop lands
  // in alpha.26+, so this toggle currently round-trips the value
  // without changing runtime behavior yet).
  if (els.autoReconnect && document.activeElement !== els.autoReconnect) {
    els.autoReconnect.value = snap.auto_reconnect === false ? 'false' : 'true';
  }

  // Audio knobs.
  if (els.audioCodec && document.activeElement !== els.audioCodec) {
    els.audioCodec.value = snap.audio_codec || 'aac';
  }
  if (els.audioBitrateKbps && document.activeElement !== els.audioBitrateKbps) {
    els.audioBitrateKbps.value = snap.audio_bitrate_kbps || 0;
  }
  if (els.audioSampleRate && document.activeElement !== els.audioSampleRate) {
    els.audioSampleRate.value = String(snap.audio_sample_rate || 48000);
  }
  if (els.audioChannelsSel && document.activeElement !== els.audioChannelsSel) {
    els.audioChannelsSel.value = String(snap.audio_channels || 2);
  }
  if (els.metersEnabled && document.activeElement !== els.metersEnabled) {
    els.metersEnabled.checked = snap.meters_enabled !== false;
  }
  if (els.encoderExtraFlags && document.activeElement !== els.encoderExtraFlags) {
    els.encoderExtraFlags.value = snap.encoder_extra_flags || '';
  }
}

/// Human-readable label for an encoder name. Keeps the dropdown
/// concise but informative ("h264_nvenc" -> "H.264 / NVIDIA NVENC").
function encoderLabel(name) {
  const codec = name.startsWith('hevc_') || name === 'libx265' ? 'H.265' : 'H.264';
  if (name === 'libx264' || name === 'libx265') return `${codec} / libx${codec === 'H.264' ? '264' : '265'} (software)`;
  if (name.includes('videotoolbox')) return `${codec} / VideoToolbox (macOS hardware)`;
  if (name.includes('nvenc')) return `${codec} / NVIDIA NVENC`;
  if (name.includes('qsv')) return `${codec} / Intel QuickSync`;
  if (name.includes('amf')) return `${codec} / AMD AMF`;
  return name;
}

/// Mirror the Rust auto_select_encoder logic so the UI can hint at
/// what auto-mode would pick. Doesn't have to be perfect — this is a
/// preview, not the source of truth (server's select_encoder always
/// makes the actual call at stream-start).
function guessAutoEncoder(codec, available) {
  const isMac = navigator.userAgent.includes('Mac');
  const wantH265 = codec === 'h265';
  const candidates = wantH265
    ? (isMac
        ? ['hevc_videotoolbox', 'hevc_nvenc', 'hevc_qsv', 'hevc_amf', 'libx265']
        : ['hevc_nvenc', 'hevc_qsv', 'hevc_amf', 'libx265'])
    : (isMac
        ? ['h264_videotoolbox', 'h264_nvenc', 'h264_qsv', 'h264_amf', 'libx264']
        : ['h264_nvenc', 'h264_qsv', 'h264_amf', 'libx264']);
  return candidates.find((c) => available.includes(c)) || null;
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
  // Counterpart to the inline display:none we set on successful NDI
  // ticks (cache-bypass defense). Clear it so .hidden = false below
  // actually shows the bars again.
  els.previewBars.style.display = '';
  els.previewMessage.style.display = '';
  els.previewBars.hidden = false;
  els.previewMessage.hidden = true;
  // Reset the dedup key so any future call to startNdiPreview /
  // startCameraPreview / etc. for the SAME source actually re-runs
  // (otherwise the early-return on previewKey-match leaves us
  // permanently stuck after stopPreview).
  previewKey = '';
}

function showPreviewMessage(html) {
  stopPreview();
  els.previewMessage.innerHTML = html;
  els.previewMessage.hidden = false;
  els.previewBars.hidden = true;
}

// Sync the Audio Mixer card's mode + output radios from state and
// drive the Custom panel's visibility. Called from render() on every
// state poll so the controls stay in sync if something changes the
// backend audio_mode out of band (settings paste, /api/settings POST
// from elsewhere, etc.).
function updateAudioMixer(snap) {
  const mode = snap.audio_mode || 'auto';
  els.audioModeRadios.forEach((r) => { r.checked = (r.value === mode); });
  if (els.audioCustom) els.audioCustom.hidden = mode !== 'custom';

  const wantMono = !!snap.audio_output_mono;
  els.audioOutputRadios.forEach((r) => {
    r.checked = (r.value === (wantMono ? 'mono' : 'stereo'));
  });
}

// Show/hide the multi-channel audio pan picker based on whether the
// active audio device looks like a multi-channel device (Dante VSC,
// CoreAudio aggregate) AND we're in Custom mode (Auto/Silent skip
// the AVF audio device entirely). Same heuristic the streamer uses
// on the Rust side so the UI lines up exactly with when the FFmpeg
// pan filter fires.
function updateAudioPanRow(snap) {
  if (!els.audioPanRow) return;
  const name = (snap.av_audio_name || '').toLowerCase();
  const inCustom = (snap.audio_mode || 'auto') === 'custom';
  const isMultichannel = inCustom &&
    (snap.source_id === 'avfoundation') &&
    (name.includes('dante') || name.includes('aggregate'));
  els.audioPanRow.hidden = !isMultichannel;
  if (!isMultichannel) return;
  // Don't clobber a value the user is currently editing.
  if (document.activeElement !== els.audioPanL) {
    els.audioPanL.value = snap.audio_pan_l || 1;
  }
  if (document.activeElement !== els.audioPanR) {
    els.audioPanR.value = snap.audio_pan_r || 2;
  }
}

// Sync the Preview / Stop Preview button to backend preview state +
// source kind. Called from render() on every state poll so the label
// stays accurate even when something changes preview state out of
// band (e.g. starting a stream tears the preview down server-side).
/// Show/hide the top-of-page "receiver running" banner and mirror the
/// wizard's running state. Called every poll tick from render(snap).
///
/// State machine:
///   - source != relay → banner hidden, wizard back to default state.
///   - source = relay, status = Idle/Interrupted → hidden (the wizard
///     already shows error feedback in its own status panel).
///   - source = relay, status = Connecting → yellow "waiting" banner
///     (listener bound, no encoder pushing yet).
///   - source = relay, status = Streaming → green "live" banner with
///     bitrate; wizard summary glows.
///
/// The banner also acts as the canonical Stop control while the
/// receiver is running, so the user doesn't have to scroll all the
/// way down to the wizard to stop a stream.
function updateReceiverUi(snap) {
  const banner = els.receiverBanner;
  const wizard = els.receiveWizard;
  if (!banner) return;

  const isRelay = snap && (snap.source_id === 'srt_listen' || snap.source_id === 'rtmp_listen');
  const status = (snap && snap.stats && snap.stats.status) || 'Idle';
  const active = isRelay && (status === 'Streaming' || status === 'Connecting');

  if (wizard) wizard.classList.toggle('is-running', !!active);

  if (!active) {
    banner.hidden = true;
    return;
  }

  const proto = snap.source_id === 'srt_listen' ? 'SRT' : 'RTMP';
  // Reuse whatever URL the wizard already showed the user — they
  // copy/pasted it into the encoder, so it's the canonical "what to
  // connect to" string. Falls back to a generic constructed URL when
  // the wizard hasn't rendered yet (rare; only on the very first
  // tick before bind() has run).
  const wizardUrl = (document.getElementById('rw-publish-url') || {}).value || '';
  const r = snap.relay || {};
  const fallbackHost = lanIp || '<your-lan-ip>';
  const fallbackUrl = proto === 'SRT'
    ? `srt://${fallbackHost}:${r.srt_port ?? 9710}`
    : `rtmp://${fallbackHost}:${r.rtmp_port ?? 1935}/${r.rtmp_app || 'live'}/${r.rtmp_key || 'stream'}`;
  const url = wizardUrl || fallbackUrl;

  banner.classList.remove('is-waiting', 'is-error');
  els.receiverBannerUrl.textContent = url;

  if (status === 'Connecting') {
    // Listener bound but no encoder pushing yet. Yellow pulse +
    // "waiting" copy. The banner is meant to read "the server is up
    // and ready — your job is to get the camera to connect."
    banner.classList.add('is-waiting');
    els.receiverBannerTitle.textContent = `${proto} server listening — no encoder yet`;
    els.receiverBannerSubtitle.textContent = 'Paste the URL into your camera / drone / OBS / phone now. The banner turns green and shows the bitrate as soon as frames start flowing.';
  } else {
    // Streaming — frames are actually flowing through to ATEM.
    // Solid green pulse, bitrate readout, "you're done — don't touch
    // anything" copy.
    const stats = snap.stats || {};
    const kbps = Math.round((stats.bitrate || 0) / 1000);
    const fps  = stats.fps ? `${Math.round(stats.fps)} fps` : '';
    const live = kbps > 0
      ? `Live · ${kbps} kbps${fps ? ' · ' + fps : ''}`
      : 'Live · connection up';
    els.receiverBannerTitle.textContent = `${proto} server — encoder connected, forwarding to ATEM`;
    els.receiverBannerSubtitle.textContent = `${live}. Do NOT click Start Stream above — the relay handles the ATEM forward automatically.`;
  }
  banner.hidden = false;
}

function updatePreviewButton(snap) {
  if (!els.previewBtn) return;
  const isActive = snap.preview && snap.preview.active;
  const supported = snap.source_id === 'ndi' && !!snap.ndi_source_name;
  const isStreaming = snap.stats && (snap.stats.status === 'Streaming' || snap.stats.status === 'Connecting');

  if (isActive) {
    els.previewBtn.textContent = '◼ Stop Preview';
    els.previewBtn.classList.add('preview-active');
    els.previewBtn.disabled = false;
    els.previewBtn.title = '';
  } else {
    els.previewBtn.textContent = '▶ Preview';
    els.previewBtn.classList.remove('preview-active');
    els.previewBtn.disabled = !supported || isStreaming;
    if (isStreaming) {
      els.previewBtn.title = 'Streaming — preview pane is already showing the live encoder output.';
    } else if (supported) {
      els.previewBtn.title = 'Spin up an NDI receiver so you can see the source before starting the stream.';
    } else {
      els.previewBtn.title = 'Pre-stream preview is only available for NDI sources today (FFmpeg-snapshot backend for cameras / pipes lands in a follow-up).';
    }
  }
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
  // OMT preview piggybacks on the same /api/preview endpoint as NDI.
  // The server's preview waterfall returns whichever capture is
  // active (NDI > OMT > pre-stream Preview backend). In alpha.9
  // OMT preview JPEGs aren't generated (libomt-rs 0.1.3 doesn't
  // expose frame stride accessors cleanly enough to sample),
  // so this falls through to the bridge-waiting hint when the
  // user is on an OMT source — but the start-stream path still
  // works and the streaming output to ATEM is visible.
  if (tile.sourceId === 'omt-sender' || tile.sourceId === 'omt') {
    return startNdiPreview(tile.name);
  }
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
  console.log('[ndi-preview] startNdiPreview:', senderName);
  stopPreview();
  previewKey = key;

  if (!ndiPreviewImg) {
    ndiPreviewImg = document.createElement('img');
    ndiPreviewImg.id = 'preview-ndi';
    ndiPreviewImg.alt = 'NDI preview';
    // position:absolute so we overlay the SMPTE bars the same way
    // #preview-video does. Without this, the img would flow inline
    // and the bars would still be visible alongside it. z-index 2
    // beats the .live-badge (z-index 1 implicit) and any other
    // children of .preview-frame.
    ndiPreviewImg.style.cssText =
      'position:absolute;inset:0;width:100%;height:100%;object-fit:contain;background:#000;z-index:2;';
    els.previewFrame.appendChild(ndiPreviewImg);
    console.log('[ndi-preview] img element created and appended');
  }
  ndiPreviewImg.hidden = false;
  els.previewBars.hidden = true;
  els.previewVideo.hidden = true;
  els.previewMessage.hidden = true;

  if (ndiPreviewTimer) clearInterval(ndiPreviewTimer);
  let nullStreak = 0;
  let tickCount = 0;
  const tick = async () => {
    tickCount += 1;
    try {
      const r = await fetch(`/api/preview?ts=${Date.now()}`);
      // Per-tick diagnostic every ~2.5s. Drop to >= 999999 to silence
      // once we've nailed down the bug.
      if (tickCount % 5 === 0) {
        console.log(`[ndi-preview] tick #${tickCount}: HTTP ${r.status}, ok=${r.ok}, nullStreak=${nullStreak}`);
      }
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
      const wasFirstFrame = nullStreak !== 0 || !ndiPreviewImg.dataset.objectUrl;
      nullStreak = 0;
      // Show the image again if the waiting-message fallback was rendered.
      if (ndiPreviewImg.hidden) {
        els.previewMessage.hidden = true;
        els.previewBars.hidden = true;
        ndiPreviewImg.hidden = false;
      }
      // Defensive: also hide the bars on every successful tick. If the
      // CSS [hidden] !important rule isn't loaded yet (stale cache),
      // setting hidden=true alone wouldn't actually hide the bars and
      // they'd bleed through. Setting style.display = 'none' inline
      // beats any cascade.
      els.previewBars.style.display = 'none';
      els.previewMessage.style.display = 'none';
      const blob = await r.blob();
      // Object URL avoids re-encoding the JPEG bytes through base64.
      const next = URL.createObjectURL(blob);
      const prev = ndiPreviewImg.dataset.objectUrl;
      ndiPreviewImg.src = next;
      if (prev) URL.revokeObjectURL(prev);
      ndiPreviewImg.dataset.objectUrl = next;
      if (wasFirstFrame) {
        console.log(`[ndi-preview] first JPEG displayed: ${blob.size} bytes`);
      }
    } catch (e) {
      // Was a silent catch — one of these threw and we never knew:
      // fetch, .blob(), URL.createObjectURL, or DOM mutation.
      console.error('[ndi-preview] tick threw:', e);
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
  // NDI senders go BETWEEN screens and NDI Bridge — direct NDI ingest
  // is the recommended path on this app, so it should surface above
  // the legacy Virtual Camera bridge tiles. Synthetic 'ndi_senders'
  // entry in the order array slots them in cleanly without splitting
  // the loop.
  const order = [
    ['camera',       'Cameras'],
    ['capture_card', 'Capture cards'],
    ['iphone',       'iPhone (Continuity)'],
    ['screen',       'Screens'],
    ['ndi_senders',  'NDI senders on your network'],
    ['omt_senders',  'OMT senders on your network'],
    ['ndi',          'NDI Bridge (NDI Virtual Camera)'],
    ['virtual',      'Virtual cameras'],
  ];
  for (const [cat, label] of order) {
    if (cat === 'ndi_senders') {
      for (const sender of knownNdi) {
        tiles.push({
          sourceId: 'ndi-sender',
          avIndex: null,
          name: `${sender.source || sender.name} · ${sender.machine || ''}`.replace(/\s·\s$/, ''),
          category: 'ndi',
          section: label,
          discovered: true,
        });
      }
      continue;
    }
    if (cat === 'omt_senders') {
      // alpha.24 reversed alpha.13's "hide OMT when empty" — the
      // hide-when-empty UX made OMT undiscoverable for users who
      // didn't know the feature existed. Always show the section
      // (matches NDI's "scan NDI" affordance in the card title) and
      // render either the discovered senders OR a single
      // placeholder tile that explains how to populate the list.
      if (knownOmt.length > 0) {
        for (const sender of knownOmt) {
          tiles.push({
            sourceId: 'omt-sender',
            avIndex: null,
            name: sender.name,
            category: 'omt',
            section: label,
            discovered: true,
          });
        }
      } else {
        // Empty-state placeholder. Non-selectable (the click handler
        // checks `placeholder` and no-ops). Tells the operator the
        // feature exists and how to populate it.
        tiles.push({
          sourceId: 'omt-placeholder',
          avIndex: null,
          name: 'No OMT senders found',
          category: 'omt',
          section: label,
          placeholder: true,
        });
      }
      continue;
    }
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

  tiles.push({
    sourceId: 'pipe', avIndex: null,
    name: snap.pipe_path ? snap.pipe_path.split('/').pop() : 'URL / Pipe',
    category: 'pipe', section: 'URL / Pipe',
  });

  // Relay-listener tiles (srt_listen / rtmp_listen) were folded into
  // the Receive-a-Stream wizard below — single colored CTA expands
  // the protocol picker / publish URL / app instructions instead of
  // duplicating it as gallery tiles.

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
    // NDI/OMT tiles: state.source_id is "ndi"/"omt" but the tile's
    // sourceId is "ndi-sender"/"omt-sender" (the discovery list);
    // match by ndi_source_name / omt_source_name so the right sender
    // within the discovered set highlights.
    const isActive =
      ((snap.source_id === t.sourceId) ||
       (snap.source_id === 'ndi' && t.sourceId === 'ndi-sender' && snap.ndi_source_name === t.name) ||
       (snap.source_id === 'omt' && t.sourceId === 'omt-sender' && snap.omt_source_name === t.name)) &&
      (t.sourceId !== 'avfoundation' || snap.av_video_index === t.avIndex);
    const div = document.createElement('div');
    div.className = 'tile'
      + (isActive ? ' active' : '')
      + (t.discovered ? ' discovered' : '')
      + (t.placeholder ? ' placeholder' : '');
    if (t.placeholder) {
      // Non-selectable empty-state tile (e.g. "No OMT senders found").
      // Slightly muted styling via .placeholder CSS; hint text in tile-cat.
      div.innerHTML = `
        <div class="tile-icon">${ICONS[t.category] || ICONS.camera}</div>
        <div class="tile-name" title="${escapeHtml(t.name)}">${escapeHtml(t.name)}</div>
        <div class="tile-cat">click "scan ${(t.category || '').toUpperCase()}" above to refresh</div>
      `;
      // No click handler — placeholder is informational.
    } else {
      div.innerHTML = `
        <div class="tile-icon">${ICONS[t.category] || ICONS.camera}</div>
        <div class="tile-name" title="${escapeHtml(t.name)}">${escapeHtml(t.name)}</div>
        <div class="tile-cat">${CATEGORY_LABEL[t.category] || t.category}</div>
      `;
      div.addEventListener('click', () => selectSource(t));
    }
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
  if (t.sourceId === 'omt-sender') {
    // alpha.9: same handling as NDI — switch source_id, persist the
    // sender name. Note: OMT preview is no-op in alpha.9 (libomt-rs
    // 0.1.3 doesn't surface frame stride/rate cleanly enough to
    // sample preview JPEGs); the streaming output to ATEM still
    // works once Start Stream fires.
    applySettings({ source_id: 'omt', omt_source_name: t.name });
    setPreviewFor({ ...t, sourceId: 'omt' });
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

  // Session 12 — DeckLink destination toggling. Renders last so it
  // can overwrite destAux with the DeckLink-flavored label when the
  // operator has switched to a DeckLink output. Otherwise hides the
  // DeckLink body and lets the ATEM body keep its rendering above.
  renderDecklinkDestination(snap);

  // alpha.25 — encoder picker + audio knobs + auto-reconnect toggle.
  renderEncodingAdvanced(snap);

  // alpha.22's renderUdmConfigStatus removed in alpha.25 — UDM lives
  // inside the net-diag dashboard now.

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

  updatePreviewButton(snap);
  updateReceiverUi(snap);

  if (knownDevices.audio.length) {
    setOptions(els.avAudio, [
      ...(snap.source_id === 'avfoundation' ? [] : [{ value: '-1', label: '— (auto / not used)' }]),
      ...knownDevices.audio.map((d) => ({ value: d.index, label: `[${d.index}] ${d.name}` })),
    ], snap.av_audio_index);
  }
  updateAudioMixer(snap);
  updateAudioPanRow(snap);
  setOptions(els.videoMode, snap.available_video_modes, snap.video_mode);
  if (els.formatDecoded) els.formatDecoded.textContent = decodeVideoMode(snap.video_mode);
  setOptions(els.quality, snap.available_quality_levels || [], snap.quality_level);
  renderQualitySegmented(snap);
  if (document.activeElement !== els.pipePath) els.pipePath.value = snap.pipe_path || '';
  els.pipeOnly.forEach((e) => (e.hidden = snap.source_id !== 'pipe'));

  const ov = snap.overlay || {};
  if (document.activeElement !== els.ovTitle) els.ovTitle.value = ov.title || '';
  if (document.activeElement !== els.ovSubtitle) els.ovSubtitle.value = ov.subtitle || '';
  if (document.activeElement !== els.ovLogo) els.ovLogo.value = ov.logo_path || '';
  if (document.activeElement !== els.ovClock) els.ovClock.checked = !!ov.clock;

  // Status pill + body class for streaming-state animations
  const stats = snap.stats;
  // alpha.30: during auto-reconnect backoff the supervisor sets
  // status = "Reconnecting" with reconnect_attempt / reconnect_next_secs
  // populated. Surface the countdown so operators can see the loop
  // is alive and know how long until the next retry. Max is from
  // snap.auto_reconnect_max_attempts (12 default).
  const max = snap.auto_reconnect_max_attempts || 12;
  if (stats.status === 'Reconnecting') {
    els.statusPill.textContent = `RECONNECTING IN ${stats.reconnect_next_secs || 0}s · ${stats.reconnect_attempt || 0}/${max}`;
  } else {
    els.statusPill.textContent = stats.status.toUpperCase();
  }
  els.statusPill.classList.remove('streaming', 'connecting', 'interrupted', 'reconnecting');
  if (stats.status === 'Streaming') els.statusPill.classList.add('streaming');
  else if (stats.status === 'Connecting') els.statusPill.classList.add('connecting');
  else if (stats.status === 'Interrupted') els.statusPill.classList.add('interrupted');
  else if (stats.status === 'Reconnecting') els.statusPill.classList.add('reconnecting');

  els.body.classList.toggle('is-streaming', stats.status === 'Streaming');
  els.liveBadge.hidden = stats.status !== 'Streaming';

  els.duration.textContent = stats.duration;
  els.monitorAux.textContent = stats.status === 'Streaming'
    ? `live · ${Math.round((stats.bitrate || 0) / 1000)} kbps`
    : stats.status === 'Connecting' ? 'connecting…'
    : stats.status === 'Reconnecting' ? `reconnecting · ${stats.total_reconnects_this_session || 0} drops this session`
    : sourceLabel(snap);

  const cfg = snap.active_config;
  els.ovlSource.textContent  = sourceLabel(snap);
  els.ovlRes.textContent     = snap.video_mode || '—';
  els.ovlProfile.textContent = cfg ? `${snap.quality_level} · ${Math.round(cfg.bitrate / 1000)} kbps` : '—';
  els.ovlBitrate.textContent = `${Math.round((stats.bitrate || 0) / 1000)} kbps`;

  renderTelemetry(snap);
  renderOmtOutput(snap);

  // Wizard sync: lock/unlock the protocol radio + advanced inputs and
  // pull initial values out of the snapshot. window.* bridges exist
  // because the helpers are scoped inside bind(); calling them here
  // gives users an instant reaction to receiver-state changes instead
  // of waiting up to one second for the wizard's own poll tick.
  if (typeof window.hydrateRwAdvancedFromSnapshot === 'function') {
    window.hydrateRwAdvancedFromSnapshot(snap);
  }
  if (typeof window.syncRwProtocolWithSnapshot === 'function') {
    window.syncRwProtocolWithSnapshot(snap);
  }

  if (stats.error) {
    els.error.hidden = false;
    els.error.textContent = stats.error;
  } else {
    els.error.hidden = true;
    els.error.textContent = '';
  }
}

// Sync the OMT Output card's controls to backend state, plus render
// a status line that tells the user what's actually happening:
//  - "Currently disabled." (off)
//  - "Currently enabled — will publish as <name>." (on, idle)
//  - "Publishing as <name>." (on, streaming + source supports it)
//  - "Enabled but won't fire — source <X> doesn't produce raw frames."
//    (on, but source is AVF/pipe/relay)
function renderOmtOutput(snap) {
  if (!els.omtOutputEnabled) return;
  const enabled = !!snap.omt_output_enabled;
  if (els.omtOutputEnabled.checked !== enabled) {
    els.omtOutputEnabled.checked = enabled;
  }
  if (document.activeElement !== els.omtOutputName) {
    els.omtOutputName.value = snap.omt_output_name || 'ATEM Patchbay';
  }
  let msg;
  if (!enabled) {
    msg = 'Currently disabled.';
  } else {
    const sourceOk = snap.source_id === 'ndi' || snap.source_id === 'omt';
    const isLive = (snap.stats && snap.stats.status === 'Streaming');
    if (!sourceOk) {
      msg = `Enabled, but won't fire — source "${snap.source_id || 'none'}" doesn't produce raw frames in alpha.9. Switch to an NDI/OMT source.`;
    } else if (isLive) {
      msg = `Publishing as "${snap.omt_output_name || 'ATEM Patchbay'}".`;
    } else {
      msg = `Currently enabled — will publish as "${snap.omt_output_name || 'ATEM Patchbay'}" once you start the stream.`;
    }
  }
  if (els.omtOutputStatus) {
    els.omtOutputStatus.textContent = msg;
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

// alpha.9: parallel of ensureNdiLoaded for OMT senders. The endpoint
// always exists on the server side; when the `omt` cargo feature is
// off (default for prebuilt releases), it returns an empty list and
// the OMT section in the tile gallery stays hidden. No client-side
// feature detection needed — empty list = no section, full list =
// section appears.
async function ensureOmtLoaded(force = false) {
  try {
    const u = force ? '/api/omt-senders?force=1' : '/api/omt-senders';
    const j = await fetchJSON(u);
    knownOmt = j.senders || [];
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

  // Session 12 — destination-type segmented control
  els.destTypeSegs.forEach((r) => {
    r.addEventListener('change', () => {
      if (!r.checked) return;
      // Fetch DeckLink devices on first switch into DeckLink mode so
      // the dropdown isn't empty when the body becomes visible. Idempotent
      // — if the device list is already populated this is a no-op refresh.
      if (r.value === 'decklink') fetchDecklinkDevices(false);
      applySettings({ destination_type: r.value });
    });
  });
  // Device picker — also clears the picked mode so the user doesn't
  // accidentally try to drive Card B with Card A's mode code.
  els.decklinkDevice.addEventListener('change', () => {
    applySettings({
      decklink_device_name: els.decklinkDevice.value,
      decklink_format_code: '',
      decklink_output_mode: '',
    });
  });
  els.decklinkMode.addEventListener('change', () => {
    // Look up the human-friendly mode label from the picked device's
    // modes so snapshot's decklink_output_mode stays in sync with the
    // dropdown. Fall back to the format_code itself if the device
    // isn't in our local cache (shouldn't happen — defensive).
    const pickedCode = els.decklinkMode.value;
    const dev = knownDecklinkDevices.find(
      (d) => d.name === els.decklinkDevice.value,
    );
    const mode = dev?.modes?.find((m) => m.format_code === pickedCode);
    applySettings({
      decklink_format_code: pickedCode,
      decklink_output_mode: mode ? humanizeDecklinkMode(mode) : pickedCode,
    });
  });
  if (els.decklinkRefresh) {
    els.decklinkRefresh.addEventListener('click', () => fetchDecklinkDevices(true));
  }

  // alpha.22's UDM dialog wiring removed in alpha.25 — UDM moved into
  // the net-diag dashboard.

  // alpha.25 — Advanced encoding control wiring.
  if (els.videoEncoder) {
    els.videoEncoder.addEventListener('change', () => {
      applySettings({ video_encoder: els.videoEncoder.value });
    });
  }
  if (els.autoReconnect) {
    els.autoReconnect.addEventListener('change', () => {
      applySettings({ auto_reconnect: els.autoReconnect.value === 'true' });
    });
  }
  if (els.audioCodec) {
    els.audioCodec.addEventListener('change', () => {
      applySettings({ audio_codec: els.audioCodec.value });
    });
  }
  if (els.audioBitrateKbps) {
    els.audioBitrateKbps.addEventListener('change', () => {
      const v = parseInt(els.audioBitrateKbps.value, 10);
      applySettings({ audio_bitrate_kbps: isNaN(v) ? 0 : v });
    });
  }
  if (els.audioSampleRate) {
    els.audioSampleRate.addEventListener('change', () => {
      applySettings({ audio_sample_rate: parseInt(els.audioSampleRate.value, 10) });
    });
  }
  if (els.audioChannelsSel) {
    els.audioChannelsSel.addEventListener('change', () => {
      applySettings({ audio_channels: parseInt(els.audioChannelsSel.value, 10) });
    });
  }
  if (els.metersEnabled) {
    els.metersEnabled.addEventListener('change', () => {
      applySettings({ meters_enabled: !!els.metersEnabled.checked });
    });
  }
  if (els.encoderExtraFlags) {
    // Save on blur (matches the SRT-mode / streamid-override fields'
    // existing pattern). Avoids POSTing on every keystroke.
    els.encoderExtraFlags.addEventListener('blur', () => {
      applySettings({ encoder_extra_flags: els.encoderExtraFlags.value });
    });
  }

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
  // Audio channel pan (multi-channel devices like Dante VSC).
  // 'change' fires on blur or Enter — fine for number inputs since
  // partial typing shouldn't fire a settings update mid-keystroke.
  // Audio mode + output radios. Mode change drives the Custom panel
  // visibility (and on the backend, the audio routing). Output is
  // independent — stereo/mono just adjusts -ac on the encoder.
  els.audioModeRadios.forEach((r) => {
    r.addEventListener('change', () => {
      if (!r.checked) return;
      applySettings({ audio_mode: r.value });
      els.audioCustom.hidden = r.value !== 'custom';
    });
  });
  els.audioOutputRadios.forEach((r) => {
    r.addEventListener('change', () => {
      if (!r.checked) return;
      applySettings({ audio_output_mono: r.value === 'mono' });
    });
  });

  // Kill orphan FFmpeg streams from prior runs (typically after a
  // cargo tauri dev rebuild SIGKILLed the binary mid-stream and
  // FFmpeg, in its own process group, kept pushing). Confirm-then-
  // hit so the click can't fire by accident.
  if (els.killOrphans) {
    els.killOrphans.addEventListener('click', async () => {
      const ok = confirm(
        "Kill any leftover FFmpeg streams from prior runs of this app?\n\n" +
        "If a previous run crashed, was force-quit, or got rebuilt while " +
        "streaming, the FFmpeg subprocess can keep streaming to your ATEM " +
        "in the background. The Stop button can't reach those (the parent " +
        "process is gone). This finds and kills any leftover FFmpeg " +
        "streams pushing this app's BMD-flavored SRT URLs.\n\n" +
        "Safe to run anytime — won't touch unrelated FFmpeg jobs."
      );
      if (!ok) return;
      els.killOrphans.disabled = true;
      try {
        const r = await fetch('/api/kill-orphans', { method: 'POST' });
        const j = await r.json();
        if (j.error) {
          alert('Kill failed: ' + j.error);
        } else {
          alert(j.message || ('Killed: ' + (j.killed ?? '?')));
        }
      } catch (e) {
        alert('Kill failed: ' + e.message);
      } finally {
        els.killOrphans.disabled = false;
      }
    });
  }

  // Force Stop ALL — emergency escape hatch. OS-kills every FFmpeg on
  // the machine (lock-free, works even when a tile's controls are
  // wedged), then resets all tiles to Idle. The guaranteed-to-work
  // recovery path when Stop / Kill-orphans aren't enough.
  if (els.forceStopAll) {
    els.forceStopAll.addEventListener('click', async () => {
      const ok = confirm(
        "FORCE STOP ALL streams?\n\n" +
        "This kills EVERY FFmpeg process on this machine at the OS level " +
        "— including any unrelated FFmpeg jobs — then resets all tiles to " +
        "Idle. Use it when a stream is wedged and Stop / Kill-orphans " +
        "won't clear it.\n\n" +
        "On a dedicated broadcast machine this is safe and is the " +
        "guaranteed way to recover."
      );
      if (!ok) return;
      els.forceStopAll.disabled = true;
      try {
        const r = await fetch('/api/force-stop-all', { method: 'POST' });
        const j = await r.json();
        alert(j.message || ('Force-stopped. Killed: ' + (j.killed ?? '?')));
      } catch (e) {
        alert('Force stop failed: ' + e.message);
      } finally {
        els.forceStopAll.disabled = false;
      }
    });
  }

  // Open ATEM Net Diag (companion app). The backend tries to launch
  // the .app bundle on macOS first (matches by display name), then
  // opens http://localhost:8092 in the default browser regardless.
  // If neither the .app nor a running net-diag binary is found, the
  // browser shows a connection error and the user knows to grab the
  // tarball from the Releases page. The brief disabled-state on the
  // button gives visual feedback that the click fired.
  if (els.openNetDiag) {
    els.openNetDiag.addEventListener('click', async () => {
      const orig = els.openNetDiag.textContent;
      els.openNetDiag.disabled = true;
      els.openNetDiag.textContent = 'Opening…';
      try {
        const r = await fetch('/api/open-net-diag', { method: 'POST' });
        const j = await r.json();
        if (j.error && !j.opened_url) {
          // Hard failure — neither the .app nor the URL opened.
          alert(
            'Could not open ATEM Net Diag:\n\n' + j.error +
            '\n\nDownload the latest build from:\n' +
            'https://github.com/amateurmenace/atem-ip-patchbay/releases'
          );
        }
        // Soft case (URL opened but .app wasn't installed) is fine —
        // the browser shows the dashboard if net-diag was already
        // running, or a connection error pointing at the right port.
      } catch (e) {
        alert('Could not reach the backend to open Net Diag: ' + e.message);
      } finally {
        els.openNetDiag.textContent = orig;
        els.openNetDiag.disabled = false;
      }
    });
  }

  // alpha.52: openMonitor button + click handler removed. The
  // alpha.16 single-tile companion window pattern is superseded by
  // the alpha.40+ multiview UI which monitors all 4 tiles in one
  // view with richer per-tile controls + the alpha.50 expandable
  // stats panel. The Tauri open_monitor_window command stays
  // registered (low cost, no surface) so existing operator scripts
  // that invoke it directly still work.

  // alpha.42 — view-toggle was previously a separate "Open Multiview"
  // button; now it's the .view-toggle in the topbar (.view-toggle-btn
  // anchor tags) which uses default navigation. Nothing to wire here.

  // alpha.50 — system health pill + drawer. Pill polls /api/system-health
  // at 2 Hz to color-code itself; clicking opens the slide-out drawer
  // with per-core CPU, FFmpeg processes, encoder inventory, log tail.
  // Same drawer component multiview.html uses (see system-drawer.js).
  const sysPillEl = $('#sys-pill');
  if (sysPillEl && typeof window.createSystemDrawer === 'function') {
    window.createSystemDrawer({
      anchorEl: sysPillEl,
      getLogUrl: () => '/api/log',
    });
    async function pollSystemHealth() {
      try {
        const r = await fetch('/api/system-health', { cache: 'no-store' });
        if (!r.ok) return;
        const data = await r.json();
        const status = data.status || 'unknown';
        sysPillEl.className = 'sys-pill-topbar ' + status;
        const cpu = Math.round(data.cpu_percent || 0);
        const memPct = Math.round(data.mem_percent || 0);
        const lbl = $('#sys-pill-label');
        if (lbl) lbl.textContent = `CPU ${cpu}% · MEM ${memPct}%`;
        const tooltip = [
          `Status: ${status.toUpperCase()}`,
          `CPU: ${cpu}% (${data.cpu_count || '?'} cores)`,
          `Memory: ${memPct}% — ${((data.mem_used_mb || 0) / 1024).toFixed(1)} / ${((data.mem_total_mb || 0) / 1024).toFixed(1)} GB`,
          data.warning || null,
          '',
          'Click for full system monitor',
        ].filter(Boolean).join('\n');
        sysPillEl.title = tooltip;
      } catch (e) { /* silent */ }
    }
    pollSystemHealth();
    setInterval(pollSystemHealth, 2000);
  }

  // alpha.55 — Hide About is session-only (no localStorage persist).
  // App always launches with the About section visible; operator can
  // hide it for the current session via the topbar toggle or the
  // in-hero × button, but next reload/relaunch starts visible again.
  // Inline <head> script clears any old persisted flag.
  function setHeroHidden(hidden) {
    document.documentElement.dataset.heroHidden = hidden ? 'true' : 'false';
    // Re-sync the toggle button's label so the operator sees the
    // action that will happen on next click.
    syncIntroToggleLabel();
  }
  function syncIntroToggleLabel() {
    if (!els.introToggleBtn) return;
    const hidden = document.documentElement.dataset.heroHidden === 'true';
    els.introToggleBtn.textContent = hidden ? '▴ Show about' : '▼ Hide about';
    els.introToggleBtn.title = hidden ? 'Show the About section' : 'Hide the About section';
  }
  if (els.heroHideBtn) {
    els.heroHideBtn.addEventListener('click', () => setHeroHidden(true));
  }
  if (els.introToggleBtn) {
    els.introToggleBtn.addEventListener('click', () => {
      const currentlyHidden = document.documentElement.dataset.heroHidden === 'true';
      setHeroHidden(!currentlyHidden);
    });
    // Initial label sync (data attribute was set by the inline <head>
    // script before the page paint).
    syncIntroToggleLabel();
  }

  els.audioPanL.addEventListener('change', () => {
    const v = Math.max(1, parseInt(els.audioPanL.value, 10) || 1);
    els.audioPanL.value = v;
    applySettings({ audio_pan_l: v });
  });
  els.audioPanR.addEventListener('change', () => {
    const v = Math.max(1, parseInt(els.audioPanR.value, 10) || 1);
    els.audioPanR.value = v;
    applySettings({ audio_pan_r: v });
  });
  els.pipePath.addEventListener('change', () => applySettings({ pipe_path: els.pipePath.value }));
  els.rescanDevices.addEventListener('click', (e) => { e.preventDefault(); ensureDevicesLoaded(true); });
  els.ndiRescan.addEventListener('click', (e) => { e.preventDefault(); ensureNdiLoaded(true); });
  if (els.omtRescan) {
    els.omtRescan.addEventListener('click', (e) => { e.preventDefault(); ensureOmtLoaded(true); });
  }

  // OMT output toggle + sender name. POSTs to /api/omt-output which
  // updates state.omt_output_enabled / state.omt_output_name. The
  // streamer reads these at start time and tees raw frames to an
  // OmtSender concurrently with the main FFmpeg encode.
  if (els.omtOutputEnabled) {
    els.omtOutputEnabled.addEventListener('change', async () => {
      const enabled = els.omtOutputEnabled.checked;
      try {
        const r = await fetch('/api/omt-output', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ enabled }),
        });
        const j = await r.json();
        render(j);
      } catch (e) { console.error('omt-output toggle failed:', e); }
    });
  }
  if (els.omtOutputName) {
    els.omtOutputName.addEventListener('change', async () => {
      const name = (els.omtOutputName.value || '').trim();
      if (!name) return;
      try {
        const r = await fetch('/api/omt-output', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ name }),
        });
        const j = await r.json();
        render(j);
      } catch (e) { console.error('omt-output name update failed:', e); }
    });
  }
  els.videoMode.addEventListener('change', () => applySettings({ video_mode: els.videoMode.value }));
  els.quality.addEventListener('change', () => applySettings({ quality_level: els.quality.value }));

  // Relay panel inputs were removed along with the .relay-only blocks
  // — the receive-stream wizard above now drives everything. Relay
  // settings (port, latency, passphrase, RTMP app/key) stay at server
  // defaults; if a user needs to override, they can POST to
  // /api/settings { relay: { ... } } directly until the wizard grows
  // an "advanced" subsection.

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
    // Guard against the most-reported confusion: a user clicked
    // "Start Receiver" in the wizard (which already begins forwarding
    // to ATEM as soon as their encoder connects) and then expects
    // they ALSO need to click Start Stream up here. /api/start would
    // return "Stream already running" — but that error in the global
    // banner reads as if something is broken, when actually the relay
    // is running fine. Surface a clear "you're already streaming via
    // the receiver" message instead and bail without calling the API.
    const snap = lastSnapshot;
    const isRelayRunning = snap
      && (snap.source_id === 'srt_listen' || snap.source_id === 'rtmp_listen')
      && snap.stats
      && (snap.stats.status === 'Streaming' || snap.stats.status === 'Connecting');
    if (isRelayRunning) {
      els.error.hidden = false;
      els.error.textContent = `Receiver is already running — your encoder's stream forwards to your ATEM automatically. Use the green banner above (or the Stop Receiver button in the wizard below) to stop it.`;
      setTimeout(() => (els.startBtn.disabled = false), 600);
      return;
    }
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

  // "Receive a Stream" jump button in the player controls. Opens the
  // receive-stream wizard at the bottom of the right column and
  // smooth-scrolls so it's actually visible — without this users had
  // to discover the wizard by scrolling. Also focuses the protocol
  // toggle so keyboard users land somewhere useful.
  if (els.receiveJumpBtn && els.receiveWizard) {
    els.receiveJumpBtn.addEventListener('click', () => {
      els.receiveWizard.open = true;
      els.receiveWizard.scrollIntoView({ behavior: 'smooth', block: 'center' });
      // Focus a sensible control inside the wizard after the scroll.
      // Slight delay so smooth-scroll animation has started; without
      // the delay the focus jump can fight the scroll and land us
      // back at the top.
      setTimeout(() => {
        const first = document.querySelector('input[name="rw-proto"]:checked')
                   || document.querySelector('input[name="rw-proto"]');
        if (first && typeof first.focus === 'function') first.focus({ preventScroll: true });
      }, 350);
    });
  }

  // Receiver banner's Stop button — same effect as the wizard's
  // Stop Receiver, but reachable without scrolling. Important for
  // users whose wizard is collapsed at the bottom of the page.
  if (els.receiverBannerStop) {
    els.receiverBannerStop.addEventListener('click', async () => {
      els.receiverBannerStop.disabled = true;
      try {
        const r = await fetch('/api/stop', { method: 'POST' });
        const j = await r.json();
        if (!j.error) render(j);
      } finally {
        setTimeout(() => (els.receiverBannerStop.disabled = false), 400);
      }
    });
  }
  els.stopBtn.addEventListener('click', async () => {
    const r = await fetch('/api/stop', { method: 'POST' });
    render(await r.json());
  });

  // Preview / Stop Preview — toggles the server-side pre-stream
  // preview backend. Only NDI sources are supported today; the
  // updatePreviewButton() helper greys it out for other source
  // kinds so a click can't fire there.
  els.previewBtn.addEventListener('click', async () => {
    els.previewBtn.disabled = true;
    const wasActive = lastSnapshot && lastSnapshot.preview && lastSnapshot.preview.active;
    try {
      if (wasActive) {
        await fetch('/api/preview/stop', { method: 'POST' });
        // Tear down the JS poll loop too. The Rust side has already
        // dropped the receiver + cleared latest_jpeg, so the next
        // /api/state will report preview.active=false and
        // updatePreviewButton flips the label back.
        stopPreview();
      } else {
        const r = await fetch('/api/preview/start', { method: 'POST' });
        const j = await r.json();
        if (j.error) {
          els.error.hidden = false;
          els.error.textContent = j.error;
        } else {
          els.error.hidden = true;
          // Kick off the existing JPEG poll loop so frames render
          // as soon as the receiver delivers them.
          const snap = lastSnapshot;
          if (snap && snap.source_id === 'ndi' && snap.ndi_source_name) {
            setPreviewFor({ sourceId: 'ndi', name: snap.ndi_source_name, category: 'ndi' });
          }
        }
      }
      // Refresh state so the button label flips on the same tick.
      const r2 = await fetch('/api/state');
      render(await r2.json());
    } finally {
      setTimeout(() => (els.previewBtn.disabled = false), 400);
    }
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
  const rwStop = $('#rw-stop-receiver');
  const rwStatus = $('#rw-status');
  const rwIpPick = $('#rw-ip-pick');
  const rwIpManual = $('#rw-ip-manual');
  const rwIpStatus = $('#rw-ip-status');
  const rwIpHint = $('#rw-ip-hint');
  // Public-URL helper (Step 2, "Different network" sub-tab).
  const rwWhere = $$('input[name="rw-where"]');
  const rwLanBlock = $('#rw-lan-block');
  const rwPublicBlock = $('#rw-public-block');
  const rwPublicHost = $('#rw-public-host');
  const rwPublicDetect = $('#rw-public-detect');
  const rwPublicStatus = $('#rw-public-status');
  const rwPublicUrl = $('#rw-public-publish-url');
  const rwPublicCopy = $('#rw-public-copy');
  const rwPfProtocol = $('#rw-pf-protocol');
  const rwPfExtPort = $('#rw-pf-ext-port');
  const rwPfIntIp = $('#rw-pf-int-ip');
  const rwPfIntPort = $('#rw-pf-int-port');
  const rwPfCopyTemplate = $('#rw-pf-copy-template');
  const rwPfTemplateStatus = $('#rw-pf-template-status');
  // Advanced settings — ports, RTMP app/key, SRT passphrase.
  const rwAdvanced = $('#rw-advanced');
  const rwAdvancedRows = $$('#rw-advanced .rw-advanced-row');
  const rwSrtPortIn = $('#rw-srt-port');
  const rwSrtPassphraseIn = $('#rw-srt-passphrase');
  const rwRtmpPortIn = $('#rw-rtmp-port');
  const rwRtmpAppIn = $('#rw-rtmp-app');
  const rwRtmpKeyIn = $('#rw-rtmp-key');
  const rwAdvancedApply = $('#rw-advanced-apply');
  const rwAdvancedReset = $('#rw-advanced-reset');
  const rwAdvancedStatus = $('#rw-advanced-status');
  const rwAdvancedRunningHint = $('#rw-advanced-running-hint');

  // The active host for the publish URL. Empty means "we don't know
  // — user must type one in"; UI surfaces a yellow warning instead
  // of substituting a misleading placeholder like 0.0.0.0 or
  // window.location.hostname (which is just 127.0.0.1 inside the
  // webview).
  let rwSelectedIp = '';
  let rwIpMode = 'auto'; // 'auto' (use detected) | 'manual' (user typed)

  function getRwProto() {
    for (const r of rwProto) if (r.checked) return r.value;
    return 'srt_listen';
  }

  function getActiveHost() {
    if (rwIpMode === 'manual') {
      return (rwIpManual?.value || '').trim();
    }
    return rwSelectedIp || '';
  }

  function refreshRwUrl() {
    if (!rwUrl) return;
    const proto = getRwProto();
    const host = getActiveHost();
    const r = lastSnapshot?.relay || {};
    // When we don't have a confirmed LAN IP, show a placeholder that
    // CLEARLY signals "you need to fill this in" rather than a
    // wrong-but-looking-real default. The IP-picker row above the URL
    // surfaces the warning state explicitly.
    const displayHost = host || '<your-lan-ip>';
    if (proto === 'srt_listen') {
      rwUrl.value = `srt://${displayHost}:${r.srt_port ?? 9710}`;
    } else {
      rwUrl.value = `rtmp://${displayHost}:${r.rtmp_port ?? 1935}/${r.rtmp_app || 'live'}/${r.rtmp_key || 'stream'}`;
    }
    // The public-URL block reads the same protocol + port state, so
    // keep it in sync any time the LAN URL refreshes (port changes,
    // protocol toggle, snapshot updates).
    refreshRwPublicUrl();
    refreshPortForwardChecklist();
  }

  function getRwWhere() {
    for (const r of rwWhere) if (r.checked) return r.value;
    return 'lan';
  }

  function getActivePublicHost() {
    return (rwPublicHost?.value || '').trim();
  }

  function refreshRwPublicUrl() {
    if (!rwPublicUrl) return;
    const proto = getRwProto();
    const host = getActivePublicHost();
    const r = lastSnapshot?.relay || {};
    const displayHost = host || '<your-public-ip-or-hostname>';
    if (proto === 'srt_listen') {
      rwPublicUrl.value = `srt://${displayHost}:${r.srt_port ?? 9710}`;
    } else {
      rwPublicUrl.value = `rtmp://${displayHost}:${r.rtmp_port ?? 1935}/${r.rtmp_app || 'live'}/${r.rtmp_key || 'stream'}`;
    }
  }

  function refreshPortForwardChecklist() {
    const proto = getRwProto();
    const r = lastSnapshot?.relay || {};
    const port = proto === 'srt_listen' ? (r.srt_port ?? 9710) : (r.rtmp_port ?? 1935);
    const protoText = proto === 'srt_listen' ? 'UDP (SRT)' : 'TCP (RTMP)';
    if (rwPfProtocol) rwPfProtocol.textContent = protoText;
    if (rwPfExtPort) rwPfExtPort.textContent = String(port);
    if (rwPfIntPort) rwPfIntPort.textContent = String(port);
    if (rwPfIntIp) {
      rwPfIntIp.textContent = rwSelectedIp || '(set your LAN IP in the "On my network" tab)';
    }
  }

  function applyRwWhereState() {
    if (!rwLanBlock || !rwPublicBlock) return;
    const where = getRwWhere();
    rwLanBlock.hidden = where === 'public';
    rwPublicBlock.hidden = where !== 'public';
    if (where === 'public') {
      refreshRwPublicUrl();
      refreshPortForwardChecklist();
    }
  }

  // Show only the rows that apply to the currently-selected protocol.
  // SRT picker → SRT row visible, RTMP row hidden, and vice versa.
  function updateRwAdvancedRowVisibility() {
    const proto = getRwProto();
    rwAdvancedRows.forEach((row) => {
      row.hidden = row.dataset.protocol !== proto;
    });
  }

  // Populate the advanced inputs from the snapshot's relay block.
  // Called on first snapshot and any time the receiver transitions
  // off (so a port we just changed gets reflected back). Skipped if
  // the user is mid-edit (rwAdvancedDirty true) — they own the field
  // values until Apply or Reset.
  let rwAdvancedDirty = false;
  let rwAdvancedHydrated = false;
  function hydrateRwAdvancedFromSnapshot(snap) {
    const r = (snap && snap.relay) || {};
    if (rwAdvancedDirty) return;
    if (rwSrtPortIn) rwSrtPortIn.value = r.srt_port ?? 9710;
    if (rwSrtPassphraseIn) rwSrtPassphraseIn.value = r.srt_passphrase ?? '';
    if (rwRtmpPortIn) rwRtmpPortIn.value = r.rtmp_port ?? 1935;
    if (rwRtmpAppIn) rwRtmpAppIn.value = r.rtmp_app ?? 'live';
    if (rwRtmpKeyIn) rwRtmpKeyIn.value = r.rtmp_key ?? 'stream';
    rwAdvancedHydrated = true;
  }

  function markRwAdvancedDirty() {
    rwAdvancedDirty = true;
    if (rwAdvancedStatus) {
      rwAdvancedStatus.textContent = 'Unsaved changes — click Apply.';
      rwAdvancedStatus.classList.remove('is-warning');
    }
  }

  async function applyRwAdvanced() {
    if (!rwAdvancedApply) return;
    rwAdvancedApply.disabled = true;
    if (rwAdvancedStatus) {
      rwAdvancedStatus.textContent = 'Applying…';
      rwAdvancedStatus.classList.remove('is-warning');
    }
    const srtPort = parseInt(rwSrtPortIn?.value || '0', 10);
    const rtmpPort = parseInt(rwRtmpPortIn?.value || '0', 10);
    // Light client-side validation — let the backend do the real
    // check, but catch obvious nonsense locally so we don't paper
    // over a typo with a misleading 200 OK.
    if (!Number.isFinite(srtPort) || srtPort < 1 || srtPort > 65535) {
      rwAdvancedStatus.textContent = 'SRT port must be 1-65535.';
      rwAdvancedStatus.classList.add('is-warning');
      rwAdvancedApply.disabled = false;
      return;
    }
    if (!Number.isFinite(rtmpPort) || rtmpPort < 1 || rtmpPort > 65535) {
      rwAdvancedStatus.textContent = 'RTMP port must be 1-65535.';
      rwAdvancedStatus.classList.add('is-warning');
      rwAdvancedApply.disabled = false;
      return;
    }
    const payload = {
      relay: {
        srt_port: srtPort,
        srt_passphrase: rwSrtPassphraseIn?.value || '',
        rtmp_port: rtmpPort,
        rtmp_app: rwRtmpAppIn?.value || 'live',
        rtmp_key: rwRtmpKeyIn?.value || 'stream',
      },
    };
    try {
      // applySettings() POSTs the patch and internally calls render()
      // with the response, so lastSnapshot is current by the time it
      // resolves. It also swallows errors silently, so a missing
      // network or 4xx won't throw — we read lastSnapshot.relay back
      // and check whether the values stuck.
      await applySettings(payload);
      const newRelay = (lastSnapshot && lastSnapshot.relay) || {};
      const stuck =
        newRelay.srt_port === srtPort &&
        newRelay.rtmp_port === rtmpPort &&
        newRelay.rtmp_app === (rwRtmpAppIn?.value || 'live') &&
        newRelay.rtmp_key === (rwRtmpKeyIn?.value || 'stream');
      if (stuck) {
        rwAdvancedDirty = false;
        if (rwAdvancedStatus) {
          rwAdvancedStatus.textContent = 'Saved.';
          rwAdvancedStatus.classList.remove('is-warning');
          setTimeout(() => {
            if (rwAdvancedStatus && rwAdvancedStatus.textContent === 'Saved.') {
              rwAdvancedStatus.textContent = '';
            }
          }, 2200);
        }
      } else if (rwAdvancedStatus) {
        rwAdvancedStatus.textContent = 'Backend rejected one or more values — check the log panel.';
        rwAdvancedStatus.classList.add('is-warning');
      }
      refreshRwUrl();
      refreshPortForwardChecklist();
    } catch (err) {
      if (rwAdvancedStatus) {
        rwAdvancedStatus.textContent = `Save failed (${err && err.message || err}).`;
        rwAdvancedStatus.classList.add('is-warning');
      }
    } finally {
      rwAdvancedApply.disabled = false;
    }
  }

  function resetRwAdvancedDefaults() {
    if (rwSrtPortIn) rwSrtPortIn.value = '9710';
    if (rwSrtPassphraseIn) rwSrtPassphraseIn.value = '';
    if (rwRtmpPortIn) rwRtmpPortIn.value = '1935';
    if (rwRtmpAppIn) rwRtmpAppIn.value = 'live';
    if (rwRtmpKeyIn) rwRtmpKeyIn.value = 'stream';
    markRwAdvancedDirty();
  }

  // While the receiver is running, lock the protocol radio AND the
  // advanced inputs so the displayed config can't drift from the
  // running listener. Sync the radio to the snapshot's source_id so
  // the user sees the actual protocol of the running listener — fix
  // for the "I picked SRT but the banner says RTMP" UX bug.
  function syncRwProtocolWithSnapshot(snap) {
    const active = isReceiverActive(snap);
    const running = (snap && snap.source_id) || '';
    rwProto.forEach((r) => {
      if (active) {
        // Force the radio to match what the backend is actually
        // running. Without this, a user who changed the radio after
        // starting the receiver would see a SRT pill highlighted while
        // an RTMP listener is bound. By syncing on every poll, the UI
        // can't disagree with reality.
        r.checked = (r.value === running);
        r.disabled = true;
      } else {
        r.disabled = false;
      }
    });
    const advancedDisabled = active;
    [rwSrtPortIn, rwSrtPassphraseIn, rwRtmpPortIn, rwRtmpAppIn, rwRtmpKeyIn,
     rwAdvancedApply, rwAdvancedReset].forEach((el) => {
      if (el) el.disabled = advancedDisabled;
    });
    if (rwAdvancedRunningHint) rwAdvancedRunningHint.hidden = !active;
    // When the receiver flips from off → on we may have shifted the
    // radio above; reflect that in the URL + the active app's
    // instructions (so the mismatch banner clears or appears as
    // appropriate).
    refreshRwUrl();
    updateRwAdvancedRowVisibility();
    if (rwAppPick && rwAppPick.value) {
      renderRwApp(rwAppPick.value);
    }
  }

  // Hit ipify.org from JS — single-purpose API that returns the
  // requester's public IPv4. CORS-friendly so a webview fetch works.
  // Deferred behind a user click rather than auto-detected on page
  // load: spec'd that way so we never leak a third-party request
  // unless the user explicitly opts into the public-URL flow.
  async function detectPublicIp() {
    if (!rwPublicHost || !rwPublicStatus) return;
    rwPublicStatus.textContent = 'Detecting…';
    rwPublicStatus.classList.remove('is-warning');
    try {
      const r = await fetch('https://api.ipify.org?format=json', { cache: 'no-store' });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const j = await r.json();
      if (!j || !j.ip) throw new Error('no ip in response');
      rwPublicHost.value = j.ip;
      rwPublicStatus.textContent = `Detected: ${j.ip}`;
      refreshRwPublicUrl();
      if (getRwWhere() === 'public') {
        renderRwApp(rwAppPick?.value || '');
      }
    } catch (err) {
      const msg = (err && err.message) ? err.message : String(err);
      rwPublicStatus.textContent = `Detection failed (${msg}). Type your public IP or hostname manually.`;
      rwPublicStatus.classList.add('is-warning');
    }
  }

  function buildPortForwardTemplate() {
    const proto = getRwProto();
    const r = lastSnapshot?.relay || {};
    const port = proto === 'srt_listen' ? (r.srt_port ?? 9710) : (r.rtmp_port ?? 1935);
    const protoText = proto === 'srt_listen' ? 'UDP' : 'TCP';
    const scheme = proto === 'srt_listen' ? 'srt' : 'rtmp';
    const lanIp = rwSelectedIp || "<this-machine's-LAN-IP>";
    const publicHost = getActivePublicHost() || '<your-public-IP-or-hostname>';
    const remotePath = proto === 'rtmp_listen'
      ? `/${r.rtmp_app || 'live'}/${r.rtmp_key || 'stream'}`
      : '';
    return [
      `Hi,`,
      ``,
      `I need a port-forward rule on our router so a remote camera can push a`,
      `${proto === 'srt_listen' ? 'SRT' : 'RTMP'} video stream into a streaming app running on my computer.`,
      ``,
      `  Protocol:       ${protoText}`,
      `  External port:  ${port}`,
      `  Internal IP:    ${lanIp}`,
      `  Internal port:  ${port}`,
      ``,
      `Once it's live, the URL the remote camera/encoder needs is:`,
      `  ${scheme}://${publicHost}:${port}${remotePath}`,
      ``,
      `Thanks!`,
    ].join('\n');
  }

  function applyIpPickerState(interfaces, preferredIp) {
    if (!rwIpPick || !rwIpManual || !rwIpStatus) return;
    const usable = (interfaces || []).filter((iface) => iface.ip);
    if (usable.length === 0) {
      // Detection failed — surface a clear "type your IP" prompt.
      rwIpPick.hidden = true;
      rwIpPick.innerHTML = '';
      rwIpManual.hidden = false;
      rwIpManual.classList.add('needs-input');
      rwIpStatus.textContent = 'Could not auto-detect your LAN IP. Type it in.';
      rwIpStatus.classList.add('is-warning');
      rwIpMode = 'manual';
      rwSelectedIp = '';
      if (rwIpHint) {
        rwIpHint.innerHTML = `Find your IP in <strong>System Settings → Network</strong> (the IPv4 address on the interface that's on the same LAN as your camera). Default port is 9710 for SRT, 1935 for RTMP.`;
      }
      refreshRwUrl();
      return;
    }
    if (usable.length === 1) {
      // One interface — show it as a read-only label, not a dropdown.
      rwIpPick.hidden = true;
      rwIpPick.innerHTML = '';
      rwIpManual.hidden = true;
      rwIpManual.classList.remove('needs-input');
      rwSelectedIp = usable[0].ip;
      rwIpStatus.textContent = `${usable[0].ip} (${usable[0].name || 'auto'})`;
      rwIpStatus.classList.remove('is-warning');
      rwIpMode = 'auto';
      refreshRwUrl();
      return;
    }
    // Multiple interfaces — let the user pick. Default to preferred.
    rwIpPick.innerHTML = '';
    let preferred = preferredIp || usable.find((i) => i.preferred)?.ip || usable[0].ip;
    for (const iface of usable) {
      const opt = document.createElement('option');
      opt.value = iface.ip;
      opt.textContent = `${iface.ip}  (${iface.name || 'auto'})${iface.preferred ? ' — recommended' : ''}`;
      if (iface.ip === preferred) opt.selected = true;
      rwIpPick.appendChild(opt);
    }
    // Sentinel for manual entry, in case none of the auto-detected
    // ones look right (rare, but worth a way out).
    const manualOpt = document.createElement('option');
    manualOpt.value = '__manual__';
    manualOpt.textContent = 'Type a different IP…';
    rwIpPick.appendChild(manualOpt);

    rwIpPick.hidden = false;
    rwIpManual.hidden = true;
    rwIpManual.classList.remove('needs-input');
    rwSelectedIp = preferred;
    rwIpStatus.textContent = `${usable.length} interfaces detected — pick the one on the same network as your encoder.`;
    rwIpStatus.classList.remove('is-warning');
    rwIpMode = 'auto';
    refreshRwUrl();
  }

  function setRwStatus(kind, html) {
    if (!rwStatus) return;
    rwStatus.classList.remove('is-listening', 'is-error', 'is-pending');
    if (kind === 'listening') rwStatus.classList.add('is-listening');
    else if (kind === 'error') rwStatus.classList.add('is-error');
    else if (kind === 'pending') rwStatus.classList.add('is-pending');
    rwStatus.innerHTML = html;
    rwStatus.hidden = false;
  }
  function clearRwStatus() {
    if (!rwStatus) return;
    rwStatus.hidden = true;
    rwStatus.innerHTML = '';
  }

  function getReceiverStatus(snap) {
    // Returns one of: 'idle' | 'listening' | 'receiving' | 'interrupted'
    // - listening = FFmpeg started, no encoder pushing yet (status=Connecting)
    // - receiving = encoder connected and frames flowing to ATEM (status=Streaming)
    // - interrupted = something errored / connection dropped (status=Interrupted)
    if (!snap) return 'idle';
    if (snap.source_id !== 'srt_listen' && snap.source_id !== 'rtmp_listen') return 'idle';
    // Backend's status string lives at snap.stats.status — NOT
    // snap.status or snap.streaming_status (those don't exist). Earlier
    // versions read the wrong field and the wizard's status panel
    // stayed stuck at "Starting receiver…" forever, even after the
    // encoder had connected. Fixed by reading the right field.
    const raw = (snap.stats && snap.stats.status) || '';
    const s = raw.toLowerCase();
    if (s === 'streaming') return 'receiving';
    if (s === 'connecting') return 'listening';
    if (s === 'interrupted') return 'interrupted';
    return 'idle';
  }

  function isReceiverActive(snap) {
    const s = getReceiverStatus(snap);
    return s === 'listening' || s === 'receiving';
  }

  function updateReceiverButtons() {
    if (!rwStart) return;
    const status = getReceiverStatus(lastSnapshot);
    const proto = (lastSnapshot && lastSnapshot.source_id === 'srt_listen') ? 'SRT' : 'RTMP';

    if (status === 'listening' || status === 'receiving') {
      rwStart.hidden = true;
      if (rwStop) rwStop.hidden = false;
      const url = rwUrl?.value || '';
      const host = getActiveHost();

      if (status === 'listening') {
        // Listener bound, waiting for encoder. Animated dot pulses
        // yellow to signal "ready and waiting" — distinct from the
        // solid green "actively receiving" state.
        const urlLine = host
          ? `Paste this into your encoder: <code>${escapeHtml(url)}</code>`
          : `Type your LAN IP above so we can show you the URL to paste into your encoder.`;
        setRwStatus(
          'pending',
          `<span class="rw-status-title">` +
          `<span class="rw-pulse rw-pulse-waiting" aria-hidden="true"></span>` +
          `${proto} listener bound — waiting for encoder` +
          `</span>` +
          urlLine
        );
      } else {
        // status === 'receiving' — frames flowing through to ATEM.
        // Solid green dot, bitrate readout, clear "you're done" copy.
        const stats = (lastSnapshot && lastSnapshot.stats) || {};
        const kbps = Math.round((stats.bitrate || 0) / 1000);
        const fps  = stats.fps ? `${Math.round(stats.fps)} fps` : '';
        const live = kbps > 0
          ? `${kbps} kbps${fps ? ' · ' + fps : ''}`
          : 'connection up';
        setRwStatus(
          'listening',
          `<span class="rw-status-title">` +
          `<span class="rw-pulse rw-pulse-live" aria-hidden="true"></span>` +
          `Encoder connected — forwarding to ATEM` +
          `</span>` +
          `Live: ${escapeHtml(live)}<br>` +
          `Stop here when you're done — do NOT click <em>Start Stream</em> at the top of the page (it'll just complain "stream already running").`
        );
      }
    } else if (status === 'interrupted') {
      rwStart.hidden = false;
      if (rwStop) rwStop.hidden = true;
      const errText = (lastSnapshot && lastSnapshot.stats && lastSnapshot.stats.error) || '';
      setRwStatus(
        'error',
        `<span class="rw-status-title">Receiver interrupted</span>` +
        (errText ? `${escapeHtml(errText)}<br>` : '') +
        `Click <strong>Start Receiver</strong> again to re-bind the listener. ` +
        `Tip: most cameras (DJI Osmo Pocket, Larix, Blackmagic Camera) stop pushing as soon as the connection drops, so you'll need to re-start their stream too.`
      );
    } else {
      // idle
      rwStart.hidden = false;
      if (rwStop) rwStop.hidden = true;
      // Don't clear status here — preserve last error for the user
      // to see. clearRwStatus() runs explicitly when they retry.
    }
  }

  // Map: which protocol each app in the dropdown supports. Used to
  // decide when to surface a mismatch warning + one-click switch.
  // Apps that support both ('srt_listen','rtmp_listen') just render
  // for whatever the user picked in step 1.
  const RW_APP_PROTOCOLS = {
    'obs': ['srt_listen', 'rtmp_listen'],
    'larix': ['srt_listen', 'rtmp_listen'],
    'ffmpeg': ['srt_listen', 'rtmp_listen'],
    'dji-osmo-pocket3': ['rtmp_listen'],
    'dji': ['rtmp_listen'],
    'iphone-bm': ['rtmp_listen'],
  };

  function renderRwApp(app) {
    if (!rwAppBody) return;
    if (!app) { rwAppBody.hidden = true; rwAppBody.innerHTML = ''; return; }
    const proto = getRwProto();
    // Protocol mismatch check: if the app only supports one protocol
    // and the wizard's current selection is a different one, render a
    // prominent warning + "Switch to <X>" button instead of letting
    // the per-app URL silently disagree with the step 2 URL. The
    // button is disabled when the receiver is running (matching the
    // locked-radio policy in syncRwProtocolWithSnapshot).
    const supported = RW_APP_PROTOCOLS[app] || ['srt_listen', 'rtmp_listen'];
    const mismatched = !supported.includes(proto);
    const targetProto = supported[0];
    const targetLabel = targetProto === 'srt_listen' ? 'SRT' : 'RTMP';
    const receiverRunning = isReceiverActive(lastSnapshot);
    const switchDisabled = receiverRunning ? ' disabled' : '';
    const switchHint = receiverRunning
      ? `<span class="rw-app-mismatch-hint">Stop the receiver below first.</span>`
      : '';
    const mismatchBanner = mismatched
      ? `<div class="rw-app-mismatch">
           <span class="rw-app-mismatch-msg">⚠ This app only supports <strong>${targetLabel}</strong>.
           The URL in step 2 above is the wrong protocol — switch to ${targetLabel}
           so step 2 matches the URL shown here.</span>
           <button type="button" class="rw-app-mismatch-btn" id="rw-app-mismatch-btn"${switchDisabled}>Switch to ${targetLabel}</button>
           ${switchHint}
         </div>`
      : '';
    // Effective protocol for rendering this app's instructions —
    // ALWAYS use the app's required protocol when it's single-
    // protocol, so the URLs in step 3 are correct regardless of
    // what the user picked in step 1. The mismatch banner above
    // explains the situation and offers the one-click fix.
    const effectiveProto = mismatched ? targetProto : proto;
    // Host + URL the user should paste into their encoder app. When the
    // user is in "Different network" mode, switch to the public address
    // so the per-app instructions are immediately correct without them
    // having to mentally substitute the LAN IP for their public IP.
    const where = getRwWhere();
    const publicMode = where === 'public';
    const host = publicMode
      ? (getActivePublicHost() || '<your-public-ip>')
      : (lanIp || '<your-lan-ip>');
    // Build a URL string matching the EFFECTIVE protocol, not the
    // step 2 URL field (which reflects the radio). This is the URL
    // for the app's required protocol; the mismatch banner is what
    // alerts the user that the step 2 field is the wrong protocol
    // until they hit the Switch button.
    const r = lastSnapshot?.relay || {};
    const url = effectiveProto === 'srt_listen'
      ? `srt://${host}:${r.srt_port ?? 9710}`
      : `rtmp://${host}:${r.rtmp_port ?? 1935}/${r.rtmp_app || 'live'}/${r.rtmp_key || 'stream'}`;
    const networkBadge = publicMode
      ? `<p class="rw-app-network-badge">Different network — URL uses your public address. Make sure the port-forward checklist above is set up first.</p>`
      : '';
    let html = '';
    if (app === 'obs' && effectiveProto === 'srt_listen') {
      html = `<strong>OBS → Settings → Stream</strong>
        <ol>
          <li>Service: <code>Custom...</code></li>
          <li>Server: <code>${escapeHtml(url)}?streamid=publish</code></li>
          <li>Stream Key: leave blank</li>
          <li>Output → Encoder: x264 or HEVC, Keyframe Interval 2s, Bitrate to match what your network can carry</li>
        </ol>`;
    } else if (app === 'obs' && effectiveProto === 'rtmp_listen') {
      html = `<strong>OBS → Settings → Stream</strong>
        <ol>
          <li>Service: <code>Custom...</code></li>
          <li>Server: <code>rtmp://${escapeHtml(host)}:${r.rtmp_port ?? 1935}/${escapeHtml(r.rtmp_app || 'live')}</code></li>
          <li>Stream Key: <code>${escapeHtml(r.rtmp_key || 'stream')}</code></li>
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
    } else if (app === 'dji-osmo-pocket3') {
      const rtmpServer = `rtmp://${escapeHtml(host)}:${r.rtmp_port ?? 1935}/${escapeHtml(r.rtmp_app || 'live')}`;
      const rtmpKey = escapeHtml(r.rtmp_key || 'stream');
      html = `<strong>DJI Osmo Pocket 3 (via Mimo app on phone)</strong>
        <ol>
          <li>Pair the Pocket 3 to the <em>DJI Mimo</em> app on your phone.</li>
          <li>In Mimo, open the camera view → tap the icon for <em>Live Streaming</em> (the bottom toolbar; it looks like a broadcast tower).</li>
          <li>Pick <em>RTMP</em> as the platform.</li>
          <li>Server URL: <code>${rtmpServer}</code></li>
          <li>Stream Key: <code>${rtmpKey}</code></li>
          <li>Tap <em>Start Live</em> in Mimo. ${publicMode ? 'The Pocket pushes via the phone\'s data connection — works from anywhere with cellular signal.' : 'The Pocket sends video over the phone\'s connection — make sure the phone is on the same Wi-Fi as this computer.'}</li>
        </ol>`;
    } else if (app === 'dji') {
      html = `<strong>DJI drone (RC Plus / Mini 4 Pro / Mavic 3)</strong>
        <ol>
          <li>In the Fly app: <em>Camera View → Transmission → Live Streaming Platform → RTMP Custom</em></li>
          <li>RTMP URL: <code>rtmp://${escapeHtml(host)}:${r.rtmp_port ?? 1935}/${escapeHtml(r.rtmp_app || 'live')}/${escapeHtml(r.rtmp_key || 'stream')}</code></li>
        </ol>`;
    } else if (app === 'ffmpeg') {
      const cmd = effectiveProto === 'srt_listen'
        ? `ffmpeg -re -i input.mp4 -c:v libx264 -preset veryfast -tune zerolatency -c:a aac -f mpegts '${url}'`
        : `ffmpeg -re -i input.mp4 -c:v libx264 -preset veryfast -tune zerolatency -c:a aac -f flv '${url}'`;
      html = `<strong>FFmpeg from a file or device</strong>
        <pre style="white-space:pre-wrap;font-size:11px;color:#b6e1c1;background:rgba(0,0,0,0.25);padding:8px 10px;border-radius:5px;">${escapeHtml(cmd)}</pre>`;
    } else if (app === 'iphone-bm') {
      html = `<strong>Blackmagic Camera app (iPhone)</strong>
        <ol>
          <li>Tap the gear icon → <em>Stream</em></li>
          <li>Service: <code>Custom RTMP</code> (the BMD app speaks RTMP only)</li>
          <li>Server: <code>rtmp://${escapeHtml(host)}:${r.rtmp_port ?? 1935}/${escapeHtml(r.rtmp_app || 'live')}</code></li>
          <li>Key: <code>${escapeHtml(r.rtmp_key || 'stream')}</code></li>
        </ol>`;
    } else {
      html = `<em>Pick your encoder app above for tailored instructions.</em>`;
    }
    // Order: mismatch banner first (most important — warns about the
    // step 2 URL being the wrong protocol), then network-mode badge
    // (public-IP reminder), then the app-specific instructions.
    rwAppBody.innerHTML = mismatchBanner + networkBadge + html;
    rwAppBody.hidden = false;
    // The mismatch banner contains a button — wire its click handler
    // after the innerHTML assignment since the button is freshly
    // created on every render.
    const mismatchBtn = document.getElementById('rw-app-mismatch-btn');
    if (mismatchBtn && !receiverRunning) {
      mismatchBtn.addEventListener('click', () => {
        // Find the radio for the target protocol and check it.
        for (const r of rwProto) {
          r.checked = (r.value === targetProto);
        }
        // Mirror the same refresh chain the radio's change handler
        // does — but the radios were updated programmatically here,
        // which doesn't fire 'change' events, so call directly.
        refreshRwUrl();
        updateRwAdvancedRowVisibility();
        renderRwApp(app);
      });
    }
  }

  rwProto.forEach((r) => r.addEventListener('change', () => {
    refreshRwUrl();
    updateRwAdvancedRowVisibility();
    renderRwApp(rwAppPick?.value || '');
  }));
  if (rwAppPick) rwAppPick.addEventListener('change', () => renderRwApp(rwAppPick.value));
  if (rwCopy) rwCopy.addEventListener('click', () => copyToClipboard(rwUrl.value, rwCopy));

  // Advanced settings wiring — mark dirty on edit, Apply POSTs to
  // /api/settings, Reset wipes to spec defaults.
  [rwSrtPortIn, rwSrtPassphraseIn, rwRtmpPortIn, rwRtmpAppIn, rwRtmpKeyIn]
    .forEach((el) => {
      if (el) el.addEventListener('input', markRwAdvancedDirty);
    });
  if (rwAdvancedApply) rwAdvancedApply.addEventListener('click', applyRwAdvanced);
  if (rwAdvancedReset) rwAdvancedReset.addEventListener('click', resetRwAdvancedDefaults);

  // Public-URL helper wiring.
  rwWhere.forEach((r) => r.addEventListener('change', () => {
    applyRwWhereState();
    // Re-render per-app instructions with the new host (LAN ↔ public).
    renderRwApp(rwAppPick?.value || '');
  }));
  if (rwPublicHost) {
    rwPublicHost.addEventListener('input', () => {
      refreshRwPublicUrl();
      // Per-app instructions show the public URL inline when in
      // "different network" mode — re-render as the host changes.
      if (getRwWhere() === 'public') {
        renderRwApp(rwAppPick?.value || '');
      }
    });
  }
  if (rwPublicDetect) {
    rwPublicDetect.addEventListener('click', () => {
      rwPublicDetect.disabled = true;
      detectPublicIp().finally(() => {
        rwPublicDetect.disabled = false;
      });
    });
  }
  if (rwPublicCopy) {
    rwPublicCopy.addEventListener('click', () => copyToClipboard(rwPublicUrl.value, rwPublicCopy));
  }
  if (rwPfCopyTemplate) {
    rwPfCopyTemplate.addEventListener('click', () => {
      copyToClipboard(buildPortForwardTemplate(), rwPfCopyTemplate);
    });
  }
  // Initial paint so the public-URL field is populated even before
  // the user toggles into the public sub-tab. Also primes the
  // port-forward checklist's "internal IP" with whatever LAN IP
  // detection lands on first poll.
  applyRwWhereState();
  refreshRwPublicUrl();
  refreshPortForwardChecklist();

  if (rwIpPick) rwIpPick.addEventListener('change', () => {
    if (rwIpPick.value === '__manual__') {
      rwIpMode = 'manual';
      rwSelectedIp = '';
      rwIpManual.hidden = false;
      rwIpManual.classList.add('needs-input');
      rwIpManual.focus();
    } else {
      rwIpMode = 'auto';
      rwSelectedIp = rwIpPick.value;
      rwIpManual.hidden = true;
      rwIpManual.classList.remove('needs-input');
    }
    refreshRwUrl();
    updateReceiverButtons();
  });
  if (rwIpManual) rwIpManual.addEventListener('input', () => {
    if (rwIpMode === 'manual') {
      // Strip whitespace silently; let the user type freely otherwise.
      const v = rwIpManual.value.trim();
      // Drop the warning highlight as soon as something is typed.
      if (v) rwIpManual.classList.remove('needs-input');
      else rwIpManual.classList.add('needs-input');
    }
    refreshRwUrl();
    updateReceiverButtons();
  });

  if (rwStart) rwStart.addEventListener('click', async () => {
    rwStart.disabled = true;
    clearRwStatus();
    setRwStatus('pending', `<span class="rw-status-title">Starting receiver…</span>Spinning up the ${getRwProto() === 'srt_listen' ? 'SRT' : 'RTMP'} listener.`);
    try {
      // Switch the source to the chosen relay listener, then start the
      // stream. The relay panel below the source tiles becomes visible
      // automatically once source_id flips.
      await applySettings({ source_id: getRwProto() });
      const r = await fetch('/api/start', { method: 'POST' });
      const j = await r.json();
      if (j.error) {
        // Also bubble to the global error banner — but the wizard's
        // own status pane is what users were looking at when they
        // clicked Start, so it's the primary surface.
        els.error.hidden = false;
        els.error.textContent = j.error;
        setRwStatus(
          'error',
          `<span class="rw-status-title">Could not start receiver</span>` +
          `${escapeHtml(j.error)}<br>` +
          `<span style="opacity:0.85;">Tip: make sure no other process is bound to the listener port (default 9710 SRT / 1935 RTMP). Check the log panel above for FFmpeg's stderr.</span>`
        );
      } else {
        // The receiver kicks off but won't have flow yet — the status
        // panel updates to "listening" via the next poll cycle through
        // updateReceiverButtons().
        render(j);
        updateReceiverButtons();
      }
    } catch (err) {
      setRwStatus(
        'error',
        `<span class="rw-status-title">Could not reach the backend</span>` +
        `${escapeHtml(err && err.message || String(err))}`
      );
    } finally {
      setTimeout(() => (rwStart.disabled = false), 600);
    }
  });

  if (rwStop) rwStop.addEventListener('click', async () => {
    rwStop.disabled = true;
    setRwStatus('pending', `<span class="rw-status-title">Stopping receiver…</span>`);
    try {
      const r = await fetch('/api/stop', { method: 'POST' });
      const j = await r.json();
      if (j.error) {
        setRwStatus(
          'error',
          `<span class="rw-status-title">Could not stop receiver</span>${escapeHtml(j.error)}`
        );
      } else {
        clearRwStatus();
        render(j);
        updateReceiverButtons();
      }
    } catch (err) {
      setRwStatus(
        'error',
        `<span class="rw-status-title">Could not reach the backend</span>${escapeHtml(err && err.message || String(err))}`
      );
    } finally {
      setTimeout(() => (rwStop.disabled = false), 600);
    }
  });

  // Refresh URL whenever the snapshot changes (port/app may move).
  // The poll loop already calls render(snap) on every tick; we just
  // need to re-render the URL when the wizard is open.
  const urlRefreshTimer = setInterval(() => {
    // Hydrate advanced fields on first valid snapshot. Skipped on
    // subsequent ticks unless the user hits Reset (which marks dirty
    // → Apply clears dirty → next snapshot re-hydration is safe but
    // gated by !dirty).
    if (lastSnapshot && !rwAdvancedHydrated) {
      hydrateRwAdvancedFromSnapshot(lastSnapshot);
    }
    syncRwProtocolWithSnapshot(lastSnapshot);
    refreshRwUrl();
    updateReceiverButtons();
  }, 1000);
  // No teardown needed — runs for the lifetime of the page.
  void urlRefreshTimer;

  // Expose the IP-picker initializer so the boot fetch (which runs
  // after bind()) can populate the picker once /api/lan-ips returns.
  // The handler closes over the wizard's local DOM refs, so it has to
  // live inside bind() — exposing on window is the cheapest bridge.
  window.applyIpPickerState = applyIpPickerState;
  window.updateReceiverButtons = updateReceiverButtons;
  // Expose the wizard sync helpers too so render() can call them
  // synchronously after applying a snapshot (avoids waiting up to a
  // second for the next interval tick to redraw the locked-radio
  // state when the receiver flips active).
  window.syncRwProtocolWithSnapshot = syncRwProtocolWithSnapshot;
  window.hydrateRwAdvancedFromSnapshot = (snap) => {
    if (!rwAdvancedHydrated) hydrateRwAdvancedFromSnapshot(snap);
  };
}

bind();
ensureDevicesLoaded();
ensureNdiLoaded();
ensureOmtLoaded();
// Session 12 — populate the DeckLink device dropdown on boot so the
// picker is ready when the operator switches to DeckLink mode. The
// FFmpeg probe is cheap (one process invocation) and runs once per
// process; subsequent calls hit the 60s cache.
fetchDecklinkDevices(false);
// Probe every IPv4 interface so the wizard can show a picker when
// the host has more than one (Wi-Fi + Ethernet, VPN, Apple Internet
// Sharing, etc.). The legacy /api/lan-ip single-result endpoint is
// kept for back-compat but the multi-IP endpoint is what the wizard
// actually consumes. If detection fails entirely, applyIpPickerState
// surfaces a "type your IP in" prompt instead of substituting a
// wrong-but-real-looking default.
fetchJSON('/api/lan-ips').then((j) => {
  lanIp = j.preferred_ip || '';
  if (typeof window.applyIpPickerState === 'function') {
    window.applyIpPickerState(j.interfaces || [], j.preferred_ip || '');
  }
}).catch((err) => {
  console.warn('LAN IP detection failed:', err);
  if (typeof window.applyIpPickerState === 'function') {
    window.applyIpPickerState([], '');
  }
});
poll();
setInterval(poll, 1000);
// Refresh NDI senders every 30s — they come and go.
setInterval(() => ensureNdiLoaded(true), 30000);
// Same for OMT senders. Cheap when the omt feature is off (server
// returns empty immediately); polls real OmtDiscovery::addresses
// otherwise.
setInterval(() => ensureOmtLoaded(true), 30000);

// alpha.31: audio level meters. Independent 4Hz polling so the meter
// responsiveness doesn't hinge on the main /api/state 1Hz cadence.
// Self-gates: when meters_enabled is false the loop just hides the
// meters and skips the fetch.
const audioMeterState = {
  // Peak-hold state per channel. Each entry is { value, holdUntilMs }.
  // value decays back to the live peak after holdUntilMs elapses.
  peakHold: { l: { value: -120, holdUntil: 0 }, r: { value: -120, holdUntil: 0 } },
  // Most recent live values (used when the server reports stale data
  // — we fade meters to silence rather than freezing the bar).
  lastFresh: { rms_l: -120, rms_r: -120, peak_l: -120, peak_r: -120, age: 0 },
};
const PEAK_HOLD_MS = 1500;
const METER_DB_FLOOR = -60;
const METER_DB_CEIL = 0;

function dbToFrac(db) {
  // Clamp + linear-map [-60, 0] dB → [0, 1].
  if (db <= METER_DB_FLOOR) return 0;
  if (db >= METER_DB_CEIL) return 1;
  return (db - METER_DB_FLOOR) / (METER_DB_CEIL - METER_DB_FLOOR);
}

function renderMeter(canvas, rmsDb, peakDb, peakHoldDb) {
  const ctx = canvas.getContext('2d');
  const w = canvas.width;
  const h = canvas.height;
  ctx.clearRect(0, 0, w, h);

  // Background bar (always visible, very dim).
  ctx.fillStyle = 'rgba(255, 255, 255, 0.04)';
  ctx.fillRect(0, 0, w, h);

  // RMS bar — green (<-18), yellow (-18 to -6), red (-6 to 0).
  const rmsFrac = dbToFrac(rmsDb);
  const rmsW = Math.round(rmsFrac * w);
  if (rmsW > 0) {
    // Gradient stops match the dB zones.
    const grad = ctx.createLinearGradient(0, 0, w, 0);
    grad.addColorStop(0, '#1f8a3f'); // green
    grad.addColorStop(dbToFrac(-18), '#1f8a3f');
    grad.addColorStop(dbToFrac(-18) + 0.001, '#d9b32a'); // yellow
    grad.addColorStop(dbToFrac(-6), '#d9b32a');
    grad.addColorStop(dbToFrac(-6) + 0.001, '#c64545'); // red
    grad.addColorStop(1, '#c64545');
    ctx.fillStyle = grad;
    ctx.fillRect(0, 0, rmsW, h);
  }

  // Peak-hold tick: small vertical line at the held peak position.
  const peakFrac = dbToFrac(peakHoldDb);
  const peakX = Math.round(peakFrac * w);
  if (peakX > 0 && peakX < w) {
    ctx.fillStyle = peakHoldDb >= -6 ? '#ff6b6b' : peakHoldDb >= -18 ? '#ffdf6b' : '#74d791';
    ctx.fillRect(peakX - 1, 0, 2, h);
  }
}

function updatePeakHold(channelState, livePeakDb, nowMs) {
  if (livePeakDb > channelState.value || nowMs >= channelState.holdUntil) {
    channelState.value = livePeakDb;
    channelState.holdUntil = nowMs + PEAK_HOLD_MS;
  }
}

async function tickAudioMeters() {
  const wrap = document.getElementById('audio-meters');
  if (!wrap) return;
  try {
    const resp = await fetch('/api/audio-levels');
    if (!resp.ok) return;
    const data = await resp.json();

    // Toggle visibility based on backend's meters_enabled flag.
    wrap.hidden = !data.meters_enabled;
    if (!data.meters_enabled) return;

    // Stale-data detection. If the server hasn't received any astats
    // updates in >0.7s, the stream is paused or the encoder isn't
    // running — fade meters to silence rather than freezing at the
    // last live value.
    const idleSecs = Math.max(0, data.server_time - (data.updated_at || 0));
    const fresh = idleSecs < 0.7 && data.updated_at > 0;

    const rmsL = fresh ? data.rms_l : -120;
    const rmsR = fresh ? data.rms_r : -120;
    const peakL = fresh ? data.peak_l : -120;
    const peakR = fresh ? data.peak_r : -120;

    const nowMs = performance.now();
    updatePeakHold(audioMeterState.peakHold.l, peakL, nowMs);
    updatePeakHold(audioMeterState.peakHold.r, peakR, nowMs);

    const canL = document.getElementById('audio-meter-l');
    const canR = document.getElementById('audio-meter-r');
    if (canL) renderMeter(canL, rmsL, peakL, audioMeterState.peakHold.l.value);
    if (canR) renderMeter(canR, rmsR, peakR, audioMeterState.peakHold.r.value);

    // alpha.48: also render to the preview-overlay meters that sit on
    // top of the Monitor card's preview frame. Operators want levels
    // right next to the picture, not just buried in the Audio Mixer
    // card. Same data, same peak-hold state, additional render
    // targets — no extra poll cost.
    const pCanL = document.getElementById('preview-meter-l');
    const pCanR = document.getElementById('preview-meter-r');
    if (pCanL) renderMeter(pCanL, rmsL, peakL, audioMeterState.peakHold.l.value);
    if (pCanR) renderMeter(pCanR, rmsR, peakR, audioMeterState.peakHold.r.value);
    const previewReadout = document.getElementById('preview-meter-readout');
    if (previewReadout) {
      previewReadout.textContent = fresh
        ? `${Math.max(rmsL, rmsR).toFixed(0)} dB`
        : '—';
    }

    const dbLEl = document.getElementById('audio-meter-l-db');
    const dbREl = document.getElementById('audio-meter-r-db');
    if (dbLEl) dbLEl.textContent = fresh ? `${rmsL.toFixed(1)} dB` : '—';
    if (dbREl) dbREl.textContent = fresh ? `${rmsR.toFixed(1)} dB` : '—';
  } catch (_e) {
    // Silently skip — meters are a non-critical visualization, no
    // need to surface a network error on every tick.
  }
}

setInterval(tickAudioMeters, 250);
tickAudioMeters();

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

// Visible Refresh button — wipes the SW cache (none today, here as
// future-proofing) then force-reloads bypassing HTTP cache. Primary
// way to bust a stale-JS state when keystrokes don't help.
if (els.refreshApp) {
  els.refreshApp.addEventListener('click', async () => {
    if ('caches' in window) {
      try {
        const keys = await caches.keys();
        await Promise.all(keys.map((k) => caches.delete(k)));
      } catch (_e) { /* ignore */ }
    }
    // location.reload() in modern browsers always revalidates anyway,
    // but spelling it out makes the intent obvious.
    location.reload();
  });
}
