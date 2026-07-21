use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug)]
pub struct Config {
    pub nodes: Vec<NodeConfig>,
    pub nats: Option<NatsCfg>,
    pub activity: ActivityCfg,
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    nodes: Vec<NodeConfig>,
    server: Option<Server>,
    auth: Option<Auth>,
    caller: Option<LegacyCaller>,
    call: Option<LegacyCall>,
    audio: Option<LegacyAudio>,
    activity: Option<ActivityCfg>,
    nats: Option<NatsCfg>,
}

#[derive(Debug, Deserialize)]
struct LegacyCaller {
    callerid: String,
}

#[derive(Debug, Deserialize)]
struct LegacyCall {
    node: String,
}

#[derive(Debug, Deserialize)]
struct LegacyAudio {
    #[serde(default = "default_codec")]
    codec: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeConfig {
    /// 节点 ID，同时作为 IAX2 呼叫的目标 extension（CALLED NUMBER）。
    pub id: String,
    pub server: Server,
    pub auth: Auth,
    /// CALLING NAME IE。留空时默认使用 `auth.username`。
    /// ASL 的 dialplan 在它为空时会直接挂断。
    pub callerid: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Server {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Auth {
    pub username: String,
    pub secret: String,
}

/// Core NATS 集群及发布 subject 配置。只有 `--nats` 模式要求这一节存在。
#[derive(Debug, Clone, Deserialize)]
pub struct NatsCfg {
    /// 初始集群节点。客户端连上任意一个后还会接受集群通告并自动重连。
    pub servers: Vec<String>,
    /// 多节点 subject 根；节点 ID 会自动追加在后面。
    pub subject_root: Option<String>,
    /// 兼容旧版单节点配置；多节点配置必须改用 subject_root。
    pub subject_prefix: Option<String>,
    /// 用户密码认证。两项必须同时填写，且不能和 token 同时使用。
    pub username: Option<String>,
    pub password: Option<String>,
    /// Token 认证；不能和 username/password 同时使用。
    pub token: Option<String>,
    /// Gateway 监听人数心跳多久未更新后过期。
    #[serde(default = "default_listener_lease_secs")]
    pub listener_lease_secs: u64,
    /// 所有 Gateway 均无人收听多久后断开 IAX。
    #[serde(default = "default_idle_disconnect_secs")]
    pub idle_disconnect_secs: u64,
}

/// 上话活动检测。IAX2 拿不到按键信号，只能靠音频能量判断（PROTOCOL.md §9.4）。
#[derive(Debug, Clone, Deserialize)]
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

fn default_listener_lease_secs() -> u64 {
    45
}

fn default_idle_disconnect_secs() -> u64 {
    60
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
        let raw: RawConfig = toml::from_str(&text)
            .with_context(|| format!("解析配置文件失败: {}", path.display()))?;
        let cfg = Self::from_raw(raw)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn from_raw(raw: RawConfig) -> Result<Self> {
        let RawConfig {
            mut nodes,
            server,
            auth,
            caller,
            call,
            audio,
            activity,
            nats,
        } = raw;
        let has_legacy = server.is_some()
            || auth.is_some()
            || caller.is_some()
            || call.is_some()
            || audio.is_some();

        if !nodes.is_empty() && has_legacy {
            bail!("不能混用旧版顶层节点配置和 [[nodes]]")
        }
        if nodes.is_empty() && has_legacy {
            let server = server.context("旧版配置缺少 [server]")?;
            let auth = auth.context("旧版配置缺少 [auth]")?;
            let callerid = caller.context("旧版配置缺少 [caller]")?.callerid;
            let id = call.context("旧版配置缺少 [call]")?.node;
            if let Some(audio) = audio
                && !audio.codec.eq_ignore_ascii_case("ulaw")
            {
                bail!(
                    "audio.codec = \"{}\" 不支持，本客户端只实现了 ulaw",
                    audio.codec
                );
            }
            nodes.push(NodeConfig {
                id,
                server,
                auth,
                callerid: Some(callerid),
            });
        }

        Ok(Self {
            nodes,
            nats,
            activity: activity.unwrap_or_default(),
        })
    }

    pub fn require_nats(&self) -> Result<&NatsCfg> {
        self.nats
            .as_ref()
            .context("使用 --nats 时 config.toml 必须包含 [nats] 配置")
    }

    fn validate(&self) -> Result<()> {
        if self.nodes.is_empty() {
            bail!("至少需要配置一个 [[nodes]] 节点");
        }
        let mut ids = HashSet::new();
        for node in &self.nodes {
            node.validate()?;
            if !ids.insert(&node.id) {
                bail!("nodes.id 重复: {}", node.id);
            }
        }
        if let Some(nats) = &self.nats {
            nats.validate(self.nodes.len())?;
        }
        Ok(())
    }
}

impl NodeConfig {
    /// CALLING NAME：显式配置的 callerid，缺省回落到 auth.username。
    pub fn callerid_or(&self) -> &str {
        self.callerid
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&self.auth.username)
    }

    fn validate(&self) -> Result<()> {
        validate_subject_token(&self.id, "nodes.id")?;
        if self.server.host.trim().is_empty() {
            bail!("节点 {} 的 server.host 不能为空", self.id);
        }
        if self.server.port == 0 {
            bail!("节点 {} 的 server.port 必须大于 0", self.id);
        }
        if self.auth.username.trim().is_empty() {
            bail!("节点 {} 的 auth.username 不能为空", self.id);
        }
        if self.auth.secret.trim().is_empty() {
            bail!("节点 {} 的 auth.secret 不能为空", self.id);
        }
        Ok(())
    }
}

impl NatsCfg {
    fn validate(&self, node_count: usize) -> Result<()> {
        if self.servers.is_empty() || self.servers.iter().any(|server| server.trim().is_empty()) {
            bail!("nats.servers 至少要包含一个非空的 NATS 地址");
        }

        match (&self.subject_root, &self.subject_prefix) {
            (Some(_), Some(_)) => {
                bail!("nats.subject_root 和 nats.subject_prefix 只能配置一个")
            }
            (None, None) => bail!("[nats] 必须配置 subject_root"),
            (None, Some(_)) if node_count > 1 => {
                bail!("多节点配置必须使用 nats.subject_root，不能使用旧版 subject_prefix")
            }
            (Some(root), None) => validate_subject_root(root, "nats.subject_root")?,
            (None, Some(prefix)) => validate_subject_root(prefix, "nats.subject_prefix")?,
        }

        let has_username = self.username.as_ref().is_some_and(|v| !v.is_empty());
        let has_password = self.password.as_ref().is_some_and(|v| !v.is_empty());
        if has_username != has_password {
            bail!("nats.username 和 nats.password 必须同时配置");
        }
        if self.token.as_ref().is_some_and(|v| !v.is_empty()) && has_username {
            bail!("NATS token 与 username/password 只能选择一种认证方式");
        }
        if self.listener_lease_secs == 0 {
            bail!("nats.listener_lease_secs 必须大于 0");
        }
        if self.idle_disconnect_secs == 0 {
            bail!("nats.idle_disconnect_secs 必须大于 0");
        }
        Ok(())
    }

    pub fn subject_for(&self, node_id: &str) -> String {
        match &self.subject_root {
            Some(root) => format!("{root}.{node_id}"),
            None => self
                .subject_prefix
                .clone()
                .expect("validated NATS config has a subject"),
        }
    }
}

fn validate_subject_root(value: &str, name: &str) -> Result<()> {
    if value != value.trim()
        || value.is_empty()
        || value.starts_with('.')
        || value.ends_with('.')
        || value.contains("..")
        || value
            .chars()
            .any(|c| c.is_whitespace() || c == '*' || c == '>')
    {
        bail!("{name} 不是合法的 NATS subject 根");
    }
    Ok(())
}

fn validate_subject_token(value: &str, name: &str) -> Result<()> {
    if value.is_empty()
        || value
            .chars()
            .any(|c| c == '.' || c.is_whitespace() || c == '*' || c == '>')
    {
        bail!("{name} 必须是单个合法的 NATS subject token");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nats() -> NatsCfg {
        NatsCfg {
            servers: vec!["nats://127.0.0.1:4222".into()],
            subject_root: Some("iaxmon.nodes".into()),
            subject_prefix: None,
            username: None,
            password: None,
            token: None,
            listener_lease_secs: default_listener_lease_secs(),
            idle_disconnect_secs: default_idle_disconnect_secs(),
        }
    }

    #[test]
    fn valid_nats_cluster_config() {
        let mut cfg = nats();
        cfg.servers.push("nats://127.0.0.2:4222".into());
        assert!(cfg.validate(2).is_ok());
    }

    #[test]
    fn nats_servers_cannot_be_empty() {
        let mut cfg = nats();
        cfg.servers.clear();
        assert!(cfg.validate(2).is_err());
    }

    #[test]
    fn nats_subject_root_cannot_contain_wildcards() {
        let mut cfg = nats();
        cfg.subject_root = Some("iaxmon.nodes.*".into());
        assert!(cfg.validate(2).is_err());
    }

    #[test]
    fn nats_subject_root_cannot_have_surrounding_whitespace() {
        let mut cfg = nats();
        cfg.subject_root = Some(" iaxmon.nodes ".into());
        assert!(cfg.validate(2).is_err());
    }

    #[test]
    fn nats_username_requires_password() {
        let mut cfg = nats();
        cfg.username = Some("iaxmon".into());
        assert!(cfg.validate(2).is_err());
    }

    #[test]
    fn nats_auth_methods_are_mutually_exclusive() {
        let mut cfg = nats();
        cfg.username = Some("iaxmon".into());
        cfg.password = Some("secret".into());
        cfg.token = Some("token".into());
        assert!(cfg.validate(2).is_err());
    }

    #[test]
    fn nats_mode_requires_nats_section() {
        let raw: RawConfig = toml::from_str(
            r#"
                [[nodes]]
                id = "1999"
                [nodes.server]
                host = "example.invalid"
                [nodes.auth]
                username = "user"
                secret = "secret"
            "#,
        )
        .unwrap();
        let cfg = Config::from_raw(raw).unwrap();
        assert!(cfg.require_nats().is_err());
    }

    #[test]
    fn legacy_single_node_config_is_migrated() {
        let raw: RawConfig = toml::from_str(
            r#"
                [server]
                host = "example.invalid"
                [auth]
                username = "N0CALL"
                secret = "secret"
                [caller]
                callerid = "N0CALL"
                [call]
                node = "1999"
                [audio]
                codec = "ulaw"
                [nats]
                servers = ["nats://127.0.0.1:4222"]
                subject_prefix = "iaxmon.nodes.1999"
            "#,
        )
        .unwrap();
        let cfg = Config::from_raw(raw).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.nodes.len(), 1);
        assert_eq!(cfg.nodes[0].id, "1999");
        assert_eq!(cfg.nodes[0].callerid_or(), "N0CALL");
        assert_eq!(
            cfg.require_nats().unwrap().subject_for("1999"),
            "iaxmon.nodes.1999"
        );
    }

    #[test]
    fn empty_username_is_rejected_even_with_default_callerid() {
        let node: NodeConfig = toml::from_str(
            r#"
                id = "1999"
                [server]
                host = "example.invalid"
                [auth]
                username = "  "
                secret = "secret"
            "#,
        )
        .unwrap();
        assert!(node.validate().is_err());
    }

    #[test]
    fn multi_node_subjects_are_derived_from_ids() {
        let cfg = nats();
        assert_eq!(cfg.subject_for("1900"), "iaxmon.nodes.1900");
        assert_eq!(cfg.subject_for("1800"), "iaxmon.nodes.1800");
    }

    #[test]
    fn legacy_subject_prefix_is_rejected_for_multiple_nodes() {
        let mut cfg = nats();
        cfg.subject_root = None;
        cfg.subject_prefix = Some("iaxmon.nodes.1900".into());
        assert!(cfg.validate(2).is_err());
        assert!(cfg.validate(1).is_ok());
    }

    #[test]
    fn example_config_contains_two_valid_nodes() {
        let raw: RawConfig = toml::from_str(include_str!("../config.example.toml")).unwrap();
        let cfg = Config::from_raw(raw).unwrap();
        cfg.validate().unwrap();
        assert_eq!(
            cfg.nodes
                .iter()
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>(),
            ["1900", "1800"]
        );
    }
}
