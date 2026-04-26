//! Blackmagic Streaming Encoder Ethernet Protocol — TCP 9977.
//!
//! Implements enough of the v1.2 protocol to look like a real
//! Streaming Encoder to BMD's Streaming Setup utility, ATEM software,
//! or any custom integration. Line-oriented ASCII; messages are
//! "blocks" (header line ending `:`, key/value lines, blank line).
//!
//! Boot order: server connects -> sends a preamble (PROTOCOL PREAMBLE
//! / IDENTITY / VERSION / NETWORK / UI SETTINGS / STREAM SETTINGS /
//! STREAM XML / STREAM STATE / AUDIO SETTINGS / END PRELUDE) -> reads
//! client blocks one at a time. For each client block the server
//! either ACKs and re-broadcasts the affected status to all clients,
//! or NACKs.
//!
//! Architecture: one tokio task per accepted client. Broadcasts use
//! a tokio::sync::broadcast channel that every client task subscribes
//! to. The server itself owns the listener on a free port (9977
//! preferred, walks 9977..9986 to dodge stale instances on the same
//! machine).

use crate::state::{EncoderState, AVAILABLE_VIDEO_MODES};
use crate::streamer::Streamer;

use anyhow::{anyhow, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};

const PROTOCOL_VERSION: &str = "1.2";
const QUALITY_LEVELS: &[&str] = &["Streaming High", "Streaming Medium", "Streaming Low"];

#[derive(Clone, Debug)]
struct BroadcastBlock {
    header: String,
    kvs: Vec<(String, String)>,
}

pub struct ProtocolServer {
    state: Arc<EncoderState>,
    streamer: Arc<Streamer>,
    broadcast: broadcast::Sender<BroadcastBlock>,
}

impl ProtocolServer {
    pub fn new(state: Arc<EncoderState>, streamer: Arc<Streamer>) -> Arc<Self> {
        let (tx, _) = broadcast::channel(64);
        Arc::new(Self {
            state,
            streamer,
            broadcast: tx,
        })
    }

