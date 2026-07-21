mod audio;
mod config;
mod nats;
mod proto;
mod session;
mod transport;

use anyhow::{Result, bail};
use audio::AudioSink;
use clap::Parser;
use config::{ActivityCfg, Config, NodeConfig};
use nats::NatsPublisher;
use session::{CallEnd, Session, SessionError};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 重连退避的起点
const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
/// 重连退避的上限
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// 呼叫活过这个时长才算「连上过」，退避才归零。
/// 否则「一连上就被踢」会退化成 1 秒一次的死循环重试。
const STABLE_CALL: Duration = Duration::from_secs(30);

#[derive(Parser)]
#[command(
    name = "iaxmon",
    about = "IAX2 客户端 — 监听配置中的一个 AllStarLink 节点"
)]
struct Cli {
    /// 要启动的配置节点 ID；省略时列出所有可用节点。
    #[arg(value_name = "NODE_ID")]
    node: Option<String>,

    /// 配置文件路径
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    /// 输出协议细节、链路状态和周期统计。排查问题时用。
    ///
    /// 默认只打「谁在上话」这类真正关心的信息。RUST_LOG 环境变量优先于本开关。
    #[arg(short, long)]
    verbose: bool,

    /// 把有声音频发布到 Core NATS；开启后不初始化或使用本机声卡。
    #[arg(long)]
    nats: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let default_filter = if cli.verbose {
        "iaxmon=debug"
    } else {
        "iaxmon=info"
    };
    tracing_subscriber::fmt()
        .without_time()
        .with_level(false)
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_filter.into()),
        )
        .init();

    let cfg = Config::load(&cli.config)?;
    let Some(node_id) = cli.node else {
        println!("配置中的节点:");
        for node in &cfg.nodes {
            println!("  {}", node.id);
        }
        return Ok(());
    };
    let nats_cfg = if cli.nats {
        Some(cfg.require_nats()?.clone())
    } else {
        None
    };
    let activity = cfg.activity.clone();
    let available = cfg
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let Some(node) = cfg.nodes.into_iter().find(|node| node.id == node_id) else {
        bail!("节点 {node_id} 不在配置中；可用节点: {available}");
    };

    if let Some(nats_cfg) = nats_cfg {
        let subject = nats_cfg.subject_for(&node.id);
        let publisher = NatsPublisher::connect(&nats_cfg, &subject).await?;
        let idle_timeout = Duration::from_secs(nats_cfg.idle_disconnect_secs);
        return run_node(node, Some(publisher), idle_timeout, &activity).await;
    }

    run_node(node, None, Duration::MAX, &activity).await
}

async fn run_node(
    cfg: NodeConfig,
    nats: Option<NatsPublisher>,
    idle_timeout: Duration,
    activity: &ActivityCfg,
) -> Result<()> {
    tracing::debug!(
        "{}:{} user={}",
        cfg.server.host,
        cfg.server.port,
        cfg.auth.username,
    );
    let mut audience = nats.as_ref().map(NatsPublisher::audience);

    // 两种模式互斥：NATS 模式完全不创建声卡输出流。
    let mut sink = match &nats {
        Some(publisher) => AudioSink::new_nats(activity, publisher.clone()),
        None => AudioSink::new(activity)?,
    };
    let mut backoff = BACKOFF_INITIAL;
    // 顶层统一接管停止信号，覆盖握手/重连/退避等所有状态 —— run() 内部那个只在
    // 已连上时才生效。biased 让 run_call 优先：已连上时收到 SIGINT 仍走 run() 的
    // 优雅挂断路径，只有它没接住的状态才落到这里。
    let mut shutdown = Box::pin(shutdown_signal());

    loop {
        if let Some(receiver) = audience.as_mut()
            && *receiver.borrow() == 0
        {
            tracing::info!("[{}] 等待浏览器开始收听", cfg.id);
            tokio::select! {
                _ = wait_for_listener(receiver) => {
                    tracing::info!("[{}] 检测到监听者，连接 IAX", cfg.id);
                }
                _ = &mut shutdown => {
                    tracing::info!("[{}] 收到停止信号，退出", cfg.id);
                    flush_nats(&nats).await;
                    return Ok(());
                }
            }
        }

        let started = Instant::now();
        let result = tokio::select! {
            biased;
            r = run_call(
                &cfg,
                &mut sink,
                nats.as_ref(),
                audience.as_mut(),
                idle_timeout,
            ) => r,
            _ = &mut shutdown => {
                tracing::info!("[{}] 收到停止信号，退出", cfg.id);
                if let Some(ev) = sink.reset() {
                    session::log_activity(ev, &cfg.id);
                }
                if let Some(publisher) = &nats {
                    publisher.set_online(false);
                }
                flush_nats(&nats).await;
                return Ok(());
            }
        };

        // 断线时若正在上话，立即补收尾并清掉旧呼叫的媒体状态。
        if let Some(ev) = sink.reset() {
            session::log_activity(ev, &cfg.id);
        }
        if let Some(publisher) = &nats {
            publisher.set_online(false);
        }

        match result {
            Ok(CallEnd::Hangup) => {
                tracing::info!("[{}] 已挂断，退出", cfg.id);
                flush_nats(&nats).await;
                return Ok(());
            }
            Ok(CallEnd::Idle) => {
                backoff = BACKOFF_INITIAL;
                continue;
            }
            Ok(CallEnd::Disconnected(why)) => {
                tracing::warn!("[{}] 断线: {why}", cfg.id);
                if started.elapsed() >= STABLE_CALL {
                    backoff = BACKOFF_INITIAL;
                }
            }
            Err(SessionError::Fatal(e)) => {
                tracing::error!("[{}] {e:#}", cfg.id);
                flush_nats(&nats).await;
                return Err(e);
            }
            Err(SessionError::Retry(e)) => tracing::warn!("[{}] 连接失败: {e:#}", cfg.id),
        }

        tracing::info!("[{}] {} 秒后重连", cfg.id, backoff.as_secs());
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = &mut shutdown => {
                tracing::info!("[{}] 退出", cfg.id);
                flush_nats(&nats).await;
                return Ok(());
            }
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// 完成于 SIGINT 或（Unix 上）SIGTERM。docker `stop` 默认发 SIGTERM，
/// 只监听 ctrl_c（=SIGINT）会导致容器停止时干等宽限期后被强杀。
#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            // 装不上 SIGTERM 处理器就退而只等 SIGINT，别让程序起不来。
            tracing::warn!("无法注册 SIGTERM 处理器: {e}，仅监听 Ctrl-C");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn run_call(
    cfg: &NodeConfig,
    sink: &mut AudioSink,
    nats: Option<&NatsPublisher>,
    audience: Option<&mut tokio::sync::watch::Receiver<usize>>,
    idle_timeout: Duration,
) -> session::Result<CallEnd> {
    let mut session = Session::connect(cfg).await?;
    session.handshake().await?;
    if let Some(publisher) = nats {
        publisher.set_online(true);
    }
    session.run(sink, audience, idle_timeout).await
}

async fn wait_for_listener(audience: &mut tokio::sync::watch::Receiver<usize>) {
    while *audience.borrow() == 0 {
        if audience.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

async fn flush_nats(nats: &Option<NatsPublisher>) {
    if let Some(publisher) = nats
        && let Err(e) = publisher.flush().await
    {
        tracing::warn!("刷新 NATS 待发消息失败: {e:#}");
    }
}
