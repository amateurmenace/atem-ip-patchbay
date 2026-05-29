use anyhow::{anyhow, Result};
use serde::Serialize;
use std::fs;
use std::path::Path;

#[derive(Serialize, Clone, Debug)]
pub struct StreamConfig {
    pub resolution: String,
    pub fps: u32,
    pub codec: String,
    pub bitrate: u64,
    pub audio_bitrate: u64,
    pub keyframe_interval: u32,
}

#[derive(Serialize, Clone, Debug)]
pub struct StreamProfile {
    pub name: String,
    pub low_latency: bool,
    pub configs: Vec<StreamConfig>,
}

impl StreamProfile {
    pub fn find_config(&self, resolution: &str, fps: u32) -> Option<&StreamConfig> {
        self.configs
            .iter()
            .find(|c| c.resolution == resolution && c.fps == fps)
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct StreamServer {
    pub name: String,
    pub group: String,
    pub url: String,
}

impl StreamServer {
    pub fn protocol(&self) -> String {
        self.url
            .split_once("://")
            .map(|(scheme, _)| scheme.to_lowercase())
            .unwrap_or_default()
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct StreamService {
    pub name: String,
    pub key: String,
    pub servers: Vec<StreamServer>,
    pub profiles: Vec<StreamProfile>,
    pub default_profile: String,
}

impl StreamService {
    pub fn srt_servers(&self) -> Vec<&StreamServer> {
        self.servers
            .iter()
            .filter(|s| s.protocol() == "srt")
            .collect()
    }

    pub fn find_profile(&self, name: &str) -> Option<&StreamProfile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    pub fn get_default_profile(&self) -> Option<&StreamProfile> {
        self.find_profile(&self.default_profile)
            .or_else(|| self.profiles.first())
    }
}

pub fn load_service<P: AsRef<Path>>(path: P) -> Result<StreamService> {
    let text = fs::read_to_string(&path)?;
    load_service_text(&text)
}

pub fn load_service_text(text: &str) -> Result<StreamService> {
    let doc = roxmltree::Document::parse(text)?;
    let root = doc.root_element();
    let service_el = if root.tag_name().name() == "service" {
        root
    } else {
        root.children()
            .find(|n| n.is_element() && n.tag_name().name() == "service")
            .ok_or_else(|| anyhow!("no <service> element"))?
    };
    parse_service(service_el)
}

fn child_text<'a>(el: roxmltree::Node<'a, 'a>, name: &str) -> String {
    el.children()
        .find(|n| n.is_element() && n.tag_name().name() == name)
        .and_then(|n| n.text())
        .map(|t| t.trim().to_string())
        .unwrap_or_default()
}

fn parse_service(el: roxmltree::Node) -> Result<StreamService> {
    let name = child_text(el, "name");
    let key = child_text(el, "key");

    let mut servers = Vec::new();
    if let Some(servers_el) = el
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "servers")
    {
        for server_el in servers_el
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "server")
        {
            servers.push(StreamServer {
                name: child_text(server_el, "name"),
                group: server_el.attribute("group").unwrap_or("Default").to_string(),
                url: child_text(server_el, "url"),
            });
        }
    }

    let mut profiles = Vec::new();
    let mut default_profile = String::new();
    if let Some(profiles_el) = el
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "profiles")
    {
        default_profile = profiles_el.attribute("default").unwrap_or("").to_string();
        for profile_el in profiles_el
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "profile")
        {
            profiles.push(parse_profile(profile_el));
        }
    }

    Ok(StreamService {
        name,
        key,
        servers,
        profiles,
        default_profile,
    })
}

fn parse_profile(el: roxmltree::Node) -> StreamProfile {
    let mut configs = Vec::new();
    for cfg_el in el
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "config")
    {
        let parsed = (|| -> Result<StreamConfig> {
            Ok(StreamConfig {
                resolution: cfg_el.attribute("resolution").unwrap_or("").to_string(),
                fps: cfg_el.attribute("fps").unwrap_or("0").parse()?,
                codec: cfg_el.attribute("codec").unwrap_or("").to_string(),
                bitrate: child_text(cfg_el, "bitrate").parse()?,
                audio_bitrate: child_text(cfg_el, "audio-bitrate").parse()?,
                keyframe_interval: child_text(cfg_el, "keyframe-interval").parse()?,
            })
        })();
        if let Ok(cfg) = parsed {
            configs.push(cfg);
        }
    }
    StreamProfile {
        name: child_text(el, "name"),
        low_latency: el
            .children()
            .any(|n| n.is_element() && n.tag_name().name() == "low-latency"),
        configs,
    }
}
