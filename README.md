# iaxmon

用 Rust 写的 IAX2 **监听**客户端 —— 连接 AllStarLink 节点，把下行语音解码后从扬声器放出来。功能对标 DVSwitch Mobile 的收听部分。

只收不发：没有麦克风，没有 PTT，不会占用信道。挂着当守听台用。

```
INFO 振铃
INFO 对端接听，开始收音频
INFO ▶ 上话开始  22:38:01
INFO ■ 上话结束  22:38:01 ... 22:38:07   持续 5.6 秒
```

## 范围

**做什么**

- IAX2 协议客户端（RFC 5456），UDP
- CallToken + MD5 挑战认证
- 呼叫指定 node 并维持链路
- 接收 G.711 μ-law 语音，抖动缓冲，解码，播放
- 上话活动检测：打出「谁在什么时候上话、持续多久」
- 断线后指数退避自动重连
- Ctrl-C 正常挂断

**不做什么**

任何上行内容都不做 —— 麦克风、发射、PTT、DTMF。此外还排除注册、视频、Trunk、加密、ulaw 之外的编解码、GUI。

这是刻意的取舍，不是没做完。

## 文档

- **[PROTOCOL.md](PROTOCOL.md)** —— IAX2 协议细节：帧格式、子类压缩、IE 表、认证、CallToken、时间戳还原、序列号语义，以及 AllStarLink 侧的 dialplan 约束和 PTT 机制。与本实现无关，写给任何要做 IAX2 的人。
- **[DESIGN.md](DESIGN.md)** —— 本客户端的设计：范围取舍、模块划分、会话层、音频链路。

## 快速开始

需要 Rust 2024 edition（实测 1.96.0）。

```bash
cp config.example.toml config.toml
$EDITOR config.toml          # 填入你自己的服务器和凭据
cargo run --release
```

节点会先振铃约 10 秒（服务端在放提示音），然后接通，之后就能听到声音了。

## 配置

```toml
[server]
host = "your.allstar.node"
port = 4569                  # IAX2 默认端口

[auth]
username = "YOURCALL"        # 对应服务端 iax.conf 的 section 名
secret   = "your-secret"

[caller]
callerid = "YOURCALL"        # 作为 CALLING NAME 发出

[call]
node = "1999"                # 要呼叫的 extension

[audio]
codec = "ulaw"

[activity]
threshold = 50.0             # RMS 阈值，超过即判定有人上话
hang_ms = 500                # 静音多久判定上话结束
```

> **`config.toml` 含密码，已被 `.gitignore` 排除，不要提交。**
>
> 编辑器的交换文件（`.config.toml.swp` 之类）是它的镜像，同样含密码，也已被忽略。

服务端 `iax.conf` 对应的配置大致长这样：

```
[YOURCALL]
type=friend
context=iax-client
auth=md5
secret=your-secret
host=dynamic
disallow=all
allow=ulaw
transfer=no
qualify=yes
```

## 上话活动检测

IAX2 电话连接拿不到任何带内的按键信号 —— app_rpt 的按键状态只从 AMI 出去（见 [PROTOCOL.md §9.4](PROTOCOL.md)），所以只能靠音频能量判断。

实测服务端空闲时推的是**精确的数字静音**，真实音频即使在词间停顿也是非零的，两者区分得很干净，默认阈值有 250 倍的余量。若你的节点发舒适噪声而非纯静音，把 `threshold` 调到底噪之上 —— `--verbose` 的统计行会打峰值 RMS，方便对照。

时长算到最后一个有声帧为止：词间停顿算进去（同一次上话），末尾那段用于判定结束的静音不算（那是检测延迟）。

## 排查

默认只打真正关心的信息。加 `--verbose` 看协议细节、链路状态和周期统计：

```bash
iaxmon --verbose
RUST_LOG=iaxmon=trace iaxmon    # 连原始字节一起打；RUST_LOG 优先于 --verbose
```

`--verbose` 下每 30 秒打一行统计：

```
统计: 缓冲 4 帧 / 迟到 0 / 抖动欠载 0 / 溢出 0 / 漂移丢帧 0 / 输出欠载 2048 / 峰值 RMS 12845
```

- **输出欠载**在启动瞬间会有一批（缓冲还没填起来），稳态下**不应该继续增长**。持续涨说明喂数据跟不上设备消费。
- **迟到 / 溢出**长期为 0 才正常，持续增长说明网络有问题。
- **漂移丢帧**偶尔加一是正常的（服务端时钟和本机声卡晶振对不齐），持续快速增长说明偏差异常大。

抓包对照（Wireshark 有内置 IAX2 解析器）：

```bash
sudo tcpdump -i any -n udp port 4569 -w iax.pcap
```

## 常见问题

**呼叫被拒绝，服务端没给原因** —— 多半是服务端要求 CallToken 而客户端没带。本项目已实现，见 DESIGN.md §4.2.1。

**认证被拒绝** —— 用户名或密码不对。这类错误不会重连，程序直接退出。

**振铃后就断线重连，永远听不到声音** —— 静默超时设得比服务端提示音还短。见 DESIGN.md §4.9，接听前后用的是两个不同的阈值。

## 作者

BG5ATV

## 协议

MIT，见 [LICENSE](LICENSE)。
