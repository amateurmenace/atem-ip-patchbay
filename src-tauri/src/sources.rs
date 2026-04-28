use crate::device_scanner::{
    list_capture_devices, pick_best_avf_mode, probe_avf_modes, AvfMode, Device,
};
use crate::state::EncoderState;

#[derive(Debug, Clone)]
pub struct Source {
    pub id: String,
    pub label: String,
    pub description: String,
    /// CLI tokens inserted before output options. Must produce one
    /// video stream and one audio stream that the encoder maps as
    /// `0:v:0` + `1:a:0` (or `0:v:0` + `0:a:0` when `combined_av`).
    pub ffmpeg_input_args: Vec<String>,
    pub available: bool,
    pub notes: String,
    /// True when video and audio share input #0.
    pub combined_av: bool,
}

impl Default for Source {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            description: String::new(),
            ffmpeg_input_args: Vec::new(),
            available: true,
            notes: String::new(),
            combined_av: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Source factories
// ---------------------------------------------------------------------------

pub fn test_pattern(width: u32, height: u32, fps: u32) -> Source {
    let size = format!("{width}x{height}");
    Source {
        id: "test_pattern".into(),
        label: "Test Pattern".into(),
        description: format!("SMPTE bars {size} @ {fps}fps + 1 kHz tone"),
        ffmpeg_input_args: [
            "-re",
            "-f", "lavfi",
            "-i",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain(std::iter::once(format!(
            "testsrc2=size={size}:rate={fps},format=yuv420p"
        )))
        .chain(["-f", "lavfi", "-i"].iter().map(|s| s.to_string()))
        .chain(std::iter::once(
            "sine=frequency=1000:sample_rate=48000".to_string(),
        ))
        .collect(),
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)]
pub fn avfoundation(
    width: u32,
    height: u32,
    fps: u32,
    video_index: i32,
    audio_index: i32,
    video_name: &str,
    audio_name: &str,
    label: &str,
    description: &str,
) -> Source {
    let probe_index = resolve_avf_index_for_probe(video_name, video_index);
    let modes = probe_avf_modes(probe_index);
    let (actual_w, actual_h, actual_fps) =
        pick_best_avf_mode(&modes, width, height, fps as f64);
    let actual_size = format!("{actual_w}x{actual_h}");
    let fps_str = format_fps(actual_fps);

    let note = if !modes.is_empty() && (actual_w != width || actual_h != height) {
        format!(
            "Capturing at {actual_size}@{fps_str}fps (device's native mode); \
             encoder will scale to {width}x{height}@{fps}fps for the destination."
        )
    } else if !modes.is_empty() && (actual_fps - fps as f64).abs() > 0.001 {
        format!(
            "Capturing at {fps_str}fps (device-supported); \
             encoder will retime to {fps}fps for the destination."
        )
    } else {
        String::new()
    };

    let token = if !video_name.is_empty() {
        if !audio_name.is_empty() {
            format!("{video_name}:{audio_name}")
        } else {
            video_name.to_string()
        }
    } else {
        format!("{video_index}:{audio_index}")
    };

    Source {
        id: "avfoundation".into(),
        label: if label.is_empty() {
            "AVFoundation".into()
        } else {
            label.to_string()
        },
        description: if description.is_empty() {
            format!("AVFoundation {actual_size}@{fps_str}fps -> {width}x{height}@{fps}fps")
        } else {
            description.to_string()
        },
        ffmpeg_input_args: {
            let mut args = vec![
                "-f".into(),
                "avfoundation".into(),
                "-framerate".into(),
                fps_str.clone(),
                "-video_size".into(),
                actual_size,
                "-capture_cursor".into(),
                "1".into(),
                "-i".into(),
                token,
            ];
            // Video-only AVF inputs (screen capture, plain camera
            // when no audio device is paired in the token, virtual
            // cameras without mic emulation) have no audio stream,
            // so the streamer's `-map 0:a:0` would fail with
            // "Stream map '' matches no streams" the moment the
            // pipeline starts. Append a silent lavfi input as
            // track 1 and downstream combined_av=false flips the
            // mapping to `1:a:0`. Cameras WITH a paired audio
            // device in the token get the combined-AV path as
            // before — combined_av stays true.
            let has_paired_audio = !audio_name.is_empty();
            if !has_paired_audio {
                args.extend([
                    "-f".into(),
                    "lavfi".into(),
                    "-i".into(),
                    "anullsrc=channel_layout=stereo:sample_rate=48000".into(),
                ]);
            }
            args
        },
        combined_av: !audio_name.is_empty(),
        notes: note,
        ..Default::default()
    }
}

fn format_fps(fps: f64) -> String {
    if (fps - fps.round()).abs() < 0.001 {
        format!("{}", fps as i64)
    } else {
        format!("{fps:.3}")
    }
}

fn resolve_avf_index_for_probe(name: &str, fallback_index: i32) -> i32 {
    if name.is_empty() {
        return fallback_index;
    }
    let devs = list_capture_devices(true);
    devs.video
        .iter()
        .find(|d| d.name == name)
        .map(|d| d.index)
        .unwrap_or(fallback_index)
}

pub fn dshow_capture(
    width: u32,
    height: u32,
    fps: u32,
    video_name: &str,
    audio_name: &str,
    label: &str,
    description: &str,
) -> Source {
    if !audio_name.is_empty() {
        Source {
            id: "avfoundation".into(),
            label: if label.is_empty() {
                "DirectShow".into()
            } else {
                label.to_string()
            },
            description: if description.is_empty() {
                format!("DirectShow capture (native rate) -> {width}x{height} @ {fps}fps")
            } else {
                description.to_string()
            },
            ffmpeg_input_args: vec![
                "-f".into(),
                "dshow".into(),
                "-rtbufsize".into(),
                "256M".into(),
                "-i".into(),
                format!("video={video_name}:audio={audio_name}"),
            ],
            combined_av: true,
            ..Default::default()
        }
    } else {
        Source {
            id: "avfoundation".into(),
            label: if label.is_empty() {
                "DirectShow".into()
            } else {
                label.to_string()
            },
            description: if description.is_empty() {
                format!("DirectShow video (native rate, silent) -> {width}x{height} @ {fps}fps")
            } else {
                description.to_string()
            },
            ffmpeg_input_args: vec![
                "-f".into(),
                "dshow".into(),
                "-rtbufsize".into(),
                "256M".into(),
                "-i".into(),
                format!("video={video_name}"),
                "-f".into(),
                "lavfi".into(),
                "-i".into(),
                "anullsrc=channel_layout=stereo:sample_rate=48000".into(),
            ],
            combined_av: false,
            ..Default::default()
        }
    }
}

pub fn gdigrab_desktop(
    width: u32,
    height: u32,
    fps: u32,
    audio_name: &str,
    label: &str,
    description: &str,
) -> Source {
    let size = format!("{width}x{height}");
    let mut cmd = vec![
        "-f".into(),
        "gdigrab".into(),
        "-framerate".into(),
        fps.to_string(),
        "-video_size".into(),
        size.clone(),
        "-i".into(),
        "desktop".into(),
    ];
    if !audio_name.is_empty() {
        cmd.extend(
            [
                "-f", "dshow", "-rtbufsize", "256M", "-i",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        cmd.push(format!("audio={audio_name}"));
    } else {
        cmd.extend(
            [
                "-f",
                "lavfi",
                "-i",
                "anullsrc=channel_layout=stereo:sample_rate=48000",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
    }
    Source {
        id: "avfoundation".into(),
        label: if label.is_empty() {
            "Screen capture".into()
        } else {
            label.to_string()
        },
        description: if description.is_empty() {
            format!("Desktop capture {size} @ {fps}fps")
        } else {
            description.to_string()
        },
        ffmpeg_input_args: cmd,
        combined_av: false,
        ..Default::default()
    }
}

pub fn pipe(path: &str) -> Source {
    Source {
        id: "pipe".into(),
        label: "Pipe / URL".into(),
        description: format!(
            "Read from {}",
            if path.is_empty() { "<unset>" } else { path }
        ),
        ffmpeg_input_args: if path.is_empty() {
            Vec::new()
        } else {
            vec!["-re".into(), "-i".into(), path.to_string()]
        },
        available: !path.is_empty(),
        notes: "Set the pipe path in the Source panel. Works with FIFOs, files, or any URL FFmpeg supports (rtsp://, http://, etc.).".into(),
        combined_av: true,
    }
}

pub fn srt_listen(
    bind_host: &str,
    bind_port: u16,
    latency_us: u32,
    passphrase: &str,
) -> Source {
    let mut url = format!("srt://{bind_host}:{bind_port}?mode=listener&latency={latency_us}");
    if !passphrase.is_empty() {
        url.push_str(&format!("&passphrase={passphrase}"));
    }
    Source {
        id: "srt_listen".into(),
        label: format!("SRT in :{bind_port}"),
        description: format!("SRT listener on {bind_host}:{bind_port}"),
        ffmpeg_input_args: vec!["-f".into(), "mpegts".into(), "-i".into(), url],
        combined_av: true,
        notes: format!(
            "Point your encoder at srt://<this-machine-ip>:{bind_port} in caller mode. \
             MPEG-TS is auto-detected; H.264 / H.265 / AAC payloads all work — they get \
             re-encoded into BMD's preferred profile on the way out."
        ),
        ..Default::default()
    }
}

/// NDI direct ingest. The Streamer special-cases this id: spawn an
/// NDI receiver thread, probe the source's actual format, then
/// build the FFmpeg command with a matching rawvideo input on
/// `pipe:0`. The args returned here are placeholders so resolve_source
/// signals "this is an NDI source"; the real args come from
/// [`crate::streamer::build_ffmpeg_cmd_for_ndi`] after probe.
pub fn ndi(source_name: &str) -> Source {
    Source {
        id: "ndi".into(),
        label: source_name.to_string(),
        description: format!("NDI direct: {source_name}"),
        ffmpeg_input_args: Vec::new(), // filled in post-probe by streamer
        available: !source_name.is_empty(),
        notes: if source_name.is_empty() {
            "Pick an NDI sender from the discovery list.".into()
        } else {
            String::new()
        },
        combined_av: false, // NDI video pipes to stdin, audio is silent (Phase 4)
    }
}

/// OMT (Open Media Transport) source — alpha.9 Phase C. Same shape
/// as NDI: video frames pipe into FFmpeg's stdin via the OMT receiver
/// thread, audio defaults to lavfi silence (libomt audio integration
/// deferred to alpha.10). Available iff a source name has been
/// selected AND the `omt` cargo feature is on (otherwise the streamer
/// surface returns an error explaining the feature flag).
pub fn omt(source_name: &str) -> Source {
    Source {
        id: "omt".into(),
        label: source_name.to_string(),
        description: format!("OMT direct: {source_name}"),
        ffmpeg_input_args: Vec::new(), // filled in post-probe by streamer
        available: !source_name.is_empty(),
        notes: if source_name.is_empty() {
            "Pick an OMT sender from the discovery list.".into()
        } else {
            String::new()
        },
        combined_av: false,
    }
}

pub fn rtmp_listen(
    bind_host: &str,
    bind_port: u16,
    app_path: &str,
    stream_name: &str,
) -> Source {
    let url = format!("rtmp://{bind_host}:{bind_port}/{app_path}/{stream_name}");
    Source {
        id: "rtmp_listen".into(),
        label: format!("RTMP in :{bind_port}"),
        description: format!("RTMP listener on {bind_host}:{bind_port}/{app_path}"),
        ffmpeg_input_args: vec![
            "-listen".into(),
            "1".into(),
            "-f".into(),
            "flv".into(),
            "-i".into(),
            url,
        ],
        combined_av: true,
        notes: format!(
            "In OBS: Settings -> Stream -> Custom, Server rtmp://<this-machine-ip>:{bind_port}/{app_path}, Key {stream_name}. \
             FLV/H.264/AAC. Re-encoded into BMD's preferred profile downstream."
        ),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Resolver — turn the EncoderState into a concrete Source
// ---------------------------------------------------------------------------

pub fn resolve_source(state: &EncoderState) -> Result<Source, String> {
    let snap = state.source_selection();
    let (width, height, fps) = snap.dimensions;

    match snap.source_id.as_str() {
        "test_pattern" => Ok(test_pattern(width, height, fps)),
        "pipe" => Ok(pipe(&snap.pipe_path)),
        "avfoundation" => Ok(resolve_capture_device(&snap, width, height, fps)),
        "ndi" => Ok(ndi(&snap.ndi_source_name)),
        "omt" => Ok(omt(&snap.omt_source_name)),
        "srt_listen" => Ok(srt_listen(
            &snap.relay_bind_host,
            snap.relay_srt_port,
            snap.relay_srt_latency_us,
            &snap.relay_srt_passphrase,
        )),
        "rtmp_listen" => Ok(rtmp_listen(
            &snap.relay_bind_host,
            snap.relay_rtmp_port,
            &snap.relay_rtmp_app,
            &snap.relay_rtmp_key,
        )),
        other => Err(format!("Unknown source id: {other:?}")),
    }
}

fn resolve_capture_device(
    snap: &crate::state::SourceSelection,
    width: u32,
    height: u32,
    fps: u32,
) -> Source {
    // audio_mode gates whether the AVF audio device is wired in at
    // all. "auto" / "silent" → audio comes from the video source's
    // combined input (or, for silent, gets muted later by the
    // streamer's audio_filter). "custom" → use the user-picked AVF
    // audio device as a separate input.
    let custom_audio = snap.audio_mode == "custom";
    let effective_audio_index = if custom_audio { snap.av_audio_index } else { -1 };
    let effective_audio_name = if custom_audio {
        snap.av_audio_name.clone()
    } else {
        String::new()
    };

    let devs = list_capture_devices(false);
    let v_dev = devs.video.iter().find(|d| d.index == snap.av_video_index);
    let a_dev = devs.audio.iter().find(|d| d.index == effective_audio_index);
    let v_name_lookup = v_dev.map(|d| d.name.clone()).unwrap_or_else(|| {
        format!("video[{}]", snap.av_video_index)
    });
    let a_name_lookup = a_dev.map(|d| d.name.clone()).unwrap_or_else(|| {
        if effective_audio_index >= 0 {
            format!("audio[{}]", effective_audio_index)
        } else {
            String::new()
        }
    });

    if cfg!(target_os = "macos") {
        let v_name_final = if !snap.av_video_name.is_empty() {
            snap.av_video_name.clone()
        } else {
            v_name_lookup
        };
        let a_name_final = if !effective_audio_name.is_empty() {
            effective_audio_name.clone()
        } else if effective_audio_index >= 0 {
            a_name_lookup
        } else {
            String::new()
        };
        let v_clean = if v_name_final.starts_with("video[") {
            String::new()
        } else {
            v_name_final.clone()
        };
        let a_clean = if a_name_final.starts_with("audio[") {
            String::new()
        } else {
            a_name_final.clone()
        };
        return avfoundation(
            width,
            height,
            fps,
            snap.av_video_index,
            effective_audio_index,
            &v_clean,
            &a_clean,
            &v_name_final,
            &format!(
                "{} + {} @ {}x{}/{}",
                v_name_final,
                if a_name_final.is_empty() {
                    "no audio"
                } else {
                    &a_name_final
                },
                width,
                height,
                fps
            ),
        );
    }

    if cfg!(target_os = "windows") {
        if matches!(v_dev, Some(d) if d.category == "screen") {
            let v_name = v_dev.map(|d| d.name.clone()).unwrap_or_default();
            return gdigrab_desktop(
                width,
                height,
                fps,
                &a_name_lookup,
                &v_name,
                &format!(
                    "{} + {} @ {}x{}/{}",
                    v_name,
                    if a_name_lookup.is_empty() {
                        "no audio"
                    } else {
                        &a_name_lookup
                    },
                    width,
                    height,
                    fps
                ),
            );
        }
        return dshow_capture(
            width,
            height,
            fps,
            &v_name_lookup,
            &a_name_lookup,
            &v_name_lookup,
            &format!(
                "{} + {} @ {}x{}/{}",
                v_name_lookup,
                if a_name_lookup.is_empty() {
                    "no audio"
                } else {
                    &a_name_lookup
                },
                width,
                height,
                fps
            ),
        );
    }

    test_pattern(width, height, fps)
}

#[allow(dead_code)]
pub fn _devs_only_used_in_tests(_d: Device, _m: AvfMode) {}
