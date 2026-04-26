use std::collections::BTreeMap;
use uuid::Uuid;

/// Build the unencoded streamid string in Blackmagic format. The
/// returned string starts with the `#!::` marker — URL-encode it
/// before appending to an SRT URL.
///
/// Real BMD Web Presenters carry the stream key as `u=KEY` (per
/// pcap analysis), with no `m=publish` tag and no `r=` field —
/// that's the default form. Set `legacy_format` to `true` to
/// produce the older `r=KEY,m=publish,...` form for receivers that
/// learned that variant from third-party docs (mediamtx,
/// OvenMediaEngine, etc.).
pub fn build_bmd_streamid(
    stream_key: &str,
    device_name: &str,
    device_uuid: Option<&str>,
    legacy_format: bool,
) -> String {
    let device_uuid = device_uuid
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    // Stream keys with slashes are rejected by Blackmagic devices —
    // strip them defensively.
    let safe_key = stream_key.replace('/', "_");
    let safe_name = device_name.replace(',', " ").replace('=', " ");
    let parts: Vec<String> = if legacy_format {
        vec![
            format!("r={safe_key}"),
            "m=publish".into(),
            format!("bmd_uuid={device_uuid}"),
            format!("bmd_name={safe_name}"),
        ]
    } else {
        vec![
            format!("bmd_uuid={device_uuid}"),
            format!("bmd_name={safe_name}"),
            format!("u={safe_key}"),
        ]
    };
    format!("#!::{}", parts.join(","))
}

#[derive(Debug, Clone)]
pub struct SrtUrlParams<'a> {
    pub host: &'a str,
    pub port: u16,
    pub stream_key: &'a str,
    pub device_name: &'a str,
    pub device_uuid: &'a str,
    pub latency_us: u32,
    pub passphrase: Option<&'a str>,
    pub mode: &'a str,
    pub streamid_override: &'a str,
    pub listen_port: u16,
    pub legacy_streamid: bool,
}

/// Build a fully-formed SRT URL with the BMD-format streamid (or an
/// override) in the query string. Listener-mode binds locally and
/// suppresses the streamid (caller provides it at handshake time).
pub fn build_srt_url(p: &SrtUrlParams) -> String {
    let streamid: String = if !p.streamid_override.is_empty() {
        p.streamid_override.to_string()
    } else {
        build_bmd_streamid(
            p.stream_key,
            p.device_name,
            Some(p.device_uuid),
            p.legacy_streamid,
        )
    };

    // BTreeMap so query params have a stable order — easier to diff
    // against a real BMD pcap when something goes wrong.
    let mut params: BTreeMap<&str, String> = BTreeMap::new();
    params.insert("mode", p.mode.to_string());
    params.insert("latency", p.latency_us.to_string());
    if p.mode != "listener" && !streamid.trim().is_empty() {
        params.insert("streamid", streamid);
    }
    if let Some(pass) = p.passphrase {
        if !pass.is_empty() {
            params.insert("passphrase", pass.to_string());
        }
    }

    let query = params
        .into_iter()
        .map(|(k, v)| format!("{k}={}", url_encode(&v)))
        .collect::<Vec<_>>()
        .join("&");

    if p.mode == "listener" {
        format!("srt://0.0.0.0:{}?{}", p.listen_port, query)
    } else {
        format!("srt://{}:{}?{}", p.host, p.port, query)
    }
}

/// Pull host + port out of a `srt://host[:port][/...]` URL. Falls back
/// to `default_port` if no port component is present.
pub fn parse_srt_host_port(url: &str, default_port: u16) -> (String, u16) {
    let s = if let Some(idx) = url.find("://") {
        &url[idx + 3..]
    } else {
        url
    };
    let s = s.split_once('/').map(|(a, _)| a).unwrap_or(s);
    let s = s.split_once('?').map(|(a, _)| a).unwrap_or(s);
    if let Some((host, port_str)) = s.rsplit_once(':') {
        let port = port_str.parse().unwrap_or(default_port);
        (host.to_string(), port)
    } else {
        (s.to_string(), default_port)
    }
}

/// Minimal percent-encoder for SRT streamid values. RFC 3986 unreserved
/// characters pass through; everything else becomes `%XX`. We don't use
/// the `url` crate because the streamid contains characters (`!`, `#`,
/// `:`, `=`, `,`) that we need to control encoding for individually —
/// the result has to match what real BMD encoders emit.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}
