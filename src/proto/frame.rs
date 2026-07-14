//! Full / Mini frame 的编解码。

use super::consts::{FLAG_SC_LOG, MAX_SHIFT};
use super::ie::Ies;
use anyhow::{Result, bail};

pub const FULL_HEADER_LEN: usize = 12;
pub const MINI_HEADER_LEN: usize = 4;

/// 呼叫号是 15 位
pub const CALL_NUMBER_MASK: u16 = 0x7fff;
/// Full frame 标志位，在 source call number 字段的最高位
const FLAG_FULL: u16 = 0x8000;
/// 重传标志位，在 dest call number 字段的最高位
const FLAG_RETRANS: u16 = 0x8000;

/// ```text
///  0                   1                   2                   3
///  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |1|     Source Call Number      |R|   Destination Call Number   |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                            timestamp                          |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |   OSeqno      |    ISeqno     |  Frame Type   |C|  Subclass   |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullFrame {
    pub source_call: u16,
    pub dest_call: u16,
    pub retransmit: bool,
    pub timestamp: u32,
    pub oseqno: u8,
    pub iseqno: u8,
    pub frame_type: u8,
    pub subclass: u32,
    /// IAX/CONTROL 帧里是 IE 序列，VOICE 帧里是音频负载
    pub payload: Vec<u8>,
}

impl FullFrame {
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.source_call == 0 || self.source_call > CALL_NUMBER_MASK {
            bail!("source call number 不合法: {}", self.source_call);
        }
        if self.dest_call > CALL_NUMBER_MASK {
            bail!("dest call number 不合法: {}", self.dest_call);
        }

        let mut out = Vec::with_capacity(FULL_HEADER_LEN + self.payload.len());
        out.extend_from_slice(&(self.source_call | FLAG_FULL).to_be_bytes());
        let dest = if self.retransmit { self.dest_call | FLAG_RETRANS } else { self.dest_call };
        out.extend_from_slice(&dest.to_be_bytes());
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out.push(self.oseqno);
        out.push(self.iseqno);
        out.push(self.frame_type);
        out.push(compress_subclass(self.subclass)?);
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    /// 把负载当 IE 序列解析。VOICE 帧不要调用。
    pub fn ies(&self) -> Result<Ies> {
        Ies::parse(&self.payload)
    }
}

/// ```text
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |0|     Source Call Number      |          timestamp            |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// ```
/// 只用于语音。时间戳是完整时间戳的低 16 位，编码格式沿用最近一个 full voice frame。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiniFrame {
    pub source_call: u16,
    pub timestamp: u16,
    pub payload: Vec<u8>,
}

impl MiniFrame {
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.source_call == 0 || self.source_call > CALL_NUMBER_MASK {
            bail!("source call number 不合法: {}", self.source_call);
        }
        let mut out = Vec::with_capacity(MINI_HEADER_LEN + self.payload.len());
        out.extend_from_slice(&self.source_call.to_be_bytes());
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Full(FullFrame),
    Mini(MiniFrame),
    /// F=0 且 source call number=0，是 trunk/video 的 meta 帧。我们不支持，收到就丢。
    Meta,
}

impl Frame {
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < MINI_HEADER_LEN {
            bail!("包太短: {} 字节", buf.len());
        }

        let first = u16::from_be_bytes([buf[0], buf[1]]);
        let is_full = first & FLAG_FULL != 0;
        let source_call = first & CALL_NUMBER_MASK;

        if !is_full {
            if source_call == 0 {
                return Ok(Frame::Meta);
            }
            return Ok(Frame::Mini(MiniFrame {
                source_call,
                timestamp: u16::from_be_bytes([buf[2], buf[3]]),
                payload: buf[MINI_HEADER_LEN..].to_vec(),
            }));
        }

