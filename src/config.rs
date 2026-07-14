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
    pub nats: Option<NatsCfg>,
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

/// Core NATS 集群及发布 subject 配置。只有 `--nats` 模式要求这一节存在。
#[derive(Debug, Deserialize)]
pub struct NatsCfg {
    /// 初始集群节点。客户端连上任意一个后还会接受集群通告并自动重连。
    pub servers: Vec<String>,
    /// 本节点使用的 subject 根；实际发布到 `.audio` / `.events` / `.snapshot`。
    pub subject_prefix: String,
    /// 用户密码认证。两项必须同时填写，且不能和 token 同时使用。
    pub username: Option<String>,
    pub password: Option<String>,
    /// Token 认证；不能和 username/password 同时使用。
    pub token: Option<String>,
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

    pub fn require_nats(&self) -> Result<&NatsCfg> {
        self.nats
            .as_ref()
            .context("使用 --nats 时 config.toml 必须包含 [nats] 配置")
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
        if let Some(nats) = &self.nats {
            nats.validate()?;
        }
        Ok(())
    }
}

impl NatsCfg {
    fn validate(&self) -> Result<()> {
        if self.servers.is_empty() || self.servers.iter().any(|server| server.trim().is_empty()) {
            bail!("nats.servers 至少要包含一个非空的 NATS 地址");
        }

        let prefix = self.subject_prefix.trim();
        if prefix.is_empty()
            || prefix.starts_with('.')
            || prefix.ends_with('.')
            || prefix.contains("..")
            || prefix
                .chars()
                .any(|c| c.is_whitespace() || c == '*' || c == '>')
        {
            bail!("nats.subject_prefix 不是合法的 NATS subject 根");
        }

        let has_username = self.username.as_ref().is_some_and(|v| !v.is_empty());
        let has_password = self.password.as_ref().is_some_and(|v| !v.is_empty());
        if has_username != has_password {
            bail!("nats.username 和 nats.password 必须同时配置");
        }
        if self.token.as_ref().is_some_and(|v| !v.is_empty()) && has_username {
            bail!("NATS token 与 username/password 只能选择一种认证方式");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nats() -> NatsCfg {
        NatsCfg {
            servers: vec!["nats://127.0.0.1:4222".into()],
            subject_prefix: "iaxmon.nodes.1999".into(),
            username: None,
            password: None,
            token: None,
        }
    }

    #[test]
    fn valid_nats_cluster_config() {
        let mut cfg = nats();
        cfg.servers.push("nats://127.0.0.2:4222".into());
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn nats_servers_cannot_be_empty() {
        let mut cfg = nats();
        cfg.servers.clear();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn nats_subject_prefix_cannot_contain_wildcards() {
        let mut cfg = nats();
        cfg.subject_prefix = "iaxmon.nodes.*".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn nats_username_requires_password() {
        let mut cfg = nats();
        cfg.username = Some("iaxmon".into());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn nats_auth_methods_are_mutually_exclusive() {
        let mut cfg = nats();
        cfg.username = Some("iaxmon".into());
        cfg.password = Some("secret".into());
        cfg.token = Some("token".into());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn nats_mode_requires_nats_section() {
        let cfg: Config = toml::from_str(
            r#"
                [server]
                host = "example.invalid"
                [auth]
                username = "user"
                secret = "secret"
                [caller]
                callerid = "CALL"
                [call]
                node = "1999"
            "#,
        )
        .unwrap();
        assert!(cfg.require_nats().is_err());
    }
}
