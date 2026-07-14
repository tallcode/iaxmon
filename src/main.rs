mod audio;
mod config;
mod nats;
mod proto;
mod session;
mod transport;

use anyhow::Result;
use audio::AudioSink;
use clap::Parser;
use config::Config;
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
    about = "IAX2 客户端 — 连接 AllStarLink 节点并播放音频"
)]
struct Cli {
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
    tracing::debug!(
        "{}:{} user={} node={}",
        cfg.server.host,
        cfg.server.port,
        cfg.auth.username,
        cfg.call.node
    );

    let nats = if cli.nats {
        let nats_cfg = cfg.require_nats()?;
        Some(NatsPublisher::connect(nats_cfg).await?)
    } else {
        None
    };

    // 两种模式互斥：NATS 模式完全不创建声卡输出流。
    let mut sink = match &nats {
        Some(publisher) => AudioSink::new_nats(&cfg.activity, publisher.clone()),
        None => AudioSink::new(&cfg.activity)?,
    };
    let mut backoff = BACKOFF_INITIAL;

    loop {
        let started = Instant::now();
        let result = run_call(&cfg, &mut sink, nats.as_ref()).await;

        // 断线时若正在上话，立即补收尾并清掉旧呼叫的媒体状态。
        if let Some(ev) = sink.reset() {
            session::log_activity(ev);
        }
        if let Some(publisher) = &nats {
            publisher.set_online(false);
        }

        match result {
            Ok(CallEnd::Hangup) => {
                tracing::info!("已挂断，退出");
                flush_nats(&nats).await;
                return Ok(());
            }
            Ok(CallEnd::Disconnected(why)) => {
                tracing::warn!("断线: {why}");
                if started.elapsed() >= STABLE_CALL {
                    backoff = BACKOFF_INITIAL;
                }
            }
            Err(SessionError::Fatal(e)) => {
                tracing::error!("{e:#}");
                flush_nats(&nats).await;
                return Err(e);
            }
            Err(SessionError::Retry(e)) => tracing::warn!("连接失败: {e:#}"),
        }

        tracing::info!("{} 秒后重连", backoff.as_secs());
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("退出");
                flush_nats(&nats).await;
                return Ok(());
            }
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

async fn run_call(
    cfg: &Config,
    sink: &mut AudioSink,
    nats: Option<&NatsPublisher>,
) -> session::Result<CallEnd> {
    let mut session = Session::connect(cfg).await?;
    session.handshake().await?;
    if let Some(publisher) = nats {
        publisher.set_online(true);
    }
    session.run(sink).await
}

async fn flush_nats(nats: &Option<NatsPublisher>) {
    if let Some(publisher) = nats
        && let Err(e) = publisher.flush().await
    {
        tracing::warn!("刷新 NATS 待发消息失败: {e:#}");
    }
}
