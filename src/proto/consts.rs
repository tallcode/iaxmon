//! IAX2 协议常量（RFC 5456）。
//!
//! 这里的表是**故意做全的** —— 它同时是协议参考（对应 PROTOCOL.md §3、§4），
//! 而不只是「本客户端恰好用到的那几个」。收方向要靠它认出对端发来的任何东西，
//! 诊断时要靠它把数值翻成名字。每个值都由 proto/conformance.rs 的金标测试钉死。
#![allow(dead_code)]

/// 帧类型
pub mod frame_type {
    pub const DTMF: u8 = 0x01;
    pub const VOICE: u8 = 0x02;
    pub const CONTROL: u8 = 0x04;
    pub const NULL: u8 = 0x05;
    pub const IAX: u8 = 0x06;
}

/// IAX 控制帧子类（frame_type = IAX）
pub mod iax {
    pub const NEW: u32 = 0x01;
    pub const PING: u32 = 0x02;
    pub const PONG: u32 = 0x03;
    pub const ACK: u32 = 0x04;
    pub const HANGUP: u32 = 0x05;
    pub const REJECT: u32 = 0x06;
    pub const ACCEPT: u32 = 0x07;
    pub const AUTHREQ: u32 = 0x08;
    pub const AUTHREP: u32 = 0x09;
    pub const INVAL: u32 = 0x0a;
    pub const LAGRQ: u32 = 0x0b;
    pub const LAGRP: u32 = 0x0c;
    pub const VNAK: u32 = 0x12;
    pub const TXCNT: u32 = 0x17;
    pub const TXACC: u32 = 0x18;
    pub const POKE: u32 = 0x1e;
    /// 服务端下发呼叫令牌，见 §4.2.1
    pub const CALLTOKEN: u32 = 0x28;

    /// 这些子类不递增 OSeqno（RFC 5456 §8.1）
    pub fn no_seq_increment(subclass: u32) -> bool {
        matches!(subclass, ACK | INVAL | VNAK | TXCNT | TXACC)
    }

    pub fn name(subclass: u32) -> &'static str {
        match subclass {
            NEW => "NEW",
            PING => "PING",
            PONG => "PONG",
            ACK => "ACK",
            HANGUP => "HANGUP",
            REJECT => "REJECT",
            ACCEPT => "ACCEPT",
            AUTHREQ => "AUTHREQ",
            AUTHREP => "AUTHREP",
            INVAL => "INVAL",
            LAGRQ => "LAGRQ",
            LAGRP => "LAGRP",
            VNAK => "VNAK",
            POKE => "POKE",
            CALLTOKEN => "CALLTOKEN",
            _ => "?",
        }
    }
}

/// CONTROL 帧子类（frame_type = CONTROL）
pub mod control {
    pub const HANGUP: u32 = 0x01;
    pub const RINGING: u32 = 0x03;
    pub const ANSWER: u32 = 0x04;
    pub const BUSY: u32 = 0x05;
    pub const CONGESTION: u32 = 0x08;

    pub fn name(subclass: u32) -> &'static str {
        match subclass {
            HANGUP => "HANGUP",
            RINGING => "RINGING",
            ANSWER => "ANSWER",
            BUSY => "BUSY",
            CONGESTION => "CONGESTION",
            _ => "?",
        }
    }
}

/// Information Element 类型
pub mod ie {
    pub const CALLED_NUMBER: u8 = 0x01;
    pub const CALLING_NUMBER: u8 = 0x02;
    pub const CALLING_NAME: u8 = 0x04;
    pub const CALLED_CONTEXT: u8 = 0x05;
    pub const USERNAME: u8 = 0x06;
    pub const PASSWORD: u8 = 0x07;
    pub const CAPABILITY: u8 = 0x08;
    pub const FORMAT: u8 = 0x09;
    pub const VERSION: u8 = 0x0b;
    pub const AUTHMETHODS: u8 = 0x0e;
    pub const CHALLENGE: u8 = 0x0f;
    pub const MD5_RESULT: u8 = 0x10;
    pub const APPARENT_ADDR: u8 = 0x12;
    pub const REFRESH: u8 = 0x13;
    pub const CAUSE: u8 = 0x16;
    /// Q.931 原因码，u8。
    ///
    /// 是 0x2a（十进制 42），不是 0x2f —— 0x2f 是 RR_LOSS（接收报告的丢包率，u32）。
    /// 两者都出现在错误处理路径上，容易混淆。
    pub const CAUSE_CODE: u8 = 0x2a;
    /// 呼叫令牌，防 IP 伪造的反射攻击
    pub const CALLTOKEN: u8 = 0x36;
}

/// 认证方式位掩码。
///
/// 取值以 Asterisk 的 `IAX_AUTH_*` 为准：PLAINTEXT=(1<<0), MD5=(1<<1), RSA=(1<<2)。
/// 实测佐证：服务端 iax.conf 写的是 `auth=md5`，AUTHREQ 里广播的 AUTHMETHODS 就是 0x0002。
pub mod auth_method {
    pub const PLAINTEXT: u16 = 0x0001;
    pub const MD5: u16 = 0x0002;
    pub const RSA: u16 = 0x0004;
}

/// 媒体格式位掩码
pub mod format {
    pub const ULAW: u32 = 0x0000_0004;
    pub const ALAW: u32 = 0x0000_0008;
}

/// IAX 协议版本，固定 2
pub const IAX_PROTO_VERSION: u16 = 2;

/// 子类压缩标志位（C bit）
pub const FLAG_SC_LOG: u8 = 0x80;
/// 压缩子类的最大移位量
pub const MAX_SHIFT: u8 = 0x1f;
