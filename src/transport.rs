//! UDP 收发。

use anyhow::{Context, Result, bail};
use std::net::SocketAddr;
use tokio::net::UdpSocket;

/// IAX2 的包不会超过这个大小（以太网 MTU 之内）
const MAX_DATAGRAM: usize = 1500;

pub struct Transport {
    sock: UdpSocket,
    peer: SocketAddr,
}

impl Transport {
    pub async fn connect(host: &str, port: u16) -> Result<Self> {
        let peer = tokio::net::lookup_host((host, port))
            .await
            .with_context(|| format!("解析 {host}:{port} 失败"))?
            .next()
            .with_context(|| format!("{host} 没有解析到任何地址"))?;

        let sock = UdpSocket::bind("0.0.0.0:0").await.context("绑定本地 UDP 端口失败")?;
        // connect 之后内核只投递来自 peer 的包，省掉每次收包的来源校验
        sock.connect(peer).await.with_context(|| format!("连接 {peer} 失败"))?;

        tracing::info!("UDP 已就绪: {} -> {}", sock.local_addr()?, peer);
        Ok(Self { sock, peer })
    }

    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    pub async fn send(&self, buf: &[u8]) -> Result<()> {
        tracing::trace!("→ {}", hex(buf));
        let n = self.sock.send(buf).await.context("发送失败")?;
        if n != buf.len() {
            bail!("发送不完整: {}/{} 字节", n, buf.len());
        }
        Ok(())
    }

    pub async fn recv(&self) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        let n = self.sock.recv(&mut buf).await.context("接收失败")?;
        buf.truncate(n);
        tracing::trace!("← {}", hex(&buf));
        Ok(buf)
    }
}

/// 逐字节十六进制，配合 Wireshark 对着看
fn hex(buf: &[u8]) -> String {
    use std::fmt::Write;
    buf.iter().fold(String::with_capacity(buf.len() * 3), |mut s, b| {
        let _ = write!(s, "{b:02x} ");
        s
    })
}
