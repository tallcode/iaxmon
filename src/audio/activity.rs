//! 上话活动检测。
//!
//! IAX2 电话连接拿不到任何带内的按键信号 —— app_rpt 的按键状态只从 AMI 出去
//! （PROTOCOL.md §9.4），所以只能靠音频能量自己判断。
//!
//! 实测这台服务端空闲时推的是**精确的数字静音**（全 0xFF，解码后全 0），而真实音频
//! 即使在词间停顿也是非零的，两者区分得很干净。阈值仍做成可配的，因为别的节点可能
//! 发舒适噪声而不是纯静音。

use super::ulaw;

/// 每帧 20ms
const FRAME_MS: u32 = 20;

/// 活动状态的变化。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// 有人开始上话
    Started,
    /// 上话结束。`frames` 是本次持续的帧数，不含末尾的静音（hang time）。
    Ended { frames: u32 },
}

impl Activity {
    /// 由帧数换算时长。用媒体时钟而非墙上时钟，因为帧数是精确的。
    pub fn duration_secs(frames: u32) -> f64 {
        frames as f64 * FRAME_MS as f64 / 1000.0
    }
}

/// 算一帧 ulaw 的 RMS。不分配内存。
pub fn frame_rms(ulaw_payload: &[u8]) -> f32 {
    if ulaw_payload.is_empty() {
        return 0.0;
    }
    let sum: f64 = ulaw_payload
        .iter()
        .map(|&b| {
            let s = ulaw::decode(b) as f64;
            s * s
        })
        .sum();
    (sum / ulaw_payload.len() as f64).sqrt() as f32
}

/// 能量门限 + 挂起时间。
///
/// 起始沿立刻触发（1 帧 = 20ms），结束沿要等够 hang time —— 语音里有停顿，
/// 结束判定必须迟钝一些，否则一句话会被切成好几段。
pub struct ActivityDetector {
    threshold: f32,
    /// 连续多少帧低于阈值才判定结束
    hang_frames: u32,
    active: bool,
    /// 当前连续静音帧数
    silent_run: u32,
    /// 本次活动从起始沿至今的总帧数
    frames: u32,
    /// 最后一个超阈值的帧在本次活动中的位置。时长以它为准 —— 末尾那段用于判定
    /// 结束的静音属于检测延迟，不是发射时长。
    frames_at_last_loud: u32,
    /// 观测到的最大 RMS，用于调阈值
    pub peak_rms: f32,
}

impl ActivityDetector {
    pub fn new(threshold: f32, hang_ms: u32) -> Self {
        Self {
            threshold,
            hang_frames: (hang_ms / FRAME_MS).max(1),
            active: false,
            silent_run: 0,
            frames: 0,
            frames_at_last_loud: 0,
            peak_rms: 0.0,
        }
    }

    /// 喂一帧，返回状态变化（没变化则 None）。
    pub fn push(&mut self, rms: f32) -> Option<Activity> {
        self.peak_rms = self.peak_rms.max(rms);
        let loud = rms > self.threshold;

        if !self.active {
            if loud {
                self.active = true;
                self.frames = 1;
                self.frames_at_last_loud = 1;
                self.silent_run = 0;
                return Some(Activity::Started);
            }
            return None;
        }

        self.frames += 1;
        if loud {
            self.silent_run = 0;
            self.frames_at_last_loud = self.frames;
            return None;
        }

        // 活动中的静音：够了 hang time 才收尾
        self.silent_run += 1;
        if self.silent_run >= self.hang_frames {
            self.active = false;
            let frames = self.frames_at_last_loud;
            self.frames = 0;
            self.frames_at_last_loud = 0;
            self.silent_run = 0;
            return Some(Activity::Ended { frames });
        }
        None
    }

