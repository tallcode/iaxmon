use anyhow::{Context, Result, bail};
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
    #[serde(default)]
    pub activity: ActivityCfg,
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
        Self {
            codec: default_codec(),
        }
    }
}

/// 上话活动检测。IAX2 拿不到按键信号，只能靠音频能量判断（PROTOCOL.md §9.4）。
#[derive(Debug, Deserialize)]
pub struct ActivityCfg {
    /// RMS 阈值，超过即判定有人上话。设 0 关闭检测。
    ///
    /// 实测服务端空闲时是精确的数字静音，所以默认值只要远低于任何真实信号即可；
    /// 若你的节点发舒适噪声而非纯静音，把它调到底噪之上。
    #[serde(default = "default_threshold")]
    pub threshold: f32,
    /// 静音多久才判定上话结束。太短会把一句话切成几段。
    #[serde(default = "default_hang_ms")]
    pub hang_ms: u32,
}

impl Default for ActivityCfg {
    fn default() -> Self {
        Self {
            threshold: default_threshold(),
            hang_ms: default_hang_ms(),
        }
    }
}

fn default_threshold() -> f32 {
    50.0
}

fn default_hang_ms() -> u32 {
    500
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
        let cfg: Self = toml::from_str(&text)
            .with_context(|| format!("解析配置文件失败: {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        // 静默地把 codec=gsm 当成 ulaw 用，比直接报错糟糕得多
        if !self.audio.codec.eq_ignore_ascii_case("ulaw") {
            bail!(
                "audio.codec = \"{}\" 不支持，本客户端只实现了 ulaw",
                self.audio.codec
            );
        }
        // CALLING NAME 为空会被 AllStarLink 的 dialplan 直接挂断（PROTOCOL.md §9.2）
        if self.caller.callerid.trim().is_empty() {
            bail!("caller.callerid 不能为空，服务端的 dialplan 会因此直接挂断");
        }
        if self.call.node.trim().is_empty() {
            bail!("call.node 不能为空");
        }
        Ok(())
    }
}