        if buf.len() < FULL_HEADER_LEN {
            bail!("full frame 头被截断: {} 字节", buf.len());
        }
        let second = u16::from_be_bytes([buf[2], buf[3]]);
        Ok(Frame::Full(FullFrame {
            source_call,
            dest_call: second & CALL_NUMBER_MASK,
            retransmit: second & FLAG_RETRANS != 0,
            timestamp: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            oseqno: buf[8],
            iseqno: buf[9],
            frame_type: buf[10],
            subclass: uncompress_subclass(buf[11]),
            payload: buf[FULL_HEADER_LEN..].to_vec(),
        }))
    }
}

/// Asterisk 把子类 -1 特例编码成 0xff。我们用 u32 表示子类，所以它落在 u32::MAX。
pub const SUBCLASS_MINUS_ONE: u32 = u32::MAX;
const CSUB_MINUS_ONE: u8 = 0xff;

/// 子类压缩：小于 0x80 的原样放；否则必须是 2 的幂，存 log2 并置 C 位。
pub(super) fn compress_subclass(subclass: u32) -> Result<u8> {
    if subclass == SUBCLASS_MINUS_ONE {
        return Ok(CSUB_MINUS_ONE);
    }
    if subclass < FLAG_SC_LOG as u32 {
        return Ok(subclass as u8);
    }
    if !subclass.is_power_of_two() {
        bail!("子类 0x{subclass:x} 不是 2 的幂，无法压缩");
    }
    let power = subclass.trailing_zeros() as u8;
    if power > MAX_SHIFT {
        bail!("子类 0x{subclass:x} 超出可压缩范围");
    }
    Ok(power | FLAG_SC_LOG)
}

