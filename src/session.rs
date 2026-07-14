//! 呼叫状态机：握手、序列号、ACK、保活、收语音。

use crate::audio::AudioSink;
use crate::config::Config;
use crate::proto::consts::{
    IAX_PROTO_VERSION, auth_method, control, format, frame_type, ie as ie_id, iax,
};
use crate::proto::{Frame, FullFrame, Ie, Ies};
use crate::transport::Transport;
use anyhow::{Context, anyhow};
use md5::{Digest, Md5};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::time::{MissedTickBehavior, interval, timeout};

/// 接听**之前**多久没包才算断线。
///
/// ASL 的 dialplan 里有硬编码的 `Wait(10)`，振铃期间服务端完全静默约 10 秒。
/// 阈值必须远大于它，否则会在接听前一刻自判断线，然后重连→振铃→再超时，
/// 陷入死循环，一帧音频都收不到。见 PROTOCOL.md §9.2。
const SILENCE_TIMEOUT_RINGING: Duration = Duration::from_secs(30);

/// 接听**之后**多久没包才算断线。
///
/// 接听后服务端按 50 帧/秒持续推流，不管 RF 侧有没有人上话（PROTOCOL.md §9.4）。
/// 静默 5 秒等于丢了 250 帧，链路铁定没了，不必等满 30 秒。
const SILENCE_TIMEOUT_ANSWERED: Duration = Duration::from_secs(5);
/// 握手帧的重传起点
const RETRY_INITIAL: Duration = Duration::from_millis(500);
/// 握手帧的重传间隔上限
const RETRY_MAX: Duration = Duration::from_secs(10);
/// 握手帧最多发几次（含首发）
const RETRY_ATTEMPTS: u32 = 4;
/// 对端 ACK 了我们的握手帧之后，等它给出实质应答的时限
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// 播放侧的出帧节奏
const TICK: Duration = Duration::from_millis(20);
/// 多久打一次统计
const STATS_INTERVAL: Duration = Duration::from_secs(30);

/// 呼叫的结束方式。
pub enum CallEnd {
    /// 用户 Ctrl-C
    Hangup,
    /// 可重连的断线
    Disconnected(String),
}

/// 区分「重连有用」和「重连没用」。
pub enum SessionError {
    /// 配置或凭据问题，重试多少次都一样
    Fatal(anyhow::Error),
    /// 网络或对端状态问题，可以重连
    Retry(anyhow::Error),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fatal(e) | Self::Retry(e) => write!(f, "{e:#}"),
        }
    }
}

/// 默认把错误当成可重连的。致命错误必须显式构造，免得漏判导致无限重试。
impl From<anyhow::Error> for SessionError {
    fn from(e: anyhow::Error) -> Self {
        Self::Retry(e)
    }
}

pub type Result<T> = std::result::Result<T, SessionError>;

/// 半个 16 位窗口。候选值和基准差出这么多，就说明它其实在相邻的窗口里。
const HALF_WINDOW: i32 = 0x8000;
/// 一个完整的 16 位时间戳窗口 = 65536ms
const WINDOW: u32 = 0x1_0000;

/// Mini frame 只带 16 位时间戳，要靠最近的 full frame 补出高 16 位。
///
/// 做法照搬 Asterisk 的 `unwrap_timestamp()`：拿基准的高 16 位拼一个候选值，再看它
/// 相对基准落在哪个窗口。**必须双向判断** —— 只处理向前回绕会出大事，见 `extend`。
#[derive(Default)]
struct VoiceClock {
    /// 最近一次判定的完整时间戳，作为判断基准
    last: u32,
}

impl VoiceClock {
    /// Full voice frame 带完整时间戳，直接对表。
    ///
    /// 乱序的旧 full frame 不会走到这里 —— 它在序列号检查那关就被当重复帧丢了。
    fn sync(&mut self, timestamp: u32) {
        self.last = timestamp;
    }

