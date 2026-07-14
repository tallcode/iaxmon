//! 固定延迟抖动缓冲。
//!
//! 网络送来的语音帧到达间隔是不均匀的，而播放侧必须每 20ms 稳定要一帧。
//! 这里按时间戳排序缓存，预热到一定深度再开始出帧，用这段延迟吸收抖动。

use std::collections::BTreeMap;

/// 每帧的时长，ulaw 20ms
pub const FRAME_MS: u32 = 20;

/// 缓冲硬上限，超过就丢最旧的。防止对端猛灌导致无限增长。
const MAX_FRAMES: usize = 100; // 2 秒

/// 允许的缓冲深度相对预热深度的倍数，超过就丢帧追上。
///
/// 服务端按它的时钟发帧，声卡按自己的晶振消费，两个时钟长期必然对不齐：偏一边
/// 是缓冲持续堆积、延迟越涨越大，偏另一边是持续欠载。丢一帧只是 20ms，语音里
/// 几乎听不出来，比让延迟无限增长好得多。
const MAX_DEPTH_FACTOR: usize = 2;

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// 迟到帧：时间戳比已播放位置还早，只能丢
    pub late: u64,
    /// 欠载：该出帧时缓冲是空的
    pub underruns: u64,
    /// 溢出：缓冲满了被迫丢最旧的帧
    pub overflows: u64,
    /// 时钟漂移导致堆积，主动丢帧把延迟拉回目标
    pub skew_drops: u64,
}

pub struct JitterBuffer {
    frames: BTreeMap<u32, Vec<u8>>,
    /// 预热帧数，达到才开始出帧
    prefill: usize,
    /// 缓冲深度上限，超过就丢帧
    max_depth: usize,
    primed: bool,
    last_played: Option<u32>,
    pub stats: Stats,
}

impl JitterBuffer {
    pub fn new(delay_ms: u32) -> Self {
        let prefill = (delay_ms / FRAME_MS).max(1) as usize;
        Self {
            frames: BTreeMap::new(),
            prefill,
            max_depth: prefill * MAX_DEPTH_FACTOR,
            primed: false,
            last_played: None,
            stats: Stats::default(),
        }
    }

    pub fn push(&mut self, timestamp: u32, payload: Vec<u8>) {
        // 迟到帧丢掉。乱序但没迟到的帧靠 BTreeMap 自动排回正确位置。
        if let Some(last) = self.last_played
            && timestamp <= last
        {
            self.stats.late += 1;
            return;
        }
        if self.frames.len() >= MAX_FRAMES {
            self.frames.pop_first();
            self.stats.overflows += 1;
        }
        self.frames.insert(timestamp, payload);
    }

