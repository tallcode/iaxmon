//! Core NATS 实时音频发布器。

use crate::config::NatsCfg;
use anyhow::{Context, Result, anyhow};
use async_nats::connection::State as ConnectionState;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, watch};

const PROTOCOL_VERSION: u8 = 1;
const AUDIO_MESSAGE_TYPE: u8 = 1;
const PUBLISH_QUEUE_CAPACITY: usize = 256;
/// 音频不占用最后几个槽位，确保 start/stop/state 总能按顺序排进同一队列。
const CONTROL_RESERVE: usize = 8;
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
const LISTENER_SWEEP_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, Serialize)]
struct PublisherState {
    #[serde(rename = "type")]
    kind: &'static str,
    online: bool,
    speaking: bool,
    listeners: usize,
}

impl Default for PublisherState {
    fn default() -> Self {
        Self {
            kind: "state",
            online: false,
            speaking: false,
            listeners: 0,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ListenerReport {
    #[serde(rename = "type")]
    kind: String,
    gateway_id: String,
    count: usize,
}

struct GatewayListeners {
    count: usize,
    expires_at: Instant,
}

enum Command {
    SetOnline(bool),
    Start { timestamp: u32 },
    Stop { timestamp: u32, duration_ms: u64 },
    Audio(Vec<u8>),
    Flush(oneshot::Sender<std::result::Result<(), String>>),
}

#[derive(Clone)]
struct Subjects {
    audio: String,
    events: String,
    snapshot: String,
    listeners: String,
}

impl Subjects {
    fn new(prefix: &str) -> Self {
        Self {
            audio: format!("{prefix}.audio"),
            events: format!("{prefix}.events"),
            snapshot: format!("{prefix}.snapshot"),
            listeners: format!("{prefix}.listeners"),
        }
    }
}

/// 同步媒体路径只把消息放入有界队列；实际 NATS I/O 在独立 Tokio task 中进行。
#[derive(Clone)]
pub struct NatsPublisher {
    commands: mpsc::Sender<Command>,
    sequence: Arc<AtomicU32>,
    dropped: Arc<AtomicU64>,
    audience: watch::Receiver<usize>,
}

impl NatsPublisher {
    pub async fn connect(cfg: &NatsCfg, subject_prefix: &str) -> Result<Self> {
        let servers = cfg
            .servers
            .iter()
            .map(|server| {
                server
                    .parse::<async_nats::ServerAddr>()
                    .with_context(|| format!("NATS 服务器地址无效: {server}"))
            })
            .collect::<Result<Vec<_>>>()?;

        let mut options = match (
            cfg.token.as_deref().filter(|value| !value.is_empty()),
            cfg.username.as_deref().filter(|value| !value.is_empty()),
            cfg.password.as_deref().filter(|value| !value.is_empty()),
        ) {
            (Some(token), None, None) => async_nats::ConnectOptions::with_token(token.to_owned()),
            (None, Some(username), Some(password)) => {
                async_nats::ConnectOptions::with_user_and_password(
                    username.to_owned(),
                    password.to_owned(),
                )
            }
            _ => async_nats::ConnectOptions::new(),
        };
        options = options
            .name("iaxmon")
            .max_reconnects(None)
            .event_callback(|event| async move {
                match event {
                    async_nats::Event::Connected => tracing::info!("NATS 已连接"),
                    async_nats::Event::Disconnected => tracing::warn!("NATS 断线，等待重连"),
                    other => tracing::warn!("NATS 事件: {other}"),
                }
            });

        let client = options
            .connect(servers)
            .await
            .context("连接 NATS 集群失败")?;
        let subjects = Subjects::new(subject_prefix);
        let snapshots = client
            .subscribe(subjects.snapshot.clone())
            .await
            .context("订阅 NATS 状态快照 subject 失败")?;
        let listeners = client
            .subscribe(subjects.listeners.clone())
            .await
            .context("订阅 NATS Gateway 监听人数 subject 失败")?;

        let (command_tx, command_rx) = mpsc::channel(PUBLISH_QUEUE_CAPACITY);
        let (audience_tx, audience_rx) = watch::channel(0);
        tokio::spawn(run_publisher(
            client,
            subjects.clone(),
            command_rx,
            snapshots,
            listeners,
            audience_tx,
            Duration::from_secs(cfg.listener_lease_secs),
        ));

        tracing::info!(
            "NATS 发布已启用: {} ({} 个集群入口)",
            subject_prefix,
            cfg.servers.len()
        );
        Ok(Self {
            commands: command_tx,
            sequence: Arc::new(AtomicU32::new(0)),
            dropped: Arc::new(AtomicU64::new(0)),
            audience: audience_rx,
        })
    }

    pub fn audience(&self) -> watch::Receiver<usize> {
        self.audience.clone()
    }

    pub fn set_online(&self, online: bool) {
        self.send_control(Command::SetOnline(online));
    }

    pub fn publish_start(&self, timestamp: u32) {
        self.send_control(Command::Start { timestamp });
    }

    pub fn publish_stop(&self, timestamp: u32, duration_secs: f64) {
        self.send_control(Command::Stop {
            timestamp,
            duration_ms: (duration_secs * 1000.0).round() as u64,
        });
    }

    pub fn publish_audio(&self, timestamp: u32, payload: Vec<u8>) {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let message = encode_audio(sequence, timestamp, &payload);
        if self.commands.capacity() <= CONTROL_RESERVE
            || self.commands.try_send(Command::Audio(message)).is_err()
        {
            let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped == 1 || dropped.is_power_of_two() {
                tracing::warn!("NATS 音频发布队列已满，累计丢弃 {dropped} 帧");
            }
        }
    }

    fn send_control(&self, command: Command) {
        if let Err(error) = self.commands.try_send(command) {
            tracing::error!("NATS 控制事件无法入队: {error}");
        }
    }

    pub async fn flush(&self) -> Result<()> {
        let (done_tx, done_rx) = oneshot::channel();
        self.commands
            .send(Command::Flush(done_tx))
            .await
            .map_err(|_| anyhow!("NATS 发布任务已经退出"))?;
        let result = tokio::time::timeout(FLUSH_TIMEOUT, done_rx)
            .await
            .context("等待 NATS flush 超时")?
            .context("NATS 发布任务未返回 flush 结果")?;
        result.map_err(|message| anyhow!(message))
    }
}

async fn run_publisher(
    client: async_nats::Client,
    subjects: Subjects,
    mut commands: mpsc::Receiver<Command>,
    mut snapshots: async_nats::Subscriber,
    mut listener_reports: async_nats::Subscriber,
    audience_tx: watch::Sender<usize>,
    listener_lease: Duration,
) {
    let mut state = PublisherState::default();
    let mut gateways = HashMap::<String, GatewayListeners>::new();
    let mut listener_sweep = tokio::time::interval(LISTENER_SWEEP_INTERVAL);
    listener_sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    listener_sweep.tick().await;

    loop {
        tokio::select! {
            biased;

            Some(command) = commands.recv() => {
                match command {
                    Command::SetOnline(online) => {
                        state.online = online;
                        if !online {
                            state.speaking = false;
                        }
                        publish_json(&client, &subjects.events, &state).await;
                    }
                    Command::Start { timestamp } => {
                        state.speaking = true;
                        publish_json(&client, &subjects.events, &json!({
                            "type": "start",
                            "timestamp": timestamp,
                        })).await;
                    }
                    Command::Stop { timestamp, duration_ms } => {
                        state.speaking = false;
                        publish_json(&client, &subjects.events, &json!({
                            "type": "stop",
                            "timestamp": timestamp,
                            "duration_ms": duration_ms,
                        })).await;
                    }
                    Command::Audio(message) => {
                        // Core NATS 恢复连接后不能补发陈旧实时音频；断线期间直接丢弃。
                        if client.connection_state() == ConnectionState::Connected
                            && let Err(error) = client.publish(subjects.audio.clone(), message.into()).await
                        {
                            tracing::warn!("发布 NATS 音频失败: {error}");
                        }
                    }
                    Command::Flush(done) => {
                        let result = client.flush().await.map_err(|error| error.to_string());
                        let _ = done.send(result);
                    }
                }
            }
            Some(request) = snapshots.next() => {
                if let Some(reply) = request.reply {
                    match serde_json::to_vec(&state) {
                        Ok(payload) => {
                            if let Err(error) = client.publish(reply, payload.into()).await {
                                tracing::warn!("回复 NATS 状态快照失败: {error}");
                            }
                        }
                        Err(error) => tracing::error!("序列化 NATS 状态快照失败: {error}"),
                    }
                }
            }
            Some(message) = listener_reports.next() => {
                match serde_json::from_slice::<ListenerReport>(&message.payload) {
                    Ok(report) if report.kind == "listeners" && !report.gateway_id.is_empty() => {
                        if report.count == 0 {
                            gateways.remove(&report.gateway_id);
                        } else {
                            gateways.insert(report.gateway_id, GatewayListeners {
                                count: report.count,
                                expires_at: Instant::now() + listener_lease,
                            });
                        }
                        update_audience(&client, &subjects, &mut state, &gateways, &audience_tx).await;
                    }
                    Ok(_) => tracing::warn!("忽略格式不正确的 Gateway 监听人数消息"),
                    Err(error) => tracing::warn!("忽略无法解析的 Gateway 监听人数消息: {error}"),
                }
            }
            _ = listener_sweep.tick() => {
                let now = Instant::now();
                gateways.retain(|_, gateway| gateway.expires_at > now);
                update_audience(&client, &subjects, &mut state, &gateways, &audience_tx).await;
            }
            else => return,
        }
    }
}

async fn update_audience(
    client: &async_nats::Client,
    subjects: &Subjects,
    state: &mut PublisherState,
    gateways: &HashMap<String, GatewayListeners>,
    audience_tx: &watch::Sender<usize>,
) {
    let listeners = gateways
        .values()
        .fold(0usize, |sum, gateway| sum.saturating_add(gateway.count));
    if listeners == state.listeners {
        return;
    }
    state.listeners = listeners;
    audience_tx.send_replace(listeners);
    let prefix = subjects
        .listeners
        .strip_suffix(".listeners")
        .unwrap_or(&subjects.listeners);
    tracing::info!("[{prefix}] 当前监听人数: {listeners}");
    publish_json(client, &subjects.events, state).await;
}

async fn publish_json<T: Serialize>(client: &async_nats::Client, subject: &str, value: &T) {
    match serde_json::to_vec(value) {
        Ok(payload) => {
            if let Err(error) = client.publish(subject.to_owned(), payload.into()).await {
                tracing::warn!("发布 NATS 事件失败: {error}");
            }
        }
        Err(error) => tracing::error!("序列化 NATS 事件失败: {error}"),
    }
}

fn encode_audio(sequence: u32, timestamp: u32, payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(10 + payload.len());
    message.push(PROTOCOL_VERSION);
    message.push(AUDIO_MESSAGE_TYPE);
    message.extend_from_slice(&sequence.to_be_bytes());
    message.extend_from_slice(&timestamp.to_be_bytes());
    message.extend_from_slice(payload);
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_header_is_stable() {
        let message = encode_audio(0x0506_0708, 0x0102_0304, &[0xff; 160]);
        assert_eq!(message.len(), 170);
        assert_eq!(&message[..10], &[1, 1, 5, 6, 7, 8, 1, 2, 3, 4]);
        assert!(message[10..].iter().all(|&byte| byte == 0xff));
    }

    #[test]
    fn subjects_share_configured_prefix() {
        let subjects = Subjects::new("iaxmon.nodes.1999");
        assert_eq!(subjects.audio, "iaxmon.nodes.1999.audio");
        assert_eq!(subjects.events, "iaxmon.nodes.1999.events");
        assert_eq!(subjects.snapshot, "iaxmon.nodes.1999.snapshot");
        assert_eq!(subjects.listeners, "iaxmon.nodes.1999.listeners");
    }
}