    /// 把 16 位低位补成 32 位完整时间戳。
    ///
    /// 两条约束缺一不可，违反任何一条的后果都是**永久静音且无法自愈**（见下）：
    ///
    /// 1. 回绕判断必须双向。跨越回绕边界乱序到达的旧帧，真值属于上一个窗口，
    ///    只判「低位变小很多 → 进位」会把它算高 65536ms。
    /// 2. 基准只能在时间戳前进时推进。倒退的帧若把基准拖回旧窗口，紧接着的正常帧
    ///    就会相对这个基准被误判成向前回绕而错误进位；高位只增不减，此后永久偏移。
    ///
    /// 之所以是永久静音而非杂音：约 65 秒后服务端会发来 full voice frame，`sync()`
    /// 把基准拉回真值，于是此后所有帧都比抖动缓冲的播放位置落后 65 秒，被当迟到帧全部
    /// 丢弃；而包仍在正常到达，静默超时不触发，重连也不会启动。
    fn extend(&mut self, low: u16) -> u32 {
        // 先假设它和基准在同一个窗口
        let candidate = (self.last & 0xFFFF_0000) | low as u32;
        // 再看差值把它修正到正确的窗口
        let delta = candidate.wrapping_sub(self.last) as i32;
        let ts = if delta < -HALF_WINDOW {
            candidate.wrapping_add(WINDOW) // 其实在下一个窗口
        } else if delta > HALF_WINDOW {
            candidate.wrapping_sub(WINDOW) // 其实在上一个窗口
        } else {
            candidate
        };

        // 只有前进才推进基准。乱序到达的旧帧不能把基准拖回去。
        if ts.wrapping_sub(self.last) as i32 > 0 {
            self.last = ts;
        }
        ts
    }
}

pub struct Session<'a> {
    cfg: &'a Config,
    transport: Transport,
    source_call: u16,
    dest_call: u16,
    oseqno: u8,
    iseqno: u8,
    start: Instant,
    clock: VoiceClock,
    /// 收到 CONTROL/ANSWER 之后为 true。决定用哪个静默阈值 —— 振铃期和通话期
    /// 服务端的发包节奏完全不同，不能用同一把尺子量。
    answered: bool,
}

impl<'a> Session<'a> {
    pub async fn connect(cfg: &'a Config) -> Result<Self> {
        let transport = Transport::connect(&cfg.server.host, cfg.server.port).await?;
        Ok(Self {
            cfg,
            transport,
            source_call: random_call_number(),
            dest_call: 0,
            oseqno: 0,
            iseqno: 0,
            start: Instant::now(),
            clock: VoiceClock::default(),
            answered: false,
        })
    }

