pub mod activity;
pub mod jitter;
pub mod player;
pub mod resample;
pub mod ulaw;

use crate::config::ActivityCfg;
use crate::nats::NatsPublisher;
use activity::{Activity, ActivityDetector};
use anyhow::Result;
use jitter::JitterBuffer;
use player::Player;
use resample::Resampler;

/// IAX2 语音是 8kHz 单声道
pub const VOICE_SAMPLE_RATE: u32 = 8000;
/// 一帧 20ms = 160 个样本
pub const SAMPLES_PER_FRAME: usize = 160;
/// 抖动缓冲的固定延迟
const JITTER_DELAY_MS: u32 = 100;
/// 输出环形缓冲的目标水位。太浅会被设备抽干，太深会平白增加延迟。
const OUTPUT_TARGET_MS: usize = 60;

/// 把「收到的 ulaw 帧」变成「扬声器里的声音」这一整条链路。
///
/// 网络侧调 [`push_frame`](Self::push_frame) 丢帧进来，
/// 播放侧每 20ms 调一次 [`tick`](Self::tick) 往输出推一帧。
struct Playback {
    jitter: JitterBuffer,
    resampler: Resampler,
    player: Player,
    pcm: Vec<i16>,
    out: Vec<f32>,
    target: usize,
}

pub struct AudioSink {
    playback: Option<Playback>,
    detector: ActivityDetector,
    nats: Option<NatsPublisher>,
    last_timestamp: Option<u32>,
}

impl AudioSink {
    pub fn new(cfg: &ActivityCfg) -> Result<Self> {
        let player = Player::new()?;
        let resampler = Resampler::new(VOICE_SAMPLE_RATE, player.sample_rate());
        let target = player.sample_rate() as usize * OUTPUT_TARGET_MS / 1000;
        Ok(Self {
            playback: Some(Playback {
                jitter: JitterBuffer::new(JITTER_DELAY_MS),
                resampler,
                player,
                pcm: Vec::with_capacity(SAMPLES_PER_FRAME),
                out: Vec::with_capacity(SAMPLES_PER_FRAME * 8),
                target,
            }),
            detector: ActivityDetector::new(cfg.threshold, cfg.hang_ms),
            nats: None,
            last_timestamp: None,
        })
    }

    /// 创建纯网络输出。这里不构造 [`Player`]，所以即使机器没有声卡也能启动。
    pub fn new_nats(cfg: &ActivityCfg, nats: NatsPublisher) -> Self {
        Self {
            playback: None,
            detector: ActivityDetector::new(cfg.threshold, cfg.hang_ms),
            nats: Some(nats),
            last_timestamp: None,
        }
    }

    /// 收到一帧语音。返回上话状态的变化（无变化则 None）。
    ///
    /// 检测放在这里而不是 `tick()`：这里反映的是**服务端实际发了什么**，而 `tick()`
    /// 在抖动缓冲之后，网络丢帧造成的空洞会被误判成静音。
    pub fn push_frame(&mut self, timestamp: u32, ulaw_payload: Vec<u8>) -> Option<Activity> {
        let rms = activity::frame_rms(&ulaw_payload);
        let loud = self.detector.is_loud(rms);
        let ev = self.detector.push(rms);
        self.last_timestamp = Some(timestamp);

        if let Some(nats) = &self.nats {
            if ev == Some(Activity::Started) {
                nats.publish_start(timestamp);
            }
            // 静音帧一概不发到 NATS；短暂停顿由下游 Gateway 按时间戳补零。
            if loud {
                nats.publish_audio(timestamp, ulaw_payload);
            }
            if let Some(Activity::Ended { frames }) = ev {
                nats.publish_stop(timestamp, Activity::duration_secs(frames));
            }
        } else if let Some(playback) = &mut self.playback {
            playback.jitter.push(timestamp, ulaw_payload);
        }
        ev
    }

    /// 每次新呼叫开始前调。输出流保持不动，只清掉跟旧呼叫时间戳绑定的状态。
    /// 若断线时正在上话，返回一个收尾事件。
    pub fn reset(&mut self) -> Option<Activity> {
        if let Some(playback) = &mut self.playback {
            playback.jitter.reset();
        }
        let ev = self.detector.reset();
        if let (Some(nats), Some(Activity::Ended { frames })) = (&self.nats, ev) {
            nats.publish_stop(
                self.last_timestamp.unwrap_or(0),
                Activity::duration_secs(frames),
            );
        }
        self.last_timestamp = None;
        ev
    }

    pub fn peak_rms(&self) -> f32 {
        self.detector.peak_rms
    }

    /// 每 20ms 调一次，把输出缓冲补到目标水位。
    ///
    /// 补多少由设备已经消费掉多少决定，不是由这个函数被调用的频率决定 —— 定时器
    /// 早一点晚一点都会被水位自动吸收。抖动缓冲里没帧就补静音，保证输出不断档。
    pub fn tick(&mut self) {
        let Some(playback) = &mut self.playback else {
            return;
        };
        while playback.player.buffered() < playback.target {
            match playback.jitter.pop() {
                Some(payload) => ulaw::decode_into(&payload, &mut playback.pcm),
                None => {
                    playback.pcm.clear();
                    playback.pcm.resize(SAMPLES_PER_FRAME, 0);
                }
            }
            playback.out.clear();
            playback.resampler.process(&playback.pcm, &mut playback.out);
            playback.player.push(&playback.out);
        }
    }

    pub fn stats(&self) -> (jitter::Stats, u64, usize) {
        match &self.playback {
            Some(playback) => (
                playback.jitter.stats,
                playback.player.underruns(),
                playback.jitter.len(),
            ),
            None => (jitter::Stats::default(), 0, 0),
        }
    }
}
