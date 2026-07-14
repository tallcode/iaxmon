mod audio;
mod config;
mod proto;
mod session;
mod transport;

use anyhow::Result;
use audio::AudioSink;
use clap::Parser;
use config::Config;
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

    // 输出流开一次就一直开着，重连期间只是没样本进来，回调自己填静音
    let mut sink = AudioSink::new(&cfg.activity)?;
    let mut backoff = BACKOFF_INITIAL;

    loop {
        // 断线时若正在上话，补一条收尾记录，免得日志里只有开始没有结束
        if let Some(ev) = sink.reset() {
            session::log_activity(ev, &mut None);
        }
        let started = Instant::now();

        match run_call(&cfg, &mut sink).await {
            Ok(CallEnd::Hangup) => {
                tracing::info!("已挂断，退出");
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
                return Err(e);
            }
            Err(SessionError::Retry(e)) => tracing::warn!("连接失败: {e:#}"),
        }

        tracing::info!("{} 秒后重连", backoff.as_secs());
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("退出");
                return Ok(());
            }
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

async fn run_call(cfg: &Config, sink: &mut AudioSink) -> session::Result<CallEnd> {
    let mut session = Session::connect(cfg).await?;
    session.handshake().await?;
    session.run(sink).await
}