    /// NEW → (CALLTOKEN → NEW) → AUTHREQ → AUTHREP → ACCEPT
    pub async fn handshake(&mut self) -> Result<()> {
        tracing::info!("呼叫 {} @ {}", self.cfg.call.node, self.transport.peer());

        let ies = new_ies(self.cfg, &[]);
        let reply = self.send_reliable(frame_type::IAX, iax::NEW, ies).await?;

        // 服务端要求呼叫令牌：拿它下发的 token 重发一次 NEW。
        // 这个应答是 Asterisk 的 send_apathetic_reply() 发的，源呼叫号是写死的 1，
        // 不是真正的呼叫号，所以整个呼叫状态要推倒重来，只留 token。
        let reply = if reply.frame_type == frame_type::IAX && reply.subclass == iax::CALLTOKEN {
            let token = reply
                .ies()
                .context("CALLTOKEN 的 IE 解析失败")?
                .get(ie_id::CALLTOKEN)
                .context("CALLTOKEN 帧里没有 CALLTOKEN IE")?
                .data
                .clone();
            tracing::debug!("服务端要求呼叫令牌 ({} 字节)，带上令牌重发 NEW", token.len());

            self.oseqno = 0;
            self.iseqno = 0;
            self.dest_call = 0;
            let ies = new_ies(self.cfg, &token);
            self.send_reliable(frame_type::IAX, iax::NEW, ies).await?
        } else {
            reply
        };

        let authreq = match (reply.frame_type, reply.subclass) {
            (frame_type::IAX, iax::AUTHREQ) => reply,
            (frame_type::IAX, iax::REJECT) => {
                // 呼叫被拒，多半是 CALLED NUMBER 不对。带上服务端给的原因。
                return Err(SessionError::Retry(anyhow!("呼叫被拒绝: {}", cause_of(&reply))));
            }
            _ => return Err(SessionError::Retry(anyhow!("NEW 之后收到意外的 {}", describe(&reply)))),
        };
        self.adopt(&authreq);

        let ies = authreq.ies().context("AUTHREQ 的 IE 解析失败")?;
        let methods = ies
            .get(ie_id::AUTHMETHODS)
            .context("AUTHREQ 缺少 AUTHMETHODS")?
            .as_u16()
            .context("AUTHMETHODS 格式错误")?;
        if methods & auth_method::MD5 == 0 {
            return Err(SessionError::Fatal(anyhow!(
                "服务端不接受 MD5 认证 (AUTHMETHODS=0x{methods:04x})，本客户端只实现了 MD5"
            )));
        }
        let challenge =
            ies.get(ie_id::CHALLENGE).context("AUTHREQ 缺少 CHALLENGE")?.as_string();

        let mut ies = Ies::new();
        ies.push(Ie::string(ie_id::MD5_RESULT, &md5_response(&challenge, &self.cfg.auth.secret)));
        let reply = self.send_reliable(frame_type::IAX, iax::AUTHREP, ies).await?;

        let accept = match (reply.frame_type, reply.subclass) {
            (frame_type::IAX, iax::ACCEPT) => reply,
            (frame_type::IAX, iax::REJECT) => {
                // 认证阶段被拒 = 用户名或密码不对，重试没有意义
                return Err(SessionError::Fatal(anyhow!(
                    "认证被拒绝: {} — 检查 config.toml 的 username/secret",
                    cause_of(&reply)
                )));
            }
            _ => {
                return Err(SessionError::Retry(anyhow!("AUTHREP 之后收到意外的 {}", describe(&reply))));
            }
        };
        self.adopt(&accept);
        self.send_ack(accept.timestamp).await?;

        // ACCEPT 里的 FORMAT 是服务端最终选定的编码。不是 ulaw 我们解不了。
        if let Some(fmt) = accept.ies().ok().and_then(|i| i.get(ie_id::FORMAT).and_then(|ie| ie.as_u32().ok()))
            && fmt != format::ULAW
        {
            return Err(SessionError::Fatal(anyhow!(
                "服务端选定的编码是 0x{fmt:08x}，本客户端只支持 ulaw (0x{:08x})",
                format::ULAW
            )));
        }

        tracing::info!("呼叫已接受 (call {} ↔ {})", self.source_call, self.dest_call);
        Ok(())
    }

    /// 握手完成后的主循环。
    pub async fn run(&mut self, sink: &mut AudioSink) -> Result<CallEnd> {
        let mut ticker = interval(TICK);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut stats = interval(STATS_INTERVAL);
        stats.set_missed_tick_behavior(MissedTickBehavior::Skip);
        stats.tick().await; // interval 的第一拍是立即触发的，丢掉

        let mut sigint = Box::pin(tokio::signal::ctrl_c());
        let mut last_rx = Instant::now();

        loop {
            tokio::select! {
                packet = self.transport.recv() => {
                    last_rx = Instant::now();
                    if let Some(end) = self.handle_packet(&packet?, sink).await? {
                        return Ok(end);
                    }
                }
                _ = ticker.tick() => {
                    sink.tick();
                    let limit = if self.answered {
                        SILENCE_TIMEOUT_ANSWERED
                    } else {
                        SILENCE_TIMEOUT_RINGING
                    };
                    if last_rx.elapsed() > limit {
                        return Ok(CallEnd::Disconnected(
                            format!("{} 秒没收到任何包", limit.as_secs())
                        ));
                    }
                }
                _ = stats.tick() => {
                    let (jb, underruns, depth) = sink.stats();
                    tracing::info!(
                        "统计: 缓冲 {depth} 帧 / 迟到 {} / 抖动欠载 {} / 溢出 {} / 漂移丢帧 {} / 输出欠载 {underruns}",
                        jb.late, jb.underruns, jb.overflows, jb.skew_drops,
                    );
                }
                _ = &mut sigint => {
                    tracing::info!("收到 Ctrl-C，挂断");
                    self.hangup().await;
                    return Ok(CallEnd::Hangup);
                }
            }
        }
    }

