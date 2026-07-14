//! 协议一致性测试。
//!
//! 这里的每个断言都对应一个**线上可观测的事实** —— 常量的数值、字节的排布、
//! 语义的边界。它们不测「代码是不是按我写的那样跑」，而测「跑出来的东西对端认不认」。
//!
//! 因此：**这些测试失败 = 协议被改坏了**，不是测试过时了。除非有 RFC / Asterisk
//! 源码 / 抓包能证明原值是错的，否则不要为了让测试通过而改这里的期望值。
//!
//! 依据见 PROTOCOL.md，每节都标了对应章节。

use super::consts::*;
use super::frame::{MiniFrame, compress_subclass, uncompress_subclass};
use super::{Frame, FullFrame, Ie, Ies};

// ===========================================================================
// 常量金标表 —— PROTOCOL.md §3、§4
//
// 这些数值由 RFC 5456 和 Asterisk 的实现定义，不是我们能选的。改动其中任何一个
// 都会让我们和真实服务端说不上话。
// ===========================================================================

#[test]
fn 金标_帧类型() {
    assert_eq!(frame_type::DTMF, 0x01);
    assert_eq!(frame_type::VOICE, 0x02);
    assert_eq!(frame_type::CONTROL, 0x04);
    assert_eq!(frame_type::NULL, 0x05);
    assert_eq!(frame_type::IAX, 0x06);
}

#[test]
fn 金标_iax_子类() {
    assert_eq!(iax::NEW, 0x01);
    assert_eq!(iax::PING, 0x02);
    assert_eq!(iax::PONG, 0x03);
    assert_eq!(iax::ACK, 0x04);
    assert_eq!(iax::HANGUP, 0x05);
    assert_eq!(iax::REJECT, 0x06);
    assert_eq!(iax::ACCEPT, 0x07);
    assert_eq!(iax::AUTHREQ, 0x08);
    assert_eq!(iax::AUTHREP, 0x09);
    assert_eq!(iax::INVAL, 0x0a);
    assert_eq!(iax::LAGRQ, 0x0b);
    assert_eq!(iax::LAGRP, 0x0c);
    assert_eq!(iax::VNAK, 0x12);
    assert_eq!(iax::TXCNT, 0x17);
    assert_eq!(iax::TXACC, 0x18);
    assert_eq!(iax::POKE, 0x1e);
    // Asterisk 扩展，未在 RFC 注册
    assert_eq!(iax::CALLTOKEN, 0x28);
}

#[test]
fn 金标_control_子类() {
    assert_eq!(control::HANGUP, 0x01);
    assert_eq!(control::RINGING, 0x03);
    assert_eq!(control::ANSWER, 0x04);
    assert_eq!(control::BUSY, 0x05);
    assert_eq!(control::CONGESTION, 0x08);
}

#[test]
fn 金标_ie_类型() {
    assert_eq!(ie::CALLED_NUMBER, 0x01);
    assert_eq!(ie::CALLING_NUMBER, 0x02);
    assert_eq!(ie::CALLING_NAME, 0x04);
    assert_eq!(ie::CALLED_CONTEXT, 0x05);
    assert_eq!(ie::USERNAME, 0x06);
    assert_eq!(ie::PASSWORD, 0x07);
    assert_eq!(ie::CAPABILITY, 0x08);
    assert_eq!(ie::FORMAT, 0x09);
    assert_eq!(ie::VERSION, 0x0b);
    assert_eq!(ie::AUTHMETHODS, 0x0e);
    assert_eq!(ie::CHALLENGE, 0x0f);
    assert_eq!(ie::MD5_RESULT, 0x10);
    assert_eq!(ie::APPARENT_ADDR, 0x12);
    assert_eq!(ie::REFRESH, 0x13);
    assert_eq!(ie::CAUSE, 0x16);
    assert_eq!(ie::CALLTOKEN, 0x36);
}

/// CAUSECODE 是 0x2a。0x2f 是 RR_LOSS(u32)，两者都在错误处理路径上，极易混淆。
#[test]
fn 金标_causecode_不是_rr_loss() {
    assert_eq!(ie::CAUSE_CODE, 0x2a, "CAUSECODE 必须是 0x2a；0x2f 是 RR_LOSS");
    assert_ne!(ie::CAUSE_CODE, 0x2f);
}

/// AUTHMETHODS 的位定义：PLAINTEXT=(1<<0), MD5=(1<<1), RSA=(1<<2)。
/// 整体偏一位会把服务端广播的 MD5 误读成明文，导致认证被我们自己拒绝。
#[test]
fn 金标_authmethods() {
    assert_eq!(auth_method::PLAINTEXT, 0x0001);
    assert_eq!(auth_method::MD5, 0x0002);
    assert_eq!(auth_method::RSA, 0x0004);
}

