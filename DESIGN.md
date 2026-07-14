# iaxmon 设计文档

IAX2 协议本身的细节见 [PROTOCOL.md](PROTOCOL.md)。本文只讲这个客户端的设计。

## 1. 目标与范围

命令行 IAX2 客户端，连到 AllStarLink 节点，把下行语音解码后从扬声器放出来。功能对标 DVSwitch Mobile 的**收听**部分。

### 做什么

- IAX2 协议客户端（RFC 5456），UDP
- CallToken + MD5 挑战认证
- 以普通 IAX2 话机方式呼叫指定 node，维持链路
- 接收 G.711 μ-law 语音，抖动缓冲，解码，播放
- 断线后指数退避自动重连
- Ctrl-C 正常挂断

### 不做什么

任何上行内容一律排除 —— 麦克风采集、语音发送、PTT、**DTMF**（DTMF 也是发送）。此外还排除：

- 注册（REGREQ/REGAUTH/REGACK），理由见 §2.1
- 视频、Trunk 模式、加密
- ulaw 之外的编解码
- GUI

这是刻意的取舍，不是没做完。

> ⚠️ **「只收不发」有一个前提不在客户端手里**：若服务端 dialplan 用 `Rpt()` 的 `D` 模式，仅仅接入呼叫就会让中继发射机全程按下，我们一帧不发也一样。详见 [PROTOCOL.md §9.3](PROTOCOL.md)。ASL stock 配置用的是 `P` 模式，安全。换到新节点前值得确认。

## 2. 协议层的取舍

### 2.1 不做注册

注册的作用是让服务端知道该往哪个 IP 呼叫我们。我们是主动呼出的一方，服务端靠 NEW 帧里的 USERNAME IE 匹配 peer 配置来认证，用不上注册。

DVSwitch 会注册是因为它还要接受呼入。

### 2.2 NEW 帧只带七个 IE

| IE | 值 |
|---|---|
| VERSION | 2 |
| USERNAME | 配置的用户名 |
| CALLED NUMBER | 配置的节点号 |
| CALLING NAME | 配置的 callerid |
| CAPABILITY | 0x00000004 (ulaw) |
| FORMAT | 0x00000004 (ulaw) |
| CALLTOKEN | 空（首次）/ 服务端下发的令牌（重发）|

不发的及理由：

- **CALLED CONTEXT** —— 入口 context 由服务端的 peer 配置指定，客户端不需要也不应该自己指定。
- **CALLING NUMBER** —— 配置里留空，留空的就不发（发一个空字符串 IE 比不发更糟）。ASL 的 `[iax-client]` dialplan 只把它塞进 `NODENUM` 打日志，不发无害。
- **ADSICPE / DNID / LANGUAGE** 等 —— 与本用途无关。

CALLING NAME 不能省：ASL 的 dialplan 在它为空时会直接挂断。见 [PROTOCOL.md §9.2](PROTOCOL.md)。

### 2.3 只实现 MD5 认证

服务端配置 `auth=md5`。AUTHREQ 里 AUTHMETHODS 不含 MD5 位时直接报错退出，不重连 —— 那是配置问题，重试无意义。

### 2.4 只实现 ulaw

服务端配置 `disallow=all` / `allow=ulaw`。CAPABILITY 和 FORMAT 都只填 ulaw；ACCEPT 里服务端选定的 FORMAT 若不是 ulaw，报错退出。

### 2.5 只对握手帧做严格重传

NEW 和 AUTHREP 用退避重传（500ms 起，翻倍，上限 10s，最多 4 次），握手阶段丢包必须恢复。

握手之后我们的上行只剩 ACK / PONG / LAGRP 这类幂等帧，丢了对端会重发，不值得为它维护重传队列。

### 2.6 media transfer 不实现

服务端配置 `transfer=no`，不会发起媒体转移。收到 TXREQ 一类的帧由兜底分支 ACK 后忽略。

## 3. 配置

落在 `config.toml`，已被 `.gitignore` 排除；仓库里只有不含密码的 `config.example.toml`。

| 项 | 说明 |
|---|---|
| `server.host` / `server.port` | 服务端地址。IAX2 默认端口 4569 |
| `auth.username` | 对应服务端 iax.conf 的 section 名 |
| `auth.secret` | MD5 认证用 |
| `caller.callerid` | 作为 CALLING NAME 发出，不能为空 |
| `call.node` | 呼叫的目标 extension |
| `audio.codec` | 目前只支持 `ulaw` |