/// 子类解压：C 位置位则是 2^value，否则原样。0xff 是 -1 的特例，不是 1<<31。
pub(super) fn uncompress_subclass(csub: u8) -> u32 {
    if csub == CSUB_MINUS_ONE {
        return SUBCLASS_MINUS_ONE;
    }
    if csub & FLAG_SC_LOG != 0 { 1u32 << (csub & MAX_SHIFT) } else { csub as u32 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::consts::{format, frame_type, iax, ie as ie_id};
    use crate::proto::ie::Ie;

    fn 样例_full() -> FullFrame {
        FullFrame {
            source_call: 0x1234,
            dest_call: 0x5678,
            retransmit: false,
            timestamp: 0xdeadbeef,
            oseqno: 3,
            iseqno: 5,
            frame_type: frame_type::IAX,
            subclass: iax::ACK,
            payload: vec![],
        }
    }

    #[test]
    fn full_frame_线格式逐字节() {
        let bytes = 样例_full().encode().unwrap();
        assert_eq!(
            bytes,
            vec![
                0x92, 0x34, // F=1 | source 0x1234
                0x56, 0x78, // R=0 | dest 0x5678
                0xde, 0xad, 0xbe, 0xef, // timestamp
                0x03, // oseqno
                0x05, // iseqno
                0x06, // frame type = IAX
                0x04, // C=0 | subclass = ACK
            ]
        );
    }

    #[test]
    fn full_frame_往返() {
        let f = 样例_full();
        let bytes = f.encode().unwrap();
        assert_eq!(Frame::parse(&bytes).unwrap(), Frame::Full(f));
    }

    #[test]
    fn 重传标志位() {
        let mut f = 样例_full();
        f.retransmit = true;
        let bytes = f.encode().unwrap();
        assert_eq!(&bytes[2..4], &[0xd6, 0x78]); // R 位置位，dest 不变
        let Frame::Full(back) = Frame::parse(&bytes).unwrap() else { panic!() };
        assert!(back.retransmit);
        assert_eq!(back.dest_call, 0x5678);
    }

    #[test]
    fn full_frame_带_ie_往返() {
        let mut ies = Ies::new();
        ies.push(Ie::u16(ie_id::VERSION, 2));
        ies.push(Ie::string(ie_id::USERNAME, "N0CALL"));
        let mut payload = Vec::new();
        ies.encode(&mut payload).unwrap();

        let mut f = 样例_full();
        f.subclass = iax::NEW;
        f.payload = payload;

        let bytes = f.encode().unwrap();
        let Frame::Full(back) = Frame::parse(&bytes).unwrap() else { panic!() };
        assert_eq!(back.ies().unwrap(), ies);
    }

    /// ulaw = 0x04，小于 0x80，所以走 C=0 原样传，不是 C=1 + log2。
    #[test]
    fn ulaw_语音帧子类不压缩() {
        let mut f = 样例_full();
        f.frame_type = frame_type::VOICE;
        f.subclass = format::ULAW;
        f.payload = vec![0xff; 160];

        let bytes = f.encode().unwrap();
        assert_eq!(bytes[11], 0x04); // C=0，不是 0x82
        assert_eq!(Frame::parse(&bytes).unwrap(), Frame::Full(f));
    }

    #[test]
    fn 大子类走压缩() {
        assert_eq!(compress_subclass(0x80).unwrap(), 0x87); // 2^7
        assert_eq!(uncompress_subclass(0x87), 0x80);
        assert_eq!(compress_subclass(0x0001_0000).unwrap(), 0x90); // 2^16
        assert_eq!(uncompress_subclass(0x90), 0x0001_0000);
    }

    #[test]
    fn 小子类不压缩() {
        for v in [0u32, 1, 4, 0x7f] {
            assert_eq!(compress_subclass(v).unwrap(), v as u8);
            assert_eq!(uncompress_subclass(v as u8), v);
        }
    }

    #[test]
    fn 非二次幂的大子类无法压缩() {
        assert!(compress_subclass(0x81).is_err());
        assert!(compress_subclass(0x1234).is_err());
    }

    /// 0xff 是 Asterisk 对 -1 的特例编码，不是 1<<31
    #[test]
    fn 子类负一的特例() {
        assert_eq!(uncompress_subclass(0xff), SUBCLASS_MINUS_ONE);
        assert_eq!(compress_subclass(SUBCLASS_MINUS_ONE).unwrap(), 0xff);
        // 0x9f 的低 5 位同样是 0x1f，但它不是特例，就是老老实实的 1<<31
        assert_eq!(uncompress_subclass(0x9f), 1 << 31);
    }

    #[test]
    fn mini_frame_线格式与往返() {
        let f = MiniFrame { source_call: 0x1234, timestamp: 0xabcd, payload: vec![0x01, 0x02] };
        let bytes = f.encode().unwrap();
        assert_eq!(bytes, vec![0x12, 0x34, 0xab, 0xcd, 0x01, 0x02]); // F=0
        assert_eq!(Frame::parse(&bytes).unwrap(), Frame::Mini(f));
    }

    #[test]
    fn 呼叫号为零的非_full_帧是_meta() {
        let bytes = [0x00, 0x00, 0x12, 0x34, 0xaa];
        assert_eq!(Frame::parse(&bytes).unwrap(), Frame::Meta);
    }

    #[test]
    fn 呼叫号必须非零且在_15_位内() {
        let mut f = 样例_full();
        f.source_call = 0;
        assert!(f.encode().is_err());
        f.source_call = 0x8000;
        assert!(f.encode().is_err());
    }

    #[test]
    fn 包太短时报错() {
        assert!(Frame::parse(&[]).is_err());
        assert!(Frame::parse(&[0x12, 0x34, 0x56]).is_err());
        // F=1 但不足 12 字节
        assert!(Frame::parse(&[0x92, 0x34, 0x56, 0x78, 0x00]).is_err());
    }

    #[test]
    fn 空负载的_mini_帧合法() {
        let bytes = [0x12, 0x34, 0xab, 0xcd];
        let Frame::Mini(f) = Frame::parse(&bytes).unwrap() else { panic!() };
        assert!(f.payload.is_empty());
    }
}
