use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: Server,
    pub auth: Auth,
    pub caller: Caller,
    pub call: Call,
    #[serde(default)]
    pub audio: Audio,
}

#[derive(Debug, Deserialize)]
pub struct Server {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

#[derive(Debug, Deserialize)]
pub struct Auth {
    pub username: String,
    pub secret: String,
}

#[derive(Debug, Deserialize)]
pub struct Caller {
    pub callerid: String,
}

#[derive(Debug, Deserialize)]
pub struct Call {
    pub node: String,
}

#[derive(Debug, Deserialize)]
pub struct Audio {
    #[serde(default = "default_codec")]
    pub codec: String,
}

impl Default for Audio {
    fn default() -> Self {
        Self { codec: default_codec() }
    }
}

fn default_port() -> u16 {
    4569
}

fn default_codec() -> String {
    "ulaw".to_string()
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("解析配置文件失败: {}", path.display()))
    }
}