    async fn handle_packet(&mut self, buf: &[u8], sink: &mut AudioSink) -> Result<Option<CallEnd>> {
        let frame = match Frame::parse(buf) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("丢弃无法解析的包 ({} 字节): {e:#}", buf.len());
                return Ok(None);
            }
        };

        match frame {
            Frame::Meta => Ok(None), // trunk/video，不支持
            Frame::Mini(m) => {
                if m.source_call != self.dest_call {
                    tracing::warn!("mini frame 的呼叫号 {} 不是本呼叫，丢弃", m.source_call);
                    return Ok(None);
                }
                let ts = self.clock.extend(m.timestamp);
                sink.push_frame(ts, m.payload);
                Ok(None)
            }
            Frame::Full(f) => self.handle_full(f, sink).await,
        }
    }

    async fn handle_full(&mut self, f: FullFrame, sink: &mut AudioSink) -> Result<Option<CallEnd>> {
        if f.dest_call != self.source_call {
            tracing::warn!("full frame 发给了呼叫号 {}，不是我们的 {}", f.dest_call, self.source_call);
            return Ok(None);
        }
        tracing::debug!("← {}", describe(&f));

        // ACK 这几类不占序列号 —— 收到的 ACK 里带的 oseqno 是对端「下一个要发的」，
        // 并没有被消耗掉。要是照常推进 iseqno，紧接着的真帧就会被误判成重复帧丢弃。
        if !(f.frame_type == frame_type::IAX && iax::no_seq_increment(f.subclass)) {
            // 序列号对不上说明是重复帧：补一个 ACK 就丢掉，别推进状态
            if f.oseqno != self.iseqno {
                tracing::debug!("重复帧 oseqno={} (期望 {})，重发 ACK", f.oseqno, self.iseqno);
                self.send_ack(f.timestamp).await?;
                return Ok(None);
            }
            self.iseqno = self.iseqno.wrapping_add(1);
        }

        match f.frame_type {
            frame_type::IAX => match f.subclass {
                // PING/POKE 的应答就是 PONG，PONG 本身再被对端 ACK
                iax::PING | iax::POKE => {
                    self.send_at(frame_type::IAX, iax::PONG, Ies::new(), f.timestamp).await?;
                }
                iax::LAGRQ => {
                    self.send_at(frame_type::IAX, iax::LAGRP, Ies::new(), f.timestamp).await?;
                }
                iax::PONG | iax::LAGRP => self.send_ack(f.timestamp).await?,
                iax::ACK => {}
                iax::HANGUP => {
                    self.send_ack(f.timestamp).await?;
                    return Ok(Some(CallEnd::Disconnected(format!("对端挂断: {}", cause_of(&f)))));
                }
                iax::REJECT => {
                    self.send_ack(f.timestamp).await?;
                    return Ok(Some(CallEnd::Disconnected(format!("对端拒绝: {}", cause_of(&f)))));
                }
                iax::INVAL => {
                    return Ok(Some(CallEnd::Disconnected("呼叫被对端作废 (INVAL)".into())));
                }
                _ => {
                    tracing::debug!("未处理的 IAX 子类 0x{:02x}，回 ACK", f.subclass);
                    self.send_ack(f.timestamp).await?;
                }
            },
            frame_type::CONTROL => {
                self.send_ack(f.timestamp).await?;
                match f.subclass {
                    control::RINGING => tracing::info!("振铃"),
                    control::ANSWER => {
                        self.answered = true;
                        tracing::info!("对端接听，开始收音频");
                    }
                    control::HANGUP => {
                        return Ok(Some(CallEnd::Disconnected("对端挂断".into())));
                    }
                    control::BUSY => return Ok(Some(CallEnd::Disconnected("对端忙".into()))),
                    control::CONGESTION => {
                        return Ok(Some(CallEnd::Disconnected("线路拥塞".into())));
                    }
                    other => tracing::debug!("未处理的 CONTROL 子类 0x{other:02x}"),
                }
            }
            frame_type::VOICE => {
                self.send_ack(f.timestamp).await?;
                if f.subclass != format::ULAW {
                    tracing::warn!("收到非 ulaw 语音帧 (0x{:08x})，丢弃", f.subclass);
                } else {
                    // full voice frame 带完整时间戳，拿它给 mini frame 的时钟对表
                    self.clock.sync(f.timestamp);
                    sink.push_frame(f.timestamp, f.payload);
                }
            }
            other => {
                tracing::debug!("未处理的帧类型 0x{other:02x}，回 ACK");
                self.send_ack(f.timestamp).await?;
            }
        }
        Ok(None)
    }

    /// 发出并等待**实质应答**，超时按退避重传。只用于握手 —— 握手之后我们的上行都是
    /// ACK/PONG 这类幂等帧，丢了对端会重发，不值得维护重传队列。
    ///
    /// 关键点：Asterisk 会先回一个裸 ACK 确认收到，再单独发 AUTHREQ/ACCEPT。
    /// ACK 只代表「送达」，不是应答，收到之后要停止重传但继续等真正的回复。
    async fn send_reliable(&mut self, ft: u8, subclass: u32, ies: Ies) -> Result<FullFrame> {
        let mut frame = self.build(ft, subclass, ies)?;
        let mut delay = RETRY_INITIAL;
        let mut sends_left = RETRY_ATTEMPTS;

        self.transport.send(&frame.encode().context("编码失败")?).await?;
        tracing::debug!("→ {}", describe(&frame));
        sends_left -= 1;

        loop {
            match timeout(delay, self.recv_full()).await {
                Ok(reply) => {
                    let reply = reply?;
                    tracing::debug!("← {}", describe(&reply));
                    if reply.frame_type == frame_type::IAX && reply.subclass == iax::ACK {
                        // 对端确认收到了，重传没意义了，但实质应答还在路上
                        sends_left = 0;
                        delay = REPLY_TIMEOUT;
                        continue;
                    }
                    return Ok(reply);
                }
                Err(_) if sends_left > 0 => {
                    frame.retransmit = true;
                    sends_left -= 1;
                    tracing::warn!("{} 超时，重传", describe(&frame));
                    self.transport.send(&frame.encode().context("编码失败")?).await?;
                    delay = (delay * 2).min(RETRY_MAX);
                }
                Err(_) => {
                    return Err(SessionError::Retry(anyhow!("{} 没有等到应答", describe(&frame))));
                }
            }
        }
    }

    /// 采纳对端在这个呼叫里的身份：它的呼叫号，以及它的序列号进度。
    /// 只对真正建立了呼叫状态的应答调用 —— CALLTOKEN 那种 apathetic reply 不算。
    fn adopt(&mut self, reply: &FullFrame) {
        self.dest_call = reply.source_call;
        self.iseqno = reply.oseqno.wrapping_add(1);
    }

    /// 阻塞到收下一个发给本呼叫的 full frame。只在握手和挂断时用 —— 这两个阶段
    /// 还没有音频链路，mini/meta 帧都是噪声。
    async fn recv_full(&mut self) -> Result<FullFrame> {
        loop {
            let buf = self.transport.recv().await?;
            match Frame::parse(&buf) {
                Ok(Frame::Full(f)) if f.dest_call == self.source_call => return Ok(f),
                Ok(Frame::Full(f)) => {
                    tracing::warn!("收到发给呼叫号 {} 的 full frame，不是我们的，忽略", f.dest_call);
                }
                Ok(_) => tracing::trace!("收到非 full frame，此阶段忽略"),
                Err(e) => tracing::warn!("丢弃无法解析的包: {e:#}"),
            }
        }
    }

    fn build(&mut self, ft: u8, subclass: u32, ies: Ies) -> Result<FullFrame> {
        self.build_at(ft, subclass, ies, self.timestamp())
    }

    fn build_at(&mut self, ft: u8, subclass: u32, ies: Ies, ts: u32) -> Result<FullFrame> {
        let mut payload = Vec::new();
        ies.encode(&mut payload).context("IE 编码失败")?;
        let frame = FullFrame {
            source_call: self.source_call,
            dest_call: self.dest_call,
            retransmit: false,
            timestamp: ts,
            oseqno: self.oseqno,
            iseqno: self.iseqno,
            frame_type: ft,
            subclass,
            payload,
        };
        // ACK 这几类不占序列号，否则对端会认为我们跳号
        if !(ft == frame_type::IAX && iax::no_seq_increment(subclass)) {
            self.oseqno = self.oseqno.wrapping_add(1);
        }
        Ok(frame)
    }

    /// 回应类的帧要带上被回应帧的时间戳，不是我们自己的当前时间。
    async fn send_at(&mut self, ft: u8, subclass: u32, ies: Ies, ts: u32) -> Result<()> {
        let frame = self.build_at(ft, subclass, ies, ts)?;
        tracing::debug!("→ {}", describe(&frame));
        self.transport.send(&frame.encode().context("编码失败")?).await?;
        Ok(())
    }

    async fn send_ack(&mut self, ts: u32) -> Result<()> {
        self.send_at(frame_type::IAX, iax::ACK, Ies::new(), ts).await
    }

    /// 尽力而为：发 HANGUP，给对端一点时间回 ACK，无论结果都退出。
    async fn hangup(&mut self) {
        let mut ies = Ies::new();
        ies.push(Ie::string(ie_id::CAUSE, "User hangup"));
        ies.push(Ie::u8(ie_id::CAUSE_CODE, 16)); // ITU Q.850 normal clearing

        if let Err(e) = self.send_at(frame_type::IAX, iax::HANGUP, ies, self.timestamp()).await {
            tracing::warn!("发送 HANGUP 失败: {e}");
            return;
        }
        let _ = timeout(Duration::from_millis(500), self.recv_full()).await;
    }

    fn timestamp(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }
}