#[test]
fn 金标_媒体格式() {
    assert_eq!(format::ULAW, 0x0000_0004);
    assert_eq!(format::ALAW, 0x0000_0008);
}

#[test]
fn 金标_协议版本() {
    assert_eq!(IAX_PROTO_VERSION, 2);
}

// ===========================================================================
// 序列号语义 —— PROTOCOL.md §8
// ===========================================================================

/// 这五个子类不占序列号。收发两侧都适用：发不递增自己的 OSeqno，收不消耗 ISeqno。
/// 少一个会导致序列号错位；多一个会让我们把真帧当重复帧丢弃。
#[test]
fn 金标_不占序列号的子类集合() {
    for sc in [iax::ACK, iax::INVAL, iax::VNAK, iax::TXCNT, iax::TXACC] {
        assert!(iax::no_seq_increment(sc), "子类 0x{sc:02x} 应该不占序列号");
    }
}

#[test]
fn 金标_其余子类都占序列号() {
    for sc in [
        iax::NEW,
        iax::PING,
        iax::PONG,
        iax::HANGUP,
        iax::REJECT,
        iax::ACCEPT,
        iax::AUTHREQ,
        iax::AUTHREP,
        iax::LAGRQ,
        iax::LAGRP,
        iax::POKE,
        iax::CALLTOKEN,
    ] {
        assert!(!iax::no_seq_increment(sc), "子类 0x{sc:02x} 应该占序列号");
    }
}

// ===========================================================================
// 子类压缩 —— PROTOCOL.md §2
// ===========================================================================

/// 所有**合法子类**都必须能压缩，并原样解回来。
///
/// 注意方向：往返只在 subclass → csub → subclass 上成立。反方向不成立，见下一个测试。
#[test]
fn 子类压缩_合法子类往返() {
    let mut cases: Vec<u32> = (0..0x80).collect(); // C=0 区间
    cases.extend((7..=31).map(|p| 1u32 << p)); // C=1 区间：0x80 起的 2 的幂
    cases.push(super::frame::SUBCLASS_MINUS_ONE); // -1 特例

    for subclass in cases {
        let csub = compress_subclass(subclass)
            .unwrap_or_else(|e| panic!("合法子类 0x{subclass:x} 压不了: {e}"));
        assert_eq!(uncompress_subclass(csub), subclass, "子类 0x{subclass:x} 往返不一致");
    }
}

/// csub → subclass **不是单射**，所以反方向的往返不成立。
///
/// `0x01`（C=0，值 1）和 `0x80`（C=1，1<<0 = 1）解出来都是 1，但压缩只会产出前者
/// —— 因为 1 < 0x80 走 C=0 路径。解析器必须接受两者，编码器只该产出规范形式。
#[test]
fn 子类压缩_解压不是单射() {
    assert_eq!(uncompress_subclass(0x01), 1);
    assert_eq!(uncompress_subclass(0x80), 1);
    // 规范形式是 C=0 的那个
    assert_eq!(compress_subclass(1).unwrap(), 0x01);
}

/// 解压对任意 csub 都不能 panic（移位量必须先和 0x1f 相与）。
#[test]
fn 子类压缩_任意_csub_都能解压() {
    for csub in 0u8..=0xff {
        let _ = uncompress_subclass(csub);
    }
}

/// C=1 区间必须是 2 的幂才能压缩，否则无法编码。
#[test]
fn 子类压缩_非二次幂的大子类不可编码() {
    for subclass in [0x81u32, 0x1234, 0xffff, 0x8000_0001] {
        assert!(compress_subclass(subclass).is_err(), "0x{subclass:x} 不该能压缩");
    }
}

/// C=0 的区间（0x00..=0x7f）必须原样透传，不做任何变换。
#[test]
fn 子类压缩_小于_0x80_原样透传() {
    for v in 0u8..0x80 {
        assert_eq!(uncompress_subclass(v), v as u32);
        assert_eq!(compress_subclass(v as u32).unwrap(), v);
    }
}

/// ulaw = 0x04 < 0x80，所以语音帧的子类字节就是 0x04（C=0），不是 0x82（C=1, log2=2）。
#[test]
fn 子类压缩_ulaw_不压缩() {
    assert_eq!(compress_subclass(format::ULAW).unwrap(), 0x04);
    assert_eq!(uncompress_subclass(0x04), format::ULAW);
}

// ===========================================================================
// 线格式金标 —— PROTOCOL.md §1
// ===========================================================================