    /// Bind the BMD protocol on 9977 (or the next free port up to
    /// 9986) and start the accept loop. Returns the port we actually
    /// bound — the dev UI shows it in the about-this-instance label.
    pub async fn start(self: &Arc<Self>, start_port: u16) -> Result<u16> {
        let (port, listener) = bind_with_walk(start_port).await?;
        log::info!("BMD control protocol listening on TCP {port}");

        let me = self.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        log::info!("BMD control client connected: {addr}");
                        let me = me.clone();
                        tokio::spawn(async move {
                            if let Err(err) = me.client_loop(stream, addr).await {
                                log::warn!("BMD client loop {addr} ended: {err}");
                            } else {
                                log::info!("BMD control client disconnected: {addr}");
                            }
                        });
                    }
                    Err(err) => {
                        log::warn!("BMD accept failed: {err}");
                        // Brief backoff so a broken socket doesn't
                        // burn a CPU core in the loop.
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
        });
        Ok(port)
    }

    async fn client_loop(self: Arc<Self>, stream: TcpStream, _addr: SocketAddr) -> Result<()> {
        stream.set_nodelay(true)?;
        let (read, write) = stream.into_split();
        let write = Arc::new(Mutex::new(write));

        // Initial dump (preamble + every block + END PRELUDE).
        self.send_preamble(write.clone()).await?;

        // Subscribe to subsequent broadcasts and forward them on a
        // separate task so the read loop never blocks.
        let mut rx = self.broadcast.subscribe();
        let write_for_bcast = write.clone();
        let bcast_task = tokio::spawn(async move {
            while let Ok(msg) = rx.recv().await {
                if send_block(&write_for_bcast, &msg.header, &msg.kvs).await.is_err() {
                    break;
                }
            }
        });

        // Read loop — blocks accumulate until a blank line, then
        // dispatch.
        let mut reader = BufReader::new(read).lines();
        let mut pending: Option<(String, Vec<(String, String)>)> = None;
        while let Ok(Some(raw_line)) = reader.next_line().await {
            let line = raw_line.trim_end_matches('\r').to_string();
            match pending.as_mut() {
                None => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Some(header) = line.strip_suffix(':') {
                        pending = Some((header.trim().to_string(), Vec::new()));
                    }
                    // Lines that don't end in `:` and don't have a
                    // pending block are stray — ignore.
                }
                Some((header, kvs)) => {
                    if line.trim().is_empty() {
                        let header = std::mem::take(header);
                        let kvs = std::mem::take(kvs);
                        pending = None;
                        self.dispatch(&write, &header, &kvs).await?;
                        continue;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        kvs.push((k.trim().to_string(), v.trim().to_string()));
                    }
                }
            }
        }

        bcast_task.abort();
        Ok(())
    }

    async fn dispatch(
        &self,
        write: &Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
        header: &str,
        kvs: &[(String, String)],
    ) -> Result<()> {
        log::info!("BMD RX block {header:?} {kvs:?}");
        let result = match header {
            "IDENTITY" => self.handle_identity(kvs),
            "STREAM SETTINGS" => self.handle_stream_settings(kvs),
            "STREAM STATE" => self.handle_stream_state(kvs).await,
            "STREAM XML" | "NETWORK INTERFACE 0" | "UI SETTINGS" | "AUDIO SETTINGS" => Ok(()),
            "SHUTDOWN" => self.handle_shutdown(kvs).await,
            _ => Err(anyhow!("unknown block header")),
        };

        match result {
            Ok(()) => {
                send_block(write, "ACK", &[]).await?;
                self.broadcast_after(header);
            }
            Err(err) => {
                send_block(write, "NACK", &[]).await?;
                send_block(write, "ERROR", &[("Message".into(), err.to_string())]).await?;
            }
        }
        Ok(())
    }

    fn broadcast_after(&self, header: &str) {
        let snap = self.state.snapshot();
        let msg = match header {
            "STREAM SETTINGS" => BroadcastBlock {
                header: "STREAM SETTINGS".into(),
                kvs: stream_settings_kvs(&snap),
            },
            "STREAM STATE" => BroadcastBlock {
                header: "STREAM STATE".into(),
                kvs: stream_state_kvs(&snap),
            },
            "IDENTITY" => BroadcastBlock {
                header: "IDENTITY".into(),
                kvs: identity_kvs(&snap),
            },
            "AUDIO SETTINGS" => BroadcastBlock {
                header: "AUDIO SETTINGS".into(),
                kvs: audio_kvs(),
            },
            _ => return,
        };
        // Errors mean no subscribers — fine, just drop.
        let _ = self.broadcast.send(msg);
    }

    fn handle_identity(&self, kvs: &[(String, String)]) -> Result<()> {
        for (k, v) in kvs {
            if k == "Label" {
                self.state.set_label(v);
            }
        }
        Ok(())
    }

    fn handle_stream_settings(&self, kvs: &[(String, String)]) -> Result<()> {
        let mut update = crate::state::SettingsUpdate::default();
        for (k, v) in kvs {
            match k.as_str() {
                "Video Mode" if AVAILABLE_VIDEO_MODES.contains(&v.as_str()) => {
                    update.video_mode = Some(v.clone())
                }
                "Current Platform" => update.current_service_name = Some(v.clone()),
                "Current Quality Level" => update.quality_level = Some(v.clone()),
                "Stream Key" => update.stream_key = Some(v.clone()),
                "Password" => update.passphrase = Some(v.clone()),
                "Current Server" => update.current_server_name = Some(v.clone()),
                _ => {}
            }
        }
        self.state.apply_settings(&update);
        Ok(())
    }

    async fn handle_stream_state(&self, kvs: &[(String, String)]) -> Result<()> {
        for (k, v) in kvs {
            if k == "Action" {
                match v.trim().to_lowercase().as_str() {
                    "start" => {
                        let streamer = self.streamer.clone();
                        let result = streamer.start().await;
                        if let Err(err) = result {
                            self.state.stats_in_place(|s| {
                                s.status = "Interrupted".into();
                                s.error = Some(err.to_string());
                            });
                            return Err(err);
                        }
                    }
                    "stop" => {
                        let _ = self.streamer.stop().await;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    async fn handle_shutdown(&self, kvs: &[(String, String)]) -> Result<()> {
        for (k, v) in kvs {
            if k == "Action" && v.trim().eq_ignore_ascii_case("factory reset") {
                let _ = self.streamer.stop().await;
                self.state.stats_in_place(|s| s.error = None);
            }
        }
        Ok(())
    }

    async fn send_preamble(
        &self,
        write: Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    ) -> Result<()> {
        let snap = self.state.snapshot();
        send_block(
            &write,
            "PROTOCOL PREAMBLE",
            &[("Version".into(), PROTOCOL_VERSION.into())],
        )
        .await?;
        send_block(&write, "IDENTITY", &identity_kvs(&snap)).await?;
        send_block(&write, "VERSION", &version_kvs()).await?;
        send_lines(
            &write,
            &["NETWORK:", "Interface Count: 1", "Default Interface: 0", ""],
        )
        .await?;
        send_lines(
            &write,
            &[
                "NETWORK INTERFACE 0:",
                "Name: Ethernet",
                "Priority: 1",
                "MAC Address: 00:11:22:33:44:55",
                "Dynamic IP: true",
                "Current Addresses: 0.0.0.0/255.255.255.0",
                "Current Gateway: 0.0.0.0",
                "Current DNS Servers: ",
                "Static Addresses: 0.0.0.0/255.255.255.0",
                "Static Gateway: 0.0.0.0",
                "Static DNS Servers: 8.8.8.8, 8.8.4.4",
                "",
            ],
        )
        .await?;
        send_block(&write, "UI SETTINGS", &ui_kvs()).await?;
        send_block(&write, "STREAM SETTINGS", &stream_settings_kvs(&snap)).await?;
        send_block(
            &write,
            "STREAM XML",
            &[("Files".into(), snap.available_services.join(", "))],
        )
        .await?;
        send_block(&write, "STREAM STATE", &stream_state_kvs(&snap)).await?;
        send_block(&write, "AUDIO SETTINGS", &audio_kvs()).await?;
        send_block(&write, "END PRELUDE", &[]).await?;
        Ok(())
    }
}

// ---- block builders --------------------------------------------------------

fn identity_kvs(snap: &crate::state::Snapshot) -> Vec<(String, String)> {
    vec![
        ("Model".into(), snap.model.clone()),
        ("Label".into(), snap.label.clone()),
        ("Unique ID".into(), snap.unique_id.clone()),
    ]
}

fn version_kvs() -> Vec<(String, String)> {
    vec![
        ("Product ID".into(), "BE73".into()),
        ("Hardware Version".into(), "0100".into()),
        ("Software Version".into(), "01000000".into()),
        ("Software Release".into(), "0.2".into()),
    ]
}

fn ui_kvs() -> Vec<(String, String)> {
    vec![
        ("Available Locales".into(), "en_US.UTF-8".into()),
        ("Current Locale".into(), "en_US.UTF-8".into()),
        (
            "Available Audio Meters".into(),
            "PPM -18dB, PPM -20dB, VU -18dB, VU -20dB".into(),
        ),
        ("Current Audio Meter".into(), "PPM -20dB".into()),
    ]
}

fn audio_kvs() -> Vec<(String, String)> {
    vec![
        (
            "Current Monitor Out Audio Source".into(),
            "Auto".into(),
        ),
        (
            "Available Monitor Out Audio Sources".into(),
            "Auto, SDI In, Remote Source".into(),
        ),
    ]
}

fn stream_settings_kvs(snap: &crate::state::Snapshot) -> Vec<(String, String)> {
    let server_names = if snap.available_servers.is_empty() {
        "SRT".to_string()
    } else {
        snap.available_servers
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    };
    vec![
        (
            "Available Video Modes".into(),
            AVAILABLE_VIDEO_MODES.join(", "),
        ),
        ("Video Mode".into(), snap.video_mode.clone()),
        (
            "Current Platform".into(),
            if snap.current_service_name.is_empty() {
                "My Platform".into()
            } else {
                snap.current_service_name.clone()
            },
        ),
        ("Current Server".into(), snap.current_server_name.clone()),
        ("Current Quality Level".into(), snap.quality_level.clone()),
        ("Stream Key".into(), snap.stream_key.clone()),
        ("Password".into(), snap.passphrase.clone()),
        ("Current URL".into(), snap.current_url.clone()),
        ("Customizable URL".into(), "true".into()),
        ("Available Default Platforms".into(), String::new()),
        (
            "Available Custom Platforms".into(),
            snap.available_services.join(", "),
        ),
        ("Available Servers".into(), server_names),
        ("Available Quality Levels".into(), QUALITY_LEVELS.join(", ")),
    ]
}

fn stream_state_kvs(snap: &crate::state::Snapshot) -> Vec<(String, String)> {
    vec![
        ("Status".into(), snap.stats.status.clone()),
        ("Bitrate".into(), snap.stats.bitrate.to_string()),
        ("Duration".into(), snap.stats.duration.clone()),
        ("Cache Used".into(), snap.stats.cache_used.to_string()),
    ]
}

// ---- low-level send helpers ------------------------------------------------

async fn send_block(
    write: &Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    header: &str,
    kvs: &[(String, String)],
) -> Result<()> {
    let mut payload = String::new();
    payload.push_str(header);
    payload.push_str(":\n");
    for (k, v) in kvs {
        payload.push_str(k);
        payload.push_str(": ");
        payload.push_str(v);
        payload.push('\n');
    }
    payload.push('\n');
    let mut w = write.lock().await;
    w.write_all(payload.as_bytes()).await?;
    Ok(())
}

async fn send_lines(
    write: &Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    lines: &[&str],
) -> Result<()> {
    let payload = lines.join("\n") + "\n";
    let mut w = write.lock().await;
    w.write_all(payload.as_bytes()).await?;
    Ok(())
}

async fn bind_with_walk(start_port: u16) -> Result<(u16, TcpListener)> {
    let mut last_err: Option<std::io::Error> = None;
    for offset in 0..10 {
        let port = start_port + offset;
        let addr: SocketAddr = ([0, 0, 0, 0], port).into();
        match TcpListener::bind(addr).await {
            Ok(listener) => return Ok((port, listener)),
            Err(err) => last_err = Some(err),
        }
    }
    Err(anyhow!(
        "could not bind BMD protocol on TCP {start_port}-{}: {}",
        start_port + 9,
        last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "no error".into())
    ))
}