凭据只存在于 `config.toml` 一处。文档里用占位符，测试里用假密码。

服务端 `iax.conf` 对应的 peer 配置形如：

```
[N0CALL]
type=friend
context=iax-client
auth=md5
secret=<密码>
host=dynamic
disallow=all
allow=ulaw
transfer=no
qualify=yes
```

## 4. 依赖

| crate | 用途 |
|---|---|
| tokio | UDP socket、定时器、任务 |
| cpal | 跨平台音频输出 |
| ringbuf | 无锁 SPSC 环形缓冲，网络任务 → 音频回调 |
| md-5 | 认证摘要 |
| serde + toml | 配置 |
| clap | 命令行参数 |
| anyhow | 错误处理 |
| tracing + tracing-subscriber | 日志 |

音频回调里不能加锁、不能分配内存，所以样本必须走无锁环形缓冲，不能用 `Mutex<VecDeque>`。

## 5. 模块划分

```
src/
  main.rs        入口、参数、组装、Ctrl-C、重连循环
  config.rs      配置
  proto/
    consts.rs    帧类型、子类、IE 类型、位掩码
    frame.rs     Full/Mini frame 编解码，子类压缩
    ie.rs        IE 编解码
  session.rs     呼叫状态机、序列号、ACK、保活、时间戳还原
  transport.rs   UDP 收发
  audio/
    ulaw.rs      μ-law 解码表
    jitter.rs    抖动缓冲
    resample.rs  8k → 设备采样率
    player.rs    cpal 输出流
    mod.rs       AudioSink，把上面四个串成一条链路
```

`proto/` 不依赖会话层以上的任何东西，纯粹是协议编解码，可独立测试。

## 6. 会话层

### 6.1 错误分类

`SessionError` 分两类，决定重连与否：

- **`Fatal`** —— 配置或凭据问题，重试多少次都一样。认证被拒、服务端不支持 MD5、选定编码不是 ulaw。直接退出。
- **`Retry`** —— 网络或对端状态问题。默认归类：`From<anyhow::Error>` 落到 `Retry`，致命错误必须显式构造，免得漏判导致无限重试。

### 6.2 断线检测

三个条件任一命中即触发重连：

- 握手帧重传耗尽
- 收到 HANGUP / REJECT / INVAL
- 长时间没收到任何包 —— **阈值分两段**：

| 阶段 | 阈值 | 理由 |
|---|---|---|
| 接听前 | 30 秒 | ASL 的 dialplan 里有硬编码的 `Wait(10)`，振铃期间完全静默约 10 秒。阈值必须远大于它，否则会在接听前一刻自判断线并陷入重连死循环 |
| 接听后 | 5 秒 | 服务端按 50 帧/秒持续推流，静默 5 秒等于丢了 250 帧，链路铁定没了 |

见 [PROTOCOL.md §9.2 / §9.4](PROTOCOL.md)。

### 6.3 重连

指数退避 1s → 2s → 4s → 8s → 16s → 30s 封顶，之后固定 30s。

每次重连是一次全新的呼叫：新的呼叫号、序列号归零、时间戳基准重置、重新协商 CallToken。

退避**只在呼叫活过 30 秒后才归零**。否则「一连上就被踢」会退化成 1 秒一次的死循环重试。

重连期间音频输出流保持打开，回调自动填静音。

**抖动缓冲必须在每次新呼叫前 `reset()`** —— 新呼叫的时间戳从 0 重新起算，不清掉旧的播放位置的话，新帧会全部被判为迟到帧丢弃，表现为重连后再也没声音。

## 7. 音频链路

μ-law 8kHz 单声道，20ms 一帧 = 160 字节 = 160 个采样。

```
tokio 任务 (UDP recv) ──► 解协议 ──► 抖动缓冲 ──► ulaw 解码 ──► 重采样 ──► ringbuf ──► cpal 回调 ──► 扬声器
                                    (20ms 定时器触发，按水位补)
```

抖动缓冲的取帧由 20ms 的 tokio interval 驱动，不由音频回调驱动 —— 保持回调里只做「从 ringbuf 拷贝」这一件事。

### 7.1 喂数据看水位，不看时钟

**补多少样本由 ringbuf 的当前水位决定，不由定时器触发的频率决定。** 每次 tick 就是一句「补到目标水位（60ms）为止」。

