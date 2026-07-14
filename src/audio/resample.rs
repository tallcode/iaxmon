//! 8kHz → 设备采样率的线性插值重采样。
//!
//! macOS 默认输出设备基本只给 48000Hz，8k 上不去就没声音。线性插值对语音够用；
//! 如果听感明显发毛，再换 `rubato` 做带低通的正经重采样。

/// 跨块保持插值连续性：上一块的最后一个样本要参与下一块第一个样本的插值。
///
/// 位置用有理数表示（整数部分 + 以 out_rate 为分母的分数部分）而不是 f64 累加。
/// 浮点累加会漂：8k→48k 本该每块出 960 个样本，误差让位置停在 159.99999999999997，
/// 于是多挤出一个样本，长跑下来就是持续的时钟偏移。整数步进则是精确的。
pub struct Resampler {
    in_rate: u32,
    out_rate: u32,
    /// 当前位置的整数部分。0 表示落在 `prev` 上。
    pos_int: usize,
    /// 当前位置的分数部分，分母是 out_rate
    pos_num: u32,
    prev: f32,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        Self {
            in_rate,
            out_rate,
            pos_int: 0,
            pos_num: 0,
            prev: 0.0,
        }
    }

    /// 把一块 i16 输入重采样成 f32 追加到 `out`。
    pub fn process(&mut self, input: &[i16], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }

        // 虚拟输入序列 = [prev, input...]，长度 input.len()+1。
        // pos_int ∈ [0, len) 保证插值要用的 i 和 i+1 都在这个序列内。
        let sample = |i: usize| -> f32 {
            if i == 0 {
                self.prev
            } else {
                input[i - 1] as f32 / 32768.0
            }
        };

        let len = input.len();
        while self.pos_int < len {
            let frac = self.pos_num as f32 / self.out_rate as f32;
            out.push(sample(self.pos_int) * (1.0 - frac) + sample(self.pos_int + 1) * frac);

            self.pos_num += self.in_rate;
            while self.pos_num >= self.out_rate {
                self.pos_num -= self.out_rate;
                self.pos_int += 1;
            }
        }

        // 位置挪到下一块的坐标系；prev 换成本块最后一个样本
        self.pos_int -= len;
        self.prev = input[len - 1] as f32 / 32768.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 八千到四万八是六倍() {
        let mut r = Resampler::new(8000, 48000);
        let mut out = Vec::new();
        r.process(&[100; 160], &mut out);
        assert_eq!(out.len(), 960);
    }

    #[test]
    fn 同采样率时长度不变() {
        let mut r = Resampler::new(8000, 8000);
        let mut out = Vec::new();
        r.process(&[100; 160], &mut out);
        assert_eq!(out.len(), 160);
    }

    #[test]
    fn 跨块的总长度不漂移() {
        // 44100 不是 8000 的整数倍，最容易暴露累计误差
        let mut r = Resampler::new(8000, 44100);
        let mut out = Vec::new();
        for _ in 0..50 {
            r.process(&[0; 160], &mut out);
        }
        // 50 帧 × 20ms = 1 秒，整数步进下应该不多不少正好 44100 个样本
        assert_eq!(out.len(), 44100);
    }

    /// 长跑不应该有任何累计漂移：一小时的音频，样本数必须精确
    #[test]
    fn 一小时不漂移() {
        let mut r = Resampler::new(8000, 48000);
        let mut total = 0usize;
        let mut out = Vec::new();
        for _ in 0..(50 * 3600) {
            out.clear();
            r.process(&[0; 160], &mut out);
            total += out.len();
        }
        assert_eq!(total, 48000 * 3600);
    }

    #[test]
    fn 直流信号重采样后仍是直流() {
        let mut r = Resampler::new(8000, 48000);
        let mut out = Vec::new();
        r.process(&[16384; 160], &mut out); // 0.5 满幅
        r.process(&[16384; 160], &mut out);
        // 第一块开头要从 prev=0 爬升，跳过这段过渡再检查
        for &s in &out[10..] {
            assert!((s - 0.5).abs() < 1e-3, "样本 {s} 偏离直流值 0.5");
        }
    }

    #[test]
    fn 跨块边界连续_没有断点() {
        let mut r = Resampler::new(8000, 48000);
        let mut out = Vec::new();
        r.process(&[1000; 160], &mut out);
        let 块一末尾 = *out.last().unwrap();
        r.process(&[1000; 160], &mut out);
        let 块二开头 = out[960];
        assert!((块一末尾 - 块二开头).abs() < 1e-3, "块边界处有跳变");
    }

    #[test]
    fn 空输入不产出也不改状态() {
        let mut r = Resampler::new(8000, 48000);
        let mut out = Vec::new();
        r.process(&[], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn 输出幅度在有效范围内() {
        let mut r = Resampler::new(8000, 48000);
        let mut out = Vec::new();
        r.process(&[i16::MIN, i16::MAX, i16::MIN, i16::MAX], &mut out);
        assert!(out.iter().all(|s| (-1.0..=1.0).contains(s)));
    }
}
