pub mod activity;
pub mod jitter;
pub mod player;
pub mod resample;
pub mod ulaw;

use crate::config::ActivityCfg;
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
pub struct AudioSink {
    jitter: JitterBuffer,
    resampler: Resampler,
    player: Player,
    pcm: Vec<i16>,
    out: Vec<f32>,
    target: usize,
    detector: ActivityDetector,
}

impl AudioSink {
    pub fn new(cfg: &ActivityCfg) -> Result<Self> {
        let player = Player::new()?;
        let resampler = Resampler::new(VOICE_SAMPLE_RATE, player.sample_rate());
        let target = player.sample_rate() as usize * OUTPUT_TARGET_MS / 1000;
        Ok(Self {
            jitter: JitterBuffer::new(JITTER_DELAY_MS),
            resampler,
            player,
            pcm: Vec::with_capacity(SAMPLES_PER_FRAME),
            out: Vec::with_capacity(SAMPLES_PER_FRAME * 8),
            target,
            detector: ActivityDetector::new(cfg.threshold, cfg.hang_ms),
        })
    }

    /// 收到一帧语音。返回上话状态的变化（无变化则 None）。
    ///
    /// 检测放在这里而不是 `tick()`：这里反映的是**服务端实际发了什么**，而 `tick()`
    /// 在抖动缓冲之后，网络丢帧造成的空洞会被误判成静音。
    pub fn push_frame(&mut self, timestamp: u32, ulaw_payload: Vec<u8>) -> Option<Activity> {
        let ev = self.detector.push(activity::frame_rms(&ulaw_payload));
        self.jitter.push(timestamp, ulaw_payload);
        ev
    }

    /// 每次新呼叫开始前调。输出流保持不动，只清掉跟旧呼叫时间戳绑定的状态。
    /// 若断线时正在上话，返回一个收尾事件。
    pub fn reset(&mut self) -> Option<Activity> {
        self.jitter.reset();
        self.detector.reset()
    }

    pub fn peak_rms(&self) -> f32 {
        self.detector.peak_rms
    }

    /// 每 20ms 调一次，把输出缓冲补到目标水位。
    ///
    /// 补多少由设备已经消费掉多少决定，不是由这个函数被调用的频率决定 —— 定时器
    /// 早一点晚一点都会被水位自动吸收。抖动缓冲里没帧就补静音，保证输出不断档。
    pub fn tick(&mut self) {
        while self.player.buffered() < self.target {
            match self.jitter.pop() {
                Some(payload) => ulaw::decode_into(&payload, &mut self.pcm),
                None => {
                    self.pcm.clear();
                    self.pcm.resize(SAMPLES_PER_FRAME, 0);
                }
            }
            self.out.clear();
            self.resampler.process(&self.pcm, &mut self.out);
            self.player.push(&self.out);
        }
    }

    pub fn stats(&self) -> (jitter::Stats, u64, usize) {
        (
            self.jitter.stats,
            self.player.underruns(),
            self.jitter.len(),
        )
    }
}