/// Full frame 头的 12 个字节，逐位对齐 RFC 5456 的图。
#[test]
fn 线格式_full_frame_头() {
    let f = FullFrame {
        source_call: 0x1234,
        dest_call: 0x5678,
        retransmit: false,
        timestamp: 0xdead_beef,
        oseqno: 0x03,
        iseqno: 0x05,
        frame_type: frame_type::IAX,
        subclass: iax::ACK,
        payload: vec![],
    };
    assert_eq!(
        f.encode().unwrap(),
        vec![
            0x92, 0x34, // F=1 | source 0x1234
            0x56, 0x78, // R=0 | dest 0x5678
            0xde, 0xad, 0xbe, 0xef, // timestamp 大端
            0x03, // oseqno
            0x05, // iseqno
            0x06, // frame type = IAX
            0x04, // C=0 | subclass = ACK
        ]
    );
}

/// R 位在 dest call number 字段的最高位，且不能污染呼叫号本身。
#[test]
fn 线格式_重传标志位() {
    let f = FullFrame {
        source_call: 0x1234,
        dest_call: 0x5678,
        retransmit: true,
        timestamp: 0,
        oseqno: 0,
        iseqno: 0,
        frame_type: frame_type::IAX,
        subclass: iax::NEW,
        payload: vec![],
    };
    let bytes = f.encode().unwrap();
    assert_eq!(&bytes[2..4], &[0xd6, 0x78], "R 位应置于 dest 字段最高位");

    let Frame::Full(back) = Frame::parse(&bytes).unwrap() else { panic!() };
    assert!(back.retransmit);
    assert_eq!(back.dest_call, 0x5678, "R 位不能污染呼叫号");
}

/// Mini frame 头 4 字节，F=0。
#[test]
fn 线格式_mini_frame_头() {
    let f = MiniFrame { source_call: 0x1234, timestamp: 0xabcd, payload: vec![0x01, 0x02] };
    assert_eq!(f.encode().unwrap(), vec![0x12, 0x34, 0xab, 0xcd, 0x01, 0x02]);
}

/// F=0 且呼叫号为 0 是 meta 帧，必须先于 mini frame 判断，否则会误解析。
#[test]
fn 线格式_meta_帧优先于_mini_识别() {
    assert_eq!(Frame::parse(&[0x00, 0x00, 0x12, 0x34, 0xaa]).unwrap(), Frame::Meta);
    // 呼叫号非 0 才是 mini
    assert!(matches!(Frame::parse(&[0x00, 0x01, 0x12, 0x34]).unwrap(), Frame::Mini(_)));
}

/// IE 线格式 `type | len | data`，数值大端。
#[test]
fn 线格式_ie() {
    let mut ies = Ies::new();
    ies.push(Ie::u16(ie::VERSION, 2));
    ies.push(Ie::u32(ie::CAPABILITY, format::ULAW));
    ies.push(Ie::string(ie::CALLED_NUMBER, "1999"));

    let mut buf = Vec::new();
    ies.encode(&mut buf).unwrap();
    assert_eq!(
        buf,
        vec![
            0x0b, 0x02, 0x00, 0x02, // VERSION, len=2, u16 大端 = 2
            0x08, 0x04, 0x00, 0x00, 0x00, 0x04, // CAPABILITY, len=4, u32 大端 = ulaw
            0x01, 0x04, b'1', b'9', b'9', b'9', // CALLED NUMBER, len=4, "1999"
        ]
    );
}

// ===========================================================================
// CallToken —— PROTOCOL.md §6
// ===========================================================================

/// 握手第一步：长度为 0 的 CALLTOKEN IE，线上就是 `0x36 0x00`。
/// 这个「空」是有意义的信号（我支持 CallToken），不是「没有这个 IE」。
#[test]
fn calltoken_空_ie_的线格式() {
    let mut ies = Ies::new();
    ies.push(Ie { id: ie::CALLTOKEN, data: vec![] });
    let mut buf = Vec::new();
    ies.encode(&mut buf).unwrap();
    assert_eq!(buf, vec![0x36, 0x00]);

    // 解析回来必须是「存在且为空」，而不是「不存在」
    let parsed = Ies::parse(&buf).unwrap();
    let token = parsed.get(ie::CALLTOKEN).expect("空 CALLTOKEN IE 必须能被解析出来");
    assert!(token.data.is_empty());
}

/// 令牌是不透明字节，必须原样回送。含非 ASCII 也不能出错。
#[test]
fn calltoken_令牌原样往返() {
    let token: Vec<u8> = (0u8..=255).collect::<Vec<_>>()[..200].to_vec();
    let mut ies = Ies::new();
    ies.push(Ie { id: ie::CALLTOKEN, data: token.clone() });
    let mut buf = Vec::new();
    ies.encode(&mut buf).unwrap();

    let parsed = Ies::parse(&buf).unwrap();
    assert_eq!(parsed.get(ie::CALLTOKEN).unwrap().data, token);
}