/// 组装 NEW 帧的 IE。`token` 是服务端下发的呼叫令牌；首次呼叫传空切片，
/// 表示「我支持 CallToken，请给我一个」。
///
/// 只发这七个，别的一概不发 —— 理由见 DESIGN.md §2.2。CALLING NAME 不能省，
/// ASL 的 dialplan 在它为空时会直接挂断。
fn new_ies(cfg: &Config, token: &[u8]) -> Ies {
    let mut ies = Ies::new();
    ies.push(Ie::u16(ie_id::VERSION, IAX_PROTO_VERSION));
    ies.push(Ie::string(ie_id::USERNAME, &cfg.auth.username));
    ies.push(Ie::string(ie_id::CALLED_NUMBER, &cfg.call.node));
    ies.push(Ie::string(ie_id::CALLING_NAME, &cfg.caller.callerid));
    ies.push(Ie::u32(ie_id::CAPABILITY, format::ULAW));
    ies.push(Ie::u32(ie_id::FORMAT, format::ULAW));
    ies.push(Ie { id: ie_id::CALLTOKEN, data: token.to_vec() });
    ies
}

fn md5_response(challenge: &str, secret: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(challenge.as_bytes());
    hasher.update(secret.as_bytes());
    hasher.finalize().iter().fold(String::with_capacity(32), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// 呼叫号是 15 位非 0。每次重连换一个，免得和服务端残留的旧呼叫状态撞上。
fn random_call_number() -> u16 {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1);
    (nanos % 0x7fff) as u16 + 1
}

/// 从 REJECT/HANGUP 里取服务端给的原因，用于日志。
fn cause_of(f: &FullFrame) -> String {
    let Ok(ies) = f.ies() else {
        return "(原因无法解析)".into();
    };
    let text = ies.get(ie_id::CAUSE).map(|ie| ie.as_string());
    let code = ies.get(ie_id::CAUSE_CODE).and_then(|ie| ie.as_u8().ok());
    match (text, code) {
        (Some(t), Some(c)) => format!("{t} (code {c})"),
        (Some(t), None) => t,
        (None, Some(c)) => format!("cause code {c}"),
        (None, None) => "(服务端没给原因)".into(),
    }
}

fn describe(f: &FullFrame) -> String {
    let name = match f.frame_type {
        frame_type::IAX => format!("IAX/{}", iax::name(f.subclass)),
        frame_type::CONTROL => format!("CONTROL/{}", control::name(f.subclass)),
        frame_type::VOICE => format!("VOICE/0x{:x} ({} 字节)", f.subclass, f.payload.len()),
        other => format!("type=0x{other:02x}/0x{:x}", f.subclass),
    };
    format!("{name} ts={} o={} i={}", f.timestamp, f.oseqno, f.iseqno)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 样例配置() -> Config {
        toml::from_str(
            r#"
            [server]
            host = "node.example"
            port = 4569
            [auth]
            username = "N0CALL"
            secret = "test-secret"
            [caller]
            callerid = "N0CALL"
            [call]
            node = "1999"
        "#,
        )
        .unwrap()
    }

    /// NEW 帧的 IE 组成是和服务端 dialplan 直接挂钩的契约，逐字节锁住。
    ///
    /// 尤其是 CALLING NAME：ASL 的 dialplan 在它为空时会直接挂断（PROTOCOL.md §9.2），
    /// 少发它的后果是彻底连不上。
    #[test]
    fn new_帧的_ie_线格式() {
        let mut buf = Vec::new();
        new_ies(&样例配置(), &[]).encode(&mut buf).unwrap();

        assert_eq!(
            buf,
            vec![
                0x0b, 0x02, 0x00, 0x02, // VERSION = 2
                0x06, 0x06, b'N', b'0', b'C', b'A', b'L', b'L', // USERNAME
                0x01, 0x04, b'1', b'9', b'9', b'9', // CALLED NUMBER
                0x04, 0x06, b'N', b'0', b'C', b'A', b'L', b'L', // CALLING NAME
                0x08, 0x04, 0x00, 0x00, 0x00, 0x04, // CAPABILITY = ulaw
                0x09, 0x04, 0x00, 0x00, 0x00, 0x04, // FORMAT = ulaw
                0x36, 0x00, // CALLTOKEN，空 = 「请给我一个令牌」
            ]
        );
    }

    /// 多发的 IE 会带来意料之外的服务端行为，少发的会连不上。锁住集合本身。
    #[test]
    fn new_帧只发这七个_ie() {
        let ies = new_ies(&样例配置(), &[]);
        let ids: Vec<u8> = ies.0.iter().map(|ie| ie.id).collect();
        assert_eq!(
            ids,
            vec![
                ie_id::VERSION,
                ie_id::USERNAME,
                ie_id::CALLED_NUMBER,
                ie_id::CALLING_NAME,
                ie_id::CAPABILITY,
                ie_id::FORMAT,
                ie_id::CALLTOKEN,
            ]
        );
        // 明确不发的：CALLING NUMBER 留空不发，CALLED CONTEXT 由服务端决定
        assert!(ies.get(ie_id::CALLING_NUMBER).is_none());
        assert!(ies.get(ie_id::CALLED_CONTEXT).is_none());
    }

    /// 拿到令牌后重发的 NEW，除 CALLTOKEN 外必须和首次完全一致。
    #[test]
    fn new_帧带令牌重发时其余_ie_不变() {
        let cfg = 样例配置();
        let 首次 = new_ies(&cfg, &[]);
        let 重发 = new_ies(&cfg, b"1752499200?0123456789abcdef0123456789abcdef01234567");

        assert_eq!(首次.0.len(), 重发.0.len());
        for (a, b) in 首次.0.iter().zip(重发.0.iter()) {
            assert_eq!(a.id, b.id);
            if a.id != ie_id::CALLTOKEN {
                assert_eq!(a.data, b.data, "IE 0x{:02x} 不该随令牌变化", a.id);
            }
        }
        assert_eq!(重发.get(ie_id::CALLTOKEN).unwrap().data.len(), 51);
    }

    /// RFC 5456 的 MD5 认证就是 md5(challenge || secret) 的小写十六进制。
    /// 测试用假密码 —— 真密码只存在于 config.toml，绝不进仓库。
    #[test]
    fn md5_应答格式() {
        let r = md5_response("123456", "test-secret");
        assert_eq!(r.len(), 32);
        assert!(r.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // 参照值来自 `printf '123456test-secret' | md5`
        assert_eq!(r, "ffb1fa75bbd4f07c4ef9c9ce291ff45a");
    }

    #[test]
    fn md5_不同挑战给不同应答() {
        assert_ne!(md5_response("aaa", "test-secret"), md5_response("bbb", "test-secret"));
    }

    #[test]
    fn 呼叫号合法() {
        for _ in 0..100 {
            let n = random_call_number();
            assert!(n >= 1 && n <= 0x7fff, "呼叫号 {n} 越界");
        }
    }

    #[test]
    fn 时钟_full_帧对表() {
        let mut c = VoiceClock::default();
        c.sync(0x0001_2345);
        assert_eq!(c.extend(0x2365), 0x0001_2365);
    }

    #[test]
    fn 时钟_低位回绕时进位() {
        let mut c = VoiceClock::default();
        c.sync(0x0000_fff0);
        assert_eq!(c.extend(0xfff8), 0x0000_fff8);
        assert_eq!(c.extend(0x0010), 0x0001_0010); // 回绕
        assert_eq!(c.extend(0x0020), 0x0001_0020);
    }

    #[test]
    fn 时钟_小幅乱序不误判为回绕() {
        let mut c = VoiceClock::default();
        c.sync(0x0001_0100);
        assert_eq!(c.extend(0x00f0), 0x0001_00f0); // 比上一个小，但差值远不到半周期
        assert_eq!(c.extend(0x0110), 0x0001_0110);
    }

    /// 跨越回绕边界乱序到达的旧帧，必须算回上一个窗口，不能算成当前窗口。
    #[test]
    fn 时钟_跨界乱序的旧帧要向后回绕() {
        let mut c = VoiceClock::default();
        c.sync(0x0001_0008); // 刚过完回绕点
        // 真值是 0x0000_fff0（上一窗口，约 24ms 之前），不是 0x0001_fff0
        assert_eq!(c.extend(0xfff0), 0x0000_fff0);
    }

    /// 这是那个会导致永久静音的组合：跨界乱序帧之后，基准不能被拖回旧窗口，
    /// 否则下一个正常帧会被误判成向前回绕，高位错误进位且永不回退。
    #[test]
    fn 时钟_跨界乱序不污染后续帧() {
        let mut c = VoiceClock::default();
        c.sync(0x0001_0008);

        c.extend(0xfff0); // 跨界乱序的旧帧

        // 后续正常帧必须仍在 0x0001 窗口，不能跳到 0x0002
        assert_eq!(c.extend(0x0010), 0x0001_0010, "基准被乱序帧拖回，导致高位误进位");
        assert_eq!(c.extend(0x0030), 0x0001_0030);
        assert_eq!(c.extend(0x0050), 0x0001_0050);
    }

    /// 重复帧（同一个低位反复到达）不应该推进任何东西
    #[test]
    fn 时钟_重复帧幂等() {
        let mut c = VoiceClock::default();
        c.sync(0x0001_0100);
        assert_eq!(c.extend(0x0120), 0x0001_0120);
        assert_eq!(c.extend(0x0120), 0x0001_0120);
        assert_eq!(c.extend(0x0120), 0x0001_0120);
        assert_eq!(c.extend(0x0140), 0x0001_0140);
    }

    /// 正常的 20ms 步进跑过多个回绕边界，不能有任何漂移
    #[test]
    fn 时钟_连续步进跨多个边界() {
        let mut c = VoiceClock::default();
        c.sync(0);
        let mut expected: u32 = 0;
        // 20ms 一帧，跑 10 分钟，跨越约 9 个回绕边界
        for _ in 0..(50 * 600) {
            expected = expected.wrapping_add(20);
            assert_eq!(c.extend(expected as u16), expected, "在 {expected} 处漂移");
        }
        assert!(expected > 5 * 0x1_0000, "没跑够回绕边界，测试无效");
    }

    #[test]
    fn 时钟_多次回绕() {
        let mut c = VoiceClock::default();
        c.sync(0);
        for i in 1..=3u32 {
            c.extend(0x8000);
            c.extend(0xffff);
            assert_eq!(c.extend(0x0000), i << 16);
        }
    }
}