音频设备按自己的晶振精确消费，和 tokio 的定时器是两个独立时钟，永远对不齐。按时钟喂必然持续欠载或溢出；按水位喂则是设备消费多少就补多少，定时器早一点晚一点都会被水位吸收。

### 7.2 抖动缓冲

固定延迟：按时间戳排进 `BTreeMap<u32, Vec<u8>>`，预热到 100ms 再开始出帧。

- 时间戳早于播放位置的**迟到帧**直接丢弃，不能插队。
- 缺帧时插静音（不做丢包隐藏）。
- 缓冲被抽干后重新预热，避免在空缓冲上持续单帧抖动。
- **深度超过预热深度的 2 倍（200ms）就主动丢最旧的帧**。服务端发帧时钟和本机声卡晶振长期必然有偏差，偏一边是延迟越涨越大，偏另一边是持续欠载。丢一帧只是 20ms，语音里几乎听不出来，比让延迟无限增长好得多。
- 硬上限 100 帧（2 秒），防止对端猛灌导致无限增长。

### 7.3 解码与重采样

μ-law → i16 用编译期生成的 256 项查找表。

macOS 默认输出设备基本只给 48000Hz，8k → 48k 用线性插值，对语音够用（已由人耳确认）。

**位置用有理数表示**（整数部分 + 以 out_rate 为分母的分数），不用 f64 累加 —— 浮点累加会漂：8k→48k 本该每块出 960 个样本，误差让位置停在 159.99999999999997 而多挤出一个，长跑下来就是持续的时钟偏移。

### 7.4 输出

cpal 默认设备 + 默认配置。回调里单声道展开到设备的所有声道；欠载时填 0 并累加一个原子计数器（回调里不能做 I/O，日志由外面定期打）。

## 8. 状态

| # | 内容 | 状态 |
|---|---|---|
| M0 | 工程骨架 + 配置加载 | ✅ |
| M1 | 帧/IE 编解码 | ✅ 单元测试覆盖线格式、往返、边界、错误路径 |
| M2 | UDP + 握手（含 CallToken）| ✅ 实测拿到 ACCEPT |
| M3 | 会话层：序列号、ACK、保活、HANGUP | ✅ 实测链路稳定 |
| M4 | 收语音帧 + ulaw 解码 + 抖动缓冲 | ✅ 迟到/溢出均为 0 |
| M5 | cpal 播放 | ✅ 音质经人耳确认 |
| M6 | Ctrl-C 挂断、指数退避重连、漂移兜底 | ✅ 重连经强制断线实测 |

## 9. 调试

```bash
RUST_LOG=iaxmon=debug cargo run    # 每个收发的 full frame 打一行
RUST_LOG=iaxmon=trace cargo run    # 连原始字节一起打
```

每 30 秒一行统计：

```
统计: 缓冲 4 帧 / 迟到 0 / 抖动欠载 0 / 溢出 0 / 漂移丢帧 0 / 输出欠载 2048
```

- **输出欠载**在启动瞬间会有一批（缓冲还没填起来），稳态下不应继续增长。持续涨说明喂数据跟不上设备消费。
- **迟到 / 溢出**长期为 0 才正常。
- **漂移丢帧**偶尔加一是正常的，持续快速增长说明时钟偏差异常大。

抓包见 [PROTOCOL.md §11](PROTOCOL.md)。

## 10. 遗留

- **长时间稳定性待观察。** 最长连续跑过 75 秒。mini frame 时间戳每 65.536 秒回绕一次，跨多个回绕边界的长跑尚未做过。
- **丢包隐藏没做。** 缺帧时插静音。实测网络干净（迟到 0、欠载 0），暂不值得做。丢包率高的网络上可改成重复上一帧并衰减。
- **VNAK 收到后只回 ACK，不重传**；自己发现入向有缺口时发的是 ACK 而非 VNAK。都不危险：我们的 ACK 带的是未推进的 ISeqno，不会谎称收到了没收到的帧，对端的重传定时器照样能补上；且握手后我们的上行只有幂等帧。属于恢复得比理论上慢，不是恢复不了。
- **POKE 处理是死代码。** POKE 携带 `dcallno=0`，会被 `session.rs` 里的 `dest_call != source_call` 前置过滤丢弃，走不到处理分支。留着无害，但它不工作。见 [PROTOCOL.md §10](PROTOCOL.md)。