    /// 取下一帧。返回 None 表示该填静音（预热中或欠载）。
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        if !self.primed {
            if self.frames.len() < self.prefill {
                return None;
            }
            self.primed = true;
        }
        // 堆积说明我们比对端慢，丢掉多余的把延迟拉回目标水位
        while self.frames.len() > self.max_depth {
            self.frames.pop_first();
            self.stats.skew_drops += 1;
        }
        match self.frames.pop_first() {
            Some((ts, payload)) => {
                self.last_played = Some(ts);
                Some(payload)
            }
            None => {
                // 缓冲被抽干，重新预热，避免在空缓冲上持续单帧抖动
                self.primed = false;
                self.stats.underruns += 1;
                None
            }
        }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// 换新呼叫时必须调。新呼叫的时间戳从 0 重新起算，不清掉旧的播放位置的话，
    /// 新帧会全部被当成迟到帧丢掉，结果就是重连后再也没声音。
    pub fn reset(&mut self) {
        self.frames.clear();
        self.primed = false;
        self.last_played = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 帧(n: u8) -> Vec<u8> {
        vec![n; 160]
    }

    #[test]
    fn 预热之前不出帧() {
        let mut jb = JitterBuffer::new(100); // 预热 5 帧
        for i in 0..4 {
            jb.push(i * FRAME_MS, 帧(i as u8));
        }
        assert!(jb.pop().is_none());

        jb.push(4 * FRAME_MS, 帧(4));
        assert_eq!(jb.pop(), Some(帧(0)));
    }

    #[test]
    fn 按时间戳顺序出帧() {
        let mut jb = JitterBuffer::new(40); // 预热 2 帧
        jb.push(0, 帧(0));
        jb.push(FRAME_MS, 帧(1));
        jb.push(2 * FRAME_MS, 帧(2));

        assert_eq!(jb.pop(), Some(帧(0)));
        assert_eq!(jb.pop(), Some(帧(1)));
        assert_eq!(jb.pop(), Some(帧(2)));
    }

    #[test]
    fn 乱序到达的帧被排回正确顺序() {
        let mut jb = JitterBuffer::new(60); // 预热 3 帧
        jb.push(2 * FRAME_MS, 帧(2));
        jb.push(0, 帧(0));
        jb.push(FRAME_MS, 帧(1));

        assert_eq!(jb.pop(), Some(帧(0)));
        assert_eq!(jb.pop(), Some(帧(1)));
        assert_eq!(jb.pop(), Some(帧(2)));
    }

    #[test]
    fn 迟到的帧被丢弃而不是插队() {
        let mut jb = JitterBuffer::new(20); // 预热 1 帧
        jb.push(5 * FRAME_MS, 帧(5));
        assert_eq!(jb.pop(), Some(帧(5)));

        jb.push(3 * FRAME_MS, 帧(3)); // 比已播放的还早
        assert_eq!(jb.stats.late, 1);
        assert_eq!(jb.len(), 0);
    }

    #[test]
    fn 欠载后重新预热() {
        let mut jb = JitterBuffer::new(40); // 预热 2 帧
        jb.push(0, 帧(0));
        jb.push(FRAME_MS, 帧(1));
        assert_eq!(jb.pop(), Some(帧(0)));
        assert_eq!(jb.pop(), Some(帧(1)));

        assert!(jb.pop().is_none()); // 抽干
        assert_eq!(jb.stats.underruns, 1);

        // 只来一帧还不够，要重新攒够预热深度
        jb.push(2 * FRAME_MS, 帧(2));
        assert!(jb.pop().is_none());
        jb.push(3 * FRAME_MS, 帧(3));
        assert_eq!(jb.pop(), Some(帧(2)));
    }

    /// 时钟漂移把缓冲顶高之后，延迟不能一直涨，要靠丢帧收敛回目标
    #[test]
    fn 堆积时丢帧把延迟拉回目标() {
        let mut jb = JitterBuffer::new(100); // 预热 5 帧，上限 10 帧
        for i in 0..20 {
            jb.push(i * FRAME_MS, 帧(i as u8));
        }
        assert_eq!(jb.len(), 20);

        // 出一帧就该把深度压回上限
        let 出帧 = jb.pop().unwrap();
        assert!(jb.len() <= 10, "出帧后深度仍是 {}", jb.len());
        assert_eq!(jb.stats.skew_drops, 10);
        // 丢的是最旧的，出的是丢完之后最旧的那帧
        assert_eq!(出帧, 帧(10));
    }

    #[test]
    fn 深度在目标以内时不丢帧() {
        let mut jb = JitterBuffer::new(100); // 预热 5 帧，上限 10 帧
        for i in 0..10 {
            jb.push(i * FRAME_MS, 帧(i as u8));
        }
        assert_eq!(jb.pop(), Some(帧(0)));
        assert_eq!(jb.stats.skew_drops, 0);
    }

    #[test]
    fn 缓冲溢出时丢最旧的() {
        let mut jb = JitterBuffer::new(20);
        for i in 0..(MAX_FRAMES + 10) {
            jb.push(i as u32 * FRAME_MS, 帧(0));
        }
        assert_eq!(jb.len(), MAX_FRAMES);
        assert_eq!(jb.stats.overflows, 10);
    }

    #[test]
    fn 重置后能接受时间戳归零的新呼叫() {
        let mut jb = JitterBuffer::new(20);
        jb.push(50_000, 帧(9));
        assert_eq!(jb.pop(), Some(帧(9)));

        jb.reset();

        // 新呼叫的时间戳从 0 起算，远小于刚播过的 50000
        jb.push(0, 帧(0));
        assert_eq!(jb.pop(), Some(帧(0)), "重置后新呼叫的帧被误判为迟到");
        assert_eq!(jb.stats.late, 0);
    }

    #[test]
    fn 时间戳不连续也能出帧() {
        // 对端跳了一大段时间戳（比如中间静音没发帧）
        let mut jb = JitterBuffer::new(20);
        jb.push(0, 帧(0));
        assert_eq!(jb.pop(), Some(帧(0)));
        jb.push(100_000, 帧(1));
        assert_eq!(jb.pop(), Some(帧(1)));
    }
}