    /// 呼叫结束/重连时调。若正处于活动中，返回收尾事件。
    pub fn reset(&mut self) -> Option<Activity> {
        let ev = self.active.then_some(Activity::Ended {
            frames: self.frames_at_last_loud,
        });
        self.active = false;
        self.silent_run = 0;
        self.frames = 0;
        self.frames_at_last_loud = 0;
        ev
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 检测器() -> ActivityDetector {
        ActivityDetector::new(50.0, 100) // 阈值 50，hang 5 帧
    }

    #[test]
    fn 静音不产生事件() {
        let mut d = 检测器();
        for _ in 0..100 {
            assert_eq!(d.push(0.0), None);
        }
        assert_eq!(d.reset(), None, "从未活动过，重置不该补收尾事件");
    }

    /// 起始沿必须立刻触发，不能等 —— 迟一帧就是迟 20ms
    #[test]
    fn 第一帧超阈值就触发开始() {
        let mut d = 检测器();
        assert_eq!(d.push(1000.0), Some(Activity::Started));
        // 后续响帧不再重复触发
        assert_eq!(d.push(1000.0), None);
    }

    #[test]
    fn 静音够_hang_时间才判定结束() {
        let mut d = 检测器();
        d.push(1000.0);
        // hang 是 5 帧，前 4 帧静音不结束
        for i in 0..4 {
            assert_eq!(d.push(0.0), None, "第 {i} 帧静音不该结束");
        }
        // 第 5 帧静音触发结束。只有 1 帧是有声的，时长就该是 1 帧
        assert_eq!(d.push(0.0), Some(Activity::Ended { frames: 1 }));
    }

    /// 语音里的短停顿不能把一句话切成两段
    #[test]
    fn 短停顿不打断() {
        let mut d = 检测器();
        assert_eq!(d.push(1000.0), Some(Activity::Started));
        for _ in 0..3 {
            assert_eq!(d.push(0.0), None); // 短停顿
        }
        assert_eq!(d.push(1000.0), None, "停顿后恢复不该重新触发 Started");
    }

    /// 停顿后恢复，计时器要重置，不能累计到上一次的静音里
    #[test]
    fn 停顿后恢复会重置静音计数() {
        let mut d = 检测器();
        d.push(1000.0);
        for _ in 0..4 {
            d.push(0.0);
        }
        d.push(1000.0); // 恢复
        // 又要重新数满 5 帧才结束
        for _ in 0..4 {
            assert_eq!(d.push(0.0), None);
        }
        assert!(matches!(d.push(0.0), Some(Activity::Ended { .. })));
    }

    /// 末尾用于判定结束的静音是检测延迟，不该算进发射时长
    #[test]
    fn 时长算到最后一个有声帧为止() {
        let mut d = 检测器();
        d.push(1000.0);
        d.push(1000.0);
        d.push(1000.0); // 3 帧有声
        for _ in 0..5 {
            d.push(0.0); // 5 帧静音触发结束
        }
        assert_eq!(d.reset(), None, "上一步已经结束了");
    }

    #[test]
    fn 时长不含末尾静音() {
        let mut d = 检测器();
        d.push(1000.0);
        d.push(1000.0);
        d.push(1000.0); // 3 帧有声
        for _ in 0..4 {
            assert_eq!(d.push(0.0), None);
        }
        assert_eq!(
            d.push(0.0),
            Some(Activity::Ended { frames: 3 }),
            "时长应为 3 帧，不是 8 帧"
        );
    }

    /// 词间停顿要算进时长（同一次上话），末尾静音不算
    #[test]
    fn 中间的停顿算进时长() {
        let mut d = 检测器();
        d.push(1000.0); // 第 1 帧有声
        d.push(0.0); // 第 2 帧：词间停顿
        d.push(0.0); // 第 3 帧
        d.push(1000.0); // 第 4 帧有声
        for _ in 0..4 {
            d.push(0.0);
        }
        assert_eq!(
            d.push(0.0),
            Some(Activity::Ended { frames: 4 }),
            "应算到第 4 帧"
        );
    }

    #[test]
    fn 恰好在阈值上不算响() {
        let mut d = 检测器();
        assert_eq!(d.push(50.0), None, "等于阈值不该触发");
        assert_eq!(d.push(50.1), Some(Activity::Started));
    }

    #[test]
    fn 重置时正在上话会补一个结束事件() {
        let mut d = 检测器();
        d.push(1000.0);
        d.push(1000.0);
        assert_eq!(d.reset(), Some(Activity::Ended { frames: 2 }));
        // 已经空闲时重置不产生事件
        assert_eq!(d.reset(), None);
    }

    #[test]
    fn 峰值被记录下来用于调阈值() {
        let mut d = 检测器();
        d.push(100.0);
        d.push(9999.0);
        d.push(200.0);
        assert_eq!(d.peak_rms, 9999.0);
    }

    #[test]
    fn 时长换算() {
        assert_eq!(Activity::duration_secs(50), 1.0); // 50 帧 × 20ms
        assert_eq!(Activity::duration_secs(0), 0.0);
    }

    // --- RMS 计算 ---

    /// 实测：服务端空闲时推的是全 0xFF（解码后全 0），RMS 必须精确为 0
    #[test]
    fn rms_数字静音是零() {
        assert_eq!(frame_rms(&[0xff; 160]), 0.0);
    }

    #[test]
    fn rms_空负载是零() {
        assert_eq!(frame_rms(&[]), 0.0);
    }

    /// 满幅方波的 RMS 应该等于幅度
    #[test]
    fn rms_满幅() {
        let payload: Vec<u8> = (0..160)
            .map(|i| if i % 2 == 0 { 0x00 } else { 0x80 })
            .collect();
        let rms = frame_rms(&payload);
        assert!(
            (rms - 32124.0).abs() < 1.0,
            "满幅方波 RMS = {rms}，应约 32124"
        );
    }

    /// 正负半幅交替，RMS 应约为幅度本身
    #[test]
    fn rms_与幅度成正比() {
        let 小 = frame_rms(&[0xff; 160]);
        let 中: Vec<u8> = (0..160)
            .map(|i| if i % 2 == 0 { 0x30 } else { 0xb0 })
            .collect();
        let 大: Vec<u8> = (0..160)
            .map(|i| if i % 2 == 0 { 0x00 } else { 0x80 })
            .collect();
        assert!(小 < frame_rms(&中));
        assert!(frame_rms(&中) < frame_rms(&大));
    }
}
