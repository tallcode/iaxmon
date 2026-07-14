//! G.711 μ-law 解码。

const BIAS: i32 = 0x84;
const SIGN_BIT: u8 = 0x80;
const QUANT_MASK: u8 = 0x0f;
const SEG_MASK: u8 = 0x70;
const SEG_SHIFT: u8 = 4;

/// 编译期生成 256 项查找表，运行时只剩一次索引。
static TABLE: [i16; 256] = build_table();

const fn build_table() -> [i16; 256] {
    let mut table = [0i16; 256];
    let mut i = 0usize;
    while i < 256 {
        table[i] = decode_one(i as u8);
        i += 1;
    }
    table
}

const fn decode_one(byte: u8) -> i16 {
    let u = !byte;
    let t = (((u & QUANT_MASK) as i32) << 3) + BIAS;
    let t = t << ((u & SEG_MASK) >> SEG_SHIFT);
    if u & SIGN_BIT != 0 { (BIAS - t) as i16 } else { (t - BIAS) as i16 }
}

#[inline]
pub fn decode(byte: u8) -> i16 {
    TABLE[byte as usize]
}

pub fn decode_into(payload: &[u8], out: &mut Vec<i16>) {
    out.clear();
    out.extend(payload.iter().map(|&b| decode(b)));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G.711 标准里的已知取值点
    #[test]
    fn 标准取值点() {
        assert_eq!(decode(0xff), 0); // +0
        assert_eq!(decode(0x7f), 0); // -0
        assert_eq!(decode(0x80), 32124); // 正满幅
        assert_eq!(decode(0x00), -32124); // 负满幅
    }

    #[test]
    fn 高位为零的字节是负值() {
        for b in 0x00..=0x7fu8 {
            assert!(decode(b) <= 0, "0x{b:02x} 应为负");
        }
    }

    #[test]
    fn 高位为一的字节是正值() {
        for b in 0x80..=0xffu8 {
            assert!(decode(b) >= 0, "0x{b:02x} 应为正");
        }
    }

    /// μ-law 是单调的：字节递减，幅度递增（各自在正负半区内）
    #[test]
    fn 单调性() {
        for b in 0x80..0xffu8 {
            assert!(decode(b) >= decode(b + 1), "正半区在 0x{b:02x} 处不单调");
        }
        for b in 0x00..0x7fu8 {
            assert!(decode(b) <= decode(b + 1), "负半区在 0x{b:02x} 处不单调");
        }
    }

    #[test]
    fn 正负对称() {
        for b in 0x00..=0x7fu8 {
            assert_eq!(decode(b), -decode(b | 0x80), "0x{b:02x} 不对称");
        }
    }

    #[test]
    fn 整帧解码() {
        let mut out = Vec::new();
        decode_into(&[0xff; 160], &mut out);
        assert_eq!(out.len(), 160);
        assert!(out.iter().all(|&s| s == 0));
    }
}
