//! Information Element 编解码。
//!
//! 线格式: `type(1) | len(1) | data(len)`，一个 full frame 后面可以跟多个。

use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ie {
    pub id: u8,
    pub data: Vec<u8>,
}

impl Ie {
    pub fn string(id: u8, s: &str) -> Self {
        Self {
            id,
            data: s.as_bytes().to_vec(),
        }
    }

    pub fn u8(id: u8, v: u8) -> Self {
        Self { id, data: vec![v] }
    }

    pub fn u16(id: u8, v: u16) -> Self {
        Self {
            id,
            data: v.to_be_bytes().to_vec(),
        }
    }

    pub fn u32(id: u8, v: u32) -> Self {
        Self {
            id,
            data: v.to_be_bytes().to_vec(),
        }
    }

    pub fn as_string(&self) -> String {
        String::from_utf8_lossy(&self.data).to_string()
    }

    pub fn as_u8(&self) -> Result<u8> {
        match self.data.as_slice() {
            [v] => Ok(*v),
            _ => bail!(
                "IE 0x{:02x} 期望 1 字节，实际 {} 字节",
                self.id,
                self.data.len()
            ),
        }
    }

    pub fn as_u16(&self) -> Result<u16> {
        match self.data.as_slice() {
            [a, b] => Ok(u16::from_be_bytes([*a, *b])),
            _ => bail!(
                "IE 0x{:02x} 期望 2 字节，实际 {} 字节",
                self.id,
                self.data.len()
            ),
        }
    }

    pub fn as_u32(&self) -> Result<u32> {
        match self.data.as_slice() {
            [a, b, c, d] => Ok(u32::from_be_bytes([*a, *b, *c, *d])),
            _ => bail!(
                "IE 0x{:02x} 期望 4 字节，实际 {} 字节",
                self.id,
                self.data.len()
            ),
        }
    }
}

/// IE 列表，保持线上的原始顺序。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ies(pub Vec<Ie>);

impl Ies {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn push(&mut self, ie: Ie) -> &mut Self {
        self.0.push(ie);
        self
    }

    /// 取第一个匹配的 IE。重复的 IE 不合法，但按 RFC 的宽容原则不报错。
    pub fn get(&self, id: u8) -> Option<&Ie> {
        self.0.iter().find(|ie| ie.id == id)
    }

    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        for ie in &self.0 {
            if ie.data.len() > u8::MAX as usize {
                bail!("IE 0x{:02x} 数据过长: {} 字节", ie.id, ie.data.len());
            }
            out.push(ie.id);
            out.push(ie.data.len() as u8);
            out.extend_from_slice(&ie.data);
        }
        Ok(())
    }

    /// 解析 IE 序列。不认识的 IE 按长度跳过后保留，由调用方决定是否理会。
    pub fn parse(mut buf: &[u8]) -> Result<Self> {
        let mut ies = Vec::new();
        while !buf.is_empty() {
            if buf.len() < 2 {
                bail!("IE 头被截断: 剩余 {} 字节", buf.len());
            }
            let id = buf[0];
            let len = buf[1] as usize;
            if buf.len() < 2 + len {
                bail!(
                    "IE 0x{:02x} 数据被截断: 声明 {} 字节，实际剩余 {}",
                    id,
                    len,
                    buf.len() - 2
                );
            }
            ies.push(Ie {
                id,
                data: buf[2..2 + len].to_vec(),
            });
            buf = &buf[2 + len..];
        }
        Ok(Self(ies))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::consts::ie as ie_id;

    #[test]
    fn 空_ie_列表往返() {
        let ies = Ies::new();
        let mut buf = Vec::new();
        ies.encode(&mut buf).unwrap();
        assert!(buf.is_empty());
        assert_eq!(Ies::parse(&buf).unwrap(), ies);
    }

    #[test]
    fn 字符串_ie_线格式() {
        let mut ies = Ies::new();
        ies.push(Ie::string(ie_id::USERNAME, "N0CALL"));
        let mut buf = Vec::new();
        ies.encode(&mut buf).unwrap();
        assert_eq!(buf, b"\x06\x06N0CALL");
    }

    #[test]
    fn 数值_ie_大端序() {
        let mut ies = Ies::new();
        ies.push(Ie::u16(ie_id::VERSION, 2));
        ies.push(Ie::u32(ie_id::CAPABILITY, 0x0000_0004));
        let mut buf = Vec::new();
        ies.encode(&mut buf).unwrap();
        assert_eq!(buf, b"\x0b\x02\x00\x02\x08\x04\x00\x00\x00\x04");
    }

    #[test]
    fn 混合_ie_往返() {
        let mut ies = Ies::new();
        ies.push(Ie::u16(ie_id::VERSION, 2));
        ies.push(Ie::string(ie_id::USERNAME, "N0CALL"));
        ies.push(Ie::string(ie_id::CALLED_NUMBER, "1999"));
        ies.push(Ie::u32(ie_id::FORMAT, 0x0000_0004));

        let mut buf = Vec::new();
        ies.encode(&mut buf).unwrap();
        let parsed = Ies::parse(&buf).unwrap();

        assert_eq!(parsed, ies);
        assert_eq!(parsed.get(ie_id::VERSION).unwrap().as_u16().unwrap(), 2);
        assert_eq!(parsed.get(ie_id::USERNAME).unwrap().as_string(), "N0CALL");
        assert_eq!(
            parsed.get(ie_id::CALLED_NUMBER).unwrap().as_string(),
            "1999"
        );
        assert_eq!(parsed.get(ie_id::FORMAT).unwrap().as_u32().unwrap(), 4);
        assert!(parsed.get(ie_id::CHALLENGE).is_none());
    }

    #[test]
    fn 长度为零的_ie() {
        let ies = Ies::parse(b"\x0f\x00").unwrap();
        assert_eq!(ies.0.len(), 1);
        assert_eq!(ies.0[0].data, Vec::<u8>::new());
        assert_eq!(ies.0[0].as_string(), "");
    }

    #[test]
    fn 未知_ie_被保留而不是报错() {
        let ies = Ies::parse(b"\xfe\x02\xaa\xbb\x06\x03abc").unwrap();
        assert_eq!(ies.0.len(), 2);
        assert_eq!(ies.0[0].id, 0xfe);
        assert_eq!(ies.get(ie_id::USERNAME).unwrap().as_string(), "abc");
    }

    #[test]
    fn 数据被截断时报错() {
        assert!(Ies::parse(b"\x06\x07abc").is_err()); // 声明 7 字节，实际只有 3
    }

    #[test]
    fn ie_头被截断时报错() {
        assert!(Ies::parse(b"\x06").is_err());
    }

    #[test]
    fn 数值_ie_长度不符时报错() {
        let ies = Ies::parse(b"\x0b\x01\x02").unwrap();
        assert!(ies.get(ie_id::VERSION).unwrap().as_u16().is_err());
    }
}