/// Asterisk 的令牌格式是 "<unix时间>?<40字符sha1>"，51 字节。IE 长度字段是 u8，装得下。
#[test]
fn calltoken_真实长度装得下() {
    let token = b"1752499200?0123456789abcdef0123456789abcdef01234567";
    assert_eq!(token.len(), 51);
    let mut ies = Ies::new();
    ies.push(Ie { id: ie::CALLTOKEN, data: token.to_vec() });
    let mut buf = Vec::new();
    ies.encode(&mut buf).unwrap();
    assert_eq!(buf[1], 51);
    assert_eq!(Ies::parse(&buf).unwrap().get(ie::CALLTOKEN).unwrap().data, token);
}

// ===========================================================================
// 真实抓包向量
//
// 下面的字节序列是从真实 Asterisk 服务端抓到的原包。它们是这套解析器和现实
// 之间唯一的硬连接 —— 其余测试都是自说自话的往返。
// ===========================================================================

/// 服务端要求 CallToken 而客户端没带时，Asterisk 的 `send_apathetic_reply()`
/// 回的 REJECT。特征：**不含任何 IE**，且源呼叫号是硬编码的 1。
///
/// 认得出这个特征，就能把「CallToken 没实现」和「真的被拒绝」区分开。
#[test]
fn 实包_calltoken_缺失时的裸_reject() {
    let wire = [0x80, 0x01, 0x62, 0x4b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x06, 0x06];

    let Frame::Full(f) = Frame::parse(&wire).unwrap() else { panic!("应解析为 full frame") };
    assert_eq!(f.source_call, 1, "apathetic reply 的源呼叫号是硬编码的 1");
    assert_eq!(f.dest_call, 0x624b, "回给我们的呼叫号");
    assert!(!f.retransmit);
    assert_eq!(f.timestamp, 0);
    assert_eq!(f.oseqno, 0);
    assert_eq!(f.iseqno, 1);
    assert_eq!(f.frame_type, frame_type::IAX);
    assert_eq!(f.subclass, iax::REJECT);
    assert!(f.payload.is_empty(), "裸 REJECT 不含任何 IE —— 这正是 CallToken 问题的特征");
    assert_eq!(f.ies().unwrap().0.len(), 0);

    // 往返
    assert_eq!(f.encode().unwrap(), wire);
}

// ===========================================================================
// 解析器健壮性 —— 对端可能发来任何东西，不能 panic
// ===========================================================================

#[test]
fn 健壮性_截断的包只报错不_panic() {
    for len in 0..12 {
        let full = vec![0x92u8; len]; // F=1
        let _ = Frame::parse(&full);
        let mini = vec![0x12u8; len]; // F=0
        let _ = Frame::parse(&mini);
    }
}

#[test]
fn 健壮性_畸形_ie_只报错不_panic() {
    let cases: &[&[u8]] = &[
        &[0x06],                   // 只有 type
        &[0x06, 0x07, b'a'],       // len 超过实际数据
        &[0x06, 0xff],             // 声明 255 字节但没有数据
        &[0x0b, 0x01, 0x02],       // VERSION 长度不对
        &[0x36, 0x00, 0x36, 0x00], // 重复的空 IE
    ];
    for c in cases {
        let _ = Ies::parse(c);
    }
}

/// 不认识的 IE 必须按长度跳过并保留，不能报错 —— 服务端会发我们不关心的 IE。
#[test]
fn 健壮性_未知_ie_按长度跳过() {
    let buf = [
        0xfe, 0x02, 0xaa, 0xbb, // 未知 IE，2 字节
        0x06, 0x06, b'N', b'0', b'C', b'A', b'L', b'L', // USERNAME
    ];
    let ies = Ies::parse(&buf).unwrap();
    assert_eq!(ies.0.len(), 2);
    assert_eq!(ies.0[0].id, 0xfe);
    assert_eq!(ies.get(ie::USERNAME).unwrap().as_string(), "N0CALL");
}

/// 任意字节序列都不能让解析器 panic。
#[test]
fn 健壮性_任意字节不_panic() {
    let mut buf = Vec::new();
    for seed in 0u32..2000 {
        buf.clear();
        let n = (seed % 40) as usize;
        for i in 0..n {
            buf.push(((seed.wrapping_mul(2654435761).wrapping_add(i as u32)) >> 13) as u8);
        }
        if let Ok(Frame::Full(f)) = Frame::parse(&buf) {
            let _ = f.ies(); // 负载当 IE 解析同样不能 panic
        }
    }
}
