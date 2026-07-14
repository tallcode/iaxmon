# iaxmon — IAX2 接收端客户端 开发文档

## 1. 目标与范围

命令行 IAX2 客户端，连到 AllStarLink 节点，把节点下发的语音解码并从本机扬声器放出来。功能对标 DVSwitch Mobile 的**收听**部分。

### 做什么

- IAX2 协议客户端（RFC 5456），UDP
- MD5 挑战认证
- 以最普通的 IAX2 话机方式呼叫指定 node，维持链路
- 接收 G.711 μ-law 语音帧，抖动缓冲，解码，播放
- 断线后指数退避自动重连
- Ctrl-C 时正常挂断

### 不做什么

任何上行内容一律排除 —— 麦克风采集、语音发送、PTT、**DTMF**（DTMF 也是发送）。此外还排除：

- 注册（REGREQ/REGAUTH/REGACK），理由见 §4.1
- 视频、Trunk 模式、加密
- ulaw 之外的编解码（服务端 `disallow=all` / `allow=ulaw`）
- GUI

## 2. 已知配置

服务端 `iax.conf` 里的对端定义：

```
[N0CALL]
type=friend
context=iax-client
auth=md5
secret=<略，见 config.toml>
host=dynamic
disallow=all
allow=ulaw
transfer=no
qualify=yes
```

客户端侧参数落在 `config.toml`。该文件已被 `.gitignore` 排除，仓库里只有不含密码的 `config.example.toml`。

| 项 | 值 | 说明 |
|---|---|---|
| host | （见本机 config.toml） | |
| port | （见本机 config.toml） | 本例用的是非标准端口，IAX2 默认是 4569 |
| username | （见本机 config.toml） | 对应 iax.conf 的 section 名 |
| secret | （见本机 config.toml，**绝不写进任何会提交的文件**） | MD5 认证用 |
| callerid | （见本机 config.toml） | 作为 CALLING NAME |
| node | （见本机 config.toml） | 呼叫的目标 extension |

截图里的 Caller Number（Optional，留空）和 Phone mode 开关都**不进配置**，理由见 §4.2。

这几条服务端配置对实现有直接影响：

- `auth=md5` → 只实现 MD5 认证分支，明文和 RSA 不做。
- `allow=ulaw` → CAPABILITY 和 FORMAT IE 都只填 `0x00000004`。
- `transfer=no` → 服务端不会发起媒体转移，TXREQ 那一套不实现，收到就忽略。
- `host=dynamic` → 服务端不知道我们的 IP，无法主动呼入，只能我们呼出。这也是不需要注册的前提。
- `qualify=yes` → 服务端会周期性发 POKE/PING，我们**必须**回 PONG，否则被判定为不可达。

## 3. 依赖

已在 `Cargo.toml` 里：

| crate | 用途 |
|---|---|
| tokio | UDP socket、定时器、任务 |
| cpal | 跨平台音频输出 |
| md-5 | 认证摘要 |
| serde + toml | 配置 |
| clap | 命令行参数 |
| anyhow | 错误处理 |
| tracing + tracing-subscriber | 日志 |

还需补一个：**ringbuf**（无锁 SPSC 环形缓冲），用于把样本从网络任务递给音频回调。音频回调里不能加锁、不能分配内存，所以不能用 `Mutex<VecDeque>`。

## 4. 协议要点

### 4.1 不做注册

`host=dynamic` 的注册作用是让 Asterisk 知道该往哪个 IP 呼叫我们；而我们是主动呼出的一方，Asterisk 靠 NEW 帧里的 USERNAME IE 匹配到 `[N0CALL]` 这条配置来认证。DVSwitch 会注册是因为它还想接受呼入。我们只收听、只呼出，所以跳过。

### 4.2 只发必要的 IE

NEW 帧只带这六个 IE，别的一概不发：

| IE | 值 |
|---|---|
| VERSION | 2 |
| USERNAME | N0CALL |
| CALLED NUMBER | 1999 |
| CALLING NAME | N0CALL |
| CAPABILITY | 0x00000004 (ulaw) |
| FORMAT | 0x00000004 (ulaw) |

外加一个 CALLTOKEN（见 §4.2.1）。不发的及原因：

- **CALLED CONTEXT** —— 服务端 `context=iax-client` 是 Asterisk 侧给这个 user 指定的入口 context，由服务端自己决定，客户端不需要也不应该自己指定。
- **CALLING NUMBER** —— 截图里留空，留空的就不发（发一个空字符串 IE 比不发更糟）。
- **ADSICPE / DNID / LANGUAGE** 等 —— 可选，与我们的用途无关。
- **phone_mode** —— 不是 IE，是 DVSwitch 的 UI 开关。我们按最普通的 IAX2 话机方式呼叫，CALLED NUMBER 直接填节点号，这个开关就没有对应物了，配置里也不保留。

### 4.2.1 CallToken —— 不做不行

这个服务端开了 `requirecalltoken`，不带呼叫令牌的 NEW 会被直接拒掉。**这是实测发现的，不是从配置推出来的**：第一版实现连不上，服务端回了一个空的 REJECT —— 没有任何 IE，源呼叫号写死是 `1`。那正是 Asterisk `send_apathetic_reply()` 的签名（它在建立呼叫状态之前就把包打发走了），而触发这条路径的就是 CallToken 检查。DVSwitch 能连上，正是因为它走了这套握手。

令牌是防 IP 伪造反射攻击的：服务端把「时间戳 + 基于来源 IP 的哈希」交给客户端，客户端原样带回来，服务端就能确认这个源 IP 是真的。流程是：

1. 客户端发 NEW，带一个**长度为 0** 的 CALLTOKEN IE，意思是「我支持 CallToken，请给我一个」
2. 服务端回 IAX/CALLTOKEN (子类 0x28)，里面的 CALLTOKEN IE 装着令牌（本服务端实测 51 字节）
3. 客户端带着这个令牌**重发** NEW，其余 IE 不变
4. 服务端这才正常走 AUTHREQ

第 2 步的应答同样是 `send_apathetic_reply()` 发的，**源呼叫号是写死的 1，不是真正的呼叫号**。所以拿到令牌后整个呼叫状态要推倒重来（序列号归零、对端呼叫号清空），只留令牌，绝不能把 `1` 当成对端的呼叫号采纳下来。

### 4.3 帧格式

**Full Frame，12 字节头**，之后跟 IE 或负载：

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|1|     Source Call Number      |R|   Destination Call Number   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                            timestamp                          |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|   OSeqno      |    ISeqno     |  Frame Type   |C|  Subclass   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

- 最高位 F=1 表示 full frame
- R = 重传标志
- 呼叫号 15 位，非 0，我们随机取 1..=32767
- timestamp = 呼叫开始至今的毫秒数
- C=1 时实际 subclass = `1 << Subclass`（这样 7 位能表达大数值）。

关于 C 位有个坑：**小于 0x80 的子类原样传，C=0；只有 ≥0x80 的才压缩成 log2 并置 C=1**，而且此时必须是 2 的幂，否则 Asterisk 直接拒绝。ulaw = 0x04 < 0x80，所以语音帧是 **C=0、subclass 字节 = 0x04**，而不是 C=1、Subclass=2。解压时 log2 值要和 `0x1f` 相与。（本文档早先版本这里写错了，已按 Asterisk `compress_subclass()` 的实际行为更正。）

**Mini Frame，4 字节头**，只用于语音：

```
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|0|     Source Call Number      |          timestamp            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

时间戳只有 16 位（完整时间戳的低 16 位），编码格式沿用**最近一个 full voice frame** 的格式。Mini frame 不需要 ACK。

F=0 且 Source Call Number=0 的是 Meta 帧（trunk/video），不支持，直接丢弃。

### 4.4 用到的帧类型和子类

Frame Type：`0x02` VOICE、`0x04` CONTROL、`0x06` IAX。

IAX 子类（Frame Type=0x06）：

| 值 | 名字 | 我们的处理 |
|---|---|---|
| 0x01 | NEW | 发出 |
| 0x02 | PING | 收到 → 回 PONG |
| 0x03 | PONG | 收到 → 回 ACK |
| 0x04 | ACK | 收发 |
| 0x05 | HANGUP | 收发 |
| 0x06 | REJECT | 收到 → 回 ACK，报错，转入重连 |
| 0x07 | ACCEPT | 收到 → 回 ACK |
| 0x08 | AUTHREQ | 收到 → 回 AUTHREP |
| 0x09 | AUTHREP | 发出 |
| 0x0a | INVAL | 收到 → 呼叫作废，转入重连 |
| 0x0b | LAGRQ | 收到 → 回 LAGRP |
| 0x0c | LAGRP | 收到 → 回 ACK |
| 0x1e | POKE | 收到 → 回 PONG（qualify=yes 会用到）|
| 0x28 | CALLTOKEN | 收到 → 带令牌重发 NEW，见 §4.2.1 |

CONTROL 子类（Frame Type=0x04）关心的：`0x01` HANGUP、`0x03` RINGING、`0x04` ANSWER、`0x05` BUSY、`0x08` CONGESTION。全部需要 ACK。

### 4.5 Information Element

格式是 `type(1) | len(1) | data(len)`，紧跟在 full frame 头后面，可以有多个。只列我们会收发的：

| 值 | 名字 | 类型 | 方向 |
|---|---|---|---|
| 0x01 | CALLED NUMBER | 字符串 | 发 |
| 0x04 | CALLING NAME | 字符串 | 发 |
| 0x06 | USERNAME | 字符串 | 发 |
| 0x08 | CAPABILITY | u32 | 发 |
| 0x09 | FORMAT | u32 | 收发 |
| 0x0b | VERSION | u16，固定 2 | 发 |
| 0x0e | AUTHMETHODS | u16 位掩码 | 收 |
| 0x0f | CHALLENGE | 字符串 | 收 |
| 0x10 | MD5 RESULT | 字符串 | 发 |
| 0x16 | CAUSE | 字符串 | 收（诊断用）|
| 0x2f | CAUSE CODE | u8 | 收（诊断用）|
| 0x36 | CALLTOKEN | 不透明字节 | 收发，见 §4.2.1 |

解析时遇到不认识的 IE 按长度跳过，不报错。

AUTHMETHODS 位掩码：`0x0001` 明文、`0x0002` MD5、`0x0004` RSA。只接受含 MD5 的，否则报错退出（不重连 —— 配置问题重试也没用）。

**本文档早先版本这里写错了**（写成 0x02/0x04/0x08，整体偏了一位），导致实现把服务端广播的 MD5 当成明文而拒绝认证。取值以 Asterisk 的 `IAX_AUTH_*` 为准：`PLAINTEXT=(1<<0), MD5=(1<<1), RSA=(1<<2)`。实测佐证：服务端 iax.conf 写的是 `auth=md5`，它在 AUTHREQ 里广播的 AUTHMETHODS 就是 `0x0002`。

媒体格式位掩码（CAPABILITY / FORMAT）：ulaw = `0x00000004`。

### 4.6 认证

`MD5_RESULT = hex(md5(challenge_string || secret))`，小写十六进制，32 字符，当字符串塞进 IE。challenge 是服务端 AUTHREQ 里 CHALLENGE IE 的原始字符串（不含结尾 NUL），secret 取自 config.toml 的 `auth.secret`。

### 4.7 呼叫流程

下面是实测跑通的流程（`RUST_LOG=iaxmon=debug` 的日志逐帧对照过）：

```
客户端                                     服务端 (<你的节点>:<端口>)
  |                                            |
  |-- NEW (VERSION,USERNAME,CALLED NUMBER,     |
  |        CALLING NAME,CAPABILITY,FORMAT,     |
  |        CALLTOKEN=空) --------------------> |
  |                                            |
  |<------------- CALLTOKEN (令牌 51 字节)     |  §4.2.1，源呼叫号是假的 1
  |                                            |
  |-- NEW (同上，CALLTOKEN=令牌) ------------> |  序列号归零重来
  |<------------- ACK                          |  只确认送达，不是应答
  |<------------- AUTHREQ (AUTHMETHODS,        |
  |                        CHALLENGE,USERNAME) |
  |                                            |
  |-- AUTHREP (MD5 RESULT) ------------------> |
  |<------------- ACK                          |
  |<------------- ACCEPT (FORMAT)              |
  |-- ACK -----------------------------------> |
  |                                            |
  |<------------- CONTROL/RINGING              |
  |-- ACK -----------------------------------> |
  |                                            |   ← 这里静默约 10 秒
  |<------------- CONTROL/ANSWER               |
  |-- ACK -----------------------------------> |
  |                                            |
  |<========= VOICE full frame (ulaw) =========|  第一帧是 full，确定格式
  |-- ACK -----------------------------------> |
  |<========= mini frames ... =================|  之后都是 mini，不用 ACK
  |                                            |
  |<------------- PING / POKE / LAGRQ          |  qualify=yes，周期性
  |-- PONG / LAGRP --------------------------> |
  |                                            |
  |-- HANGUP (Ctrl-C) -----------------------> |
  |<------------- ACK                          |
```

两个实测才发现的关键点：

- **服务端会先回一个裸 ACK 确认收到握手帧，再单独发 AUTHREQ/ACCEPT。** ACK 只代表送达，不是应答 —— 收到它要停止重传，但继续等真正的回复。把 ACK 当成应答会让握手在第一步就断掉。
- **RINGING 到 ANSWER 之间静默了约 10 秒**（服务端在放提示音），这期间一个语音帧都没有。SILENCE_TIMEOUT 必须留够余量，否则刚振铃就会被自己判成断线。

### 4.8 可靠传输

Full frame 需要 ACK，mini frame 不需要。

- **OSeqno**：我们发出的 full frame 计数，从 0 开始，每发一个非重传的 full frame +1。
- **ISeqno**：我们期望收到的下一个 full frame 的 OSeqno。收到符合期望的 full frame 后 +1。
- ACK 要带上**被确认帧的 timestamp**，且 ACK 本身不消耗 OSeqno（ACK/INVAL/VNAK/TXCNT/TXACC 这几种不递增）。
- **这条规则收发两侧都要执行 —— 收到的 ACK 同样不消耗 ISeqno。** 这是实测踩到的坑：服务端 ACK 里带的 OSeqno 是它「下一个要发的」，并没有被消耗掉。如果照常把 ISeqno 推进 1，紧接着到来的真帧（CONTROL/ANSWER）的 OSeqno 就会比我们的期望值小 1，被误判成重复帧丢弃 —— 表现是呼叫接通了却永远收不到 ANSWER。
- 重传：未收到 ACK 时按退避重发（起步 500ms，翻倍，上限 10s，最多 4 次），重发时置 R 位。超时未果 → 判定链路断开，转入重连。
- 收到重复的 full frame（OSeqno < 期望值）→ 重发一次 ACK 并丢弃。

简化：只对 NEW 和 AUTHREP 做严格重传（握手阶段丢包必须恢复）。握手之后我们的上行只剩 ACK/PONG 这类幂等帧，丢了对端会重发，不值得为它维护重传队列。

### 4.9 断线检测与重连

判定断线的三个条件，任一命中即触发重连：

- 握手帧重传耗尽
- 收到 HANGUP / REJECT / INVAL
- 长时间没收到任何来自服务端的包。**这个阈值分两段，不能用一个值**：
  - **接听前 30 秒**。振铃期间服务端在放提示音，实测会完全静默 **9.99 秒**。最早的实现用了统一的 10 秒，余量只有 13 毫秒 —— LAGRQ 稍晚一点就会在接听前一刻自判断线，然后重连→振铃→再超时，死循环，一帧音频都收不到。这种 bug 不会表现为偶发杂音，而是彻底连不上。
  - **接听后 5 秒**。接听后服务端按 50 帧/秒持续推流（见 §10），静默 5 秒等于丢了 250 帧，链路铁定没了，不必等满 30 秒。

重连用指数退避：1s → 2s → 4s → 8s → 16s → 30s 封顶，之后固定 30s 一直重试。每次重连是一次全新的呼叫（新的呼叫号、序列号归零、时间戳基准重置）。重连期间音频输出流保持打开，只是没有样本进来，回调填静音。

例外：认证被拒（服务端不支持 MD5、或 MD5 结果不对）属于配置错误，直接退出，不重连。

## 5. 音频链路

μ-law 8kHz 单声道，20ms 一帧 = 160 字节 = 160 个采样。

**解码**：μ-law → i16，标准 G.711 展开，用 256 项查找表，`const` 一次性生成。

**重采样**：macOS 默认输出设备基本只给 48000Hz，8k → 48k 是 6 倍。第一版用线性插值。如果听感不行，再换 `rubato` 做带低通的正经重采样。

**输出**：cpal 默认设备 + 默认配置，输出 f32（实测本机是 48000Hz / 2ch / f32）。音频回调从 ringbuf 消费；欠载时填 0 并累加一个原子计数器（回调里不能做 I/O，日志由外面定期打）。

线程模型：

```
tokio 任务 (UDP recv) ──► 解协议 ──► 抖动缓冲 ──► ulaw 解码 ──► 重采样 ──► ringbuf ──► cpal 回调 ──► 扬声器
                                    (20ms 定时器触发，按水位补)
```

抖动缓冲的取帧由一个 20ms 的 tokio interval 驱动，而不是由音频回调驱动 —— 保持回调里只做「从 ringbuf 拷贝」这一件事。

### 5.1 喂数据要看水位，不能看时钟

**补多少样本由 ringbuf 的当前水位决定，不由定时器触发的频率决定。** 每次 tick 就是一句「补到目标水位（60ms）为止」。

第一版实现是按时钟喂的 —— 每 20ms 推一帧，理论上正好 50 帧/秒。实测 30 秒里累计了 19160 次输出欠载（约 0.4 秒的样本），而且持续增长。原因有两层：

1. `MissedTickBehavior::Delay` 让实际周期变成「20ms + 处理耗时」，稳定慢于 50Hz；
2. 更根本的是，音频设备按自己的晶振精确消费，和 tokio 的定时器是**两个独立时钟**，永远对不齐。

按水位喂就没这个问题：设备消费掉多少，下次 tick 就补多少，自动跟上它的节奏；定时器早一点晚一点都会被水位吸收。改完之后欠载数停在启动瞬间的 2560（缓冲还没填起来时的正常现象），此后两个 30 秒窗口纹丝不动。

### 5.2 抖动缓冲

必须有，否则网络抖动直接变成爆音。

做**固定延迟**：按时间戳把语音帧排进 `BTreeMap<u32, Vec<u8>>`，预热到 100ms 再开始出帧，落后播放位置的迟到帧丢弃。缺帧时插入静音（先不做丢包隐藏）。

**时钟漂移要兜住。** 服务端按它的时钟发帧，声卡按自己的晶振消费，长期必然有偏差：偏一边是缓冲持续堆积、延迟越涨越大，偏另一边是持续欠载。所以缓冲深度超过预热深度的 2 倍（200ms）就主动丢最旧的帧追上。丢一帧只是 20ms，语音里几乎听不出来，比让延迟无限增长好得多。

（实测中缓冲深度在 30 秒里从 9 帧涨到 12 帧，但两个采样点判断不了是真漂移还是噪声，没有继续测。这个防护不依赖那个结论 —— 两个独立时钟长期对不齐是必然的，兜住就对了。）

Mini frame 的 16 位时间戳要还原成 32 位：拿最近一个 full frame 的时间戳作高 16 位基准，如果新的低 16 位相比上一个明显回绕（差值超过 32768），高位 +1。

## 6. 模块划分

```
src/
  main.rs        入口、参数、组装、Ctrl-C、重连循环
  config.rs      配置（已完成）
  proto/
    mod.rs
    frame.rs     Full/Mini frame 的编解码
    ie.rs        IE 编解码
    consts.rs    帧类型、子类、IE 类型、格式位掩码
  session.rs     呼叫状态机、序列号、ACK/重传、PING/PONG
  transport.rs   UDP socket 收发
  audio/
    mod.rs
    ulaw.rs      μ-law 解码表
    jitter.rs    抖动缓冲
    resample.rs  8k → 设备采样率
    player.rs    cpal 输出流
```

## 7. 里程碑

| # | 内容 | 状态 |
|---|---|---|
| M0 | 工程骨架 + 配置加载 | ✅ `cargo run` 正确打印配置 |
| M1 | 帧/IE 编解码 | ✅ 单元测试覆盖线格式、往返、边界、错误路径 |
| M2 | UDP + 握手 | ✅ 实测拿到 ACCEPT（额外做了 §4.2.1 的 CallToken）|
| M3 | 会话层：序列号、ACK、保活、HANGUP | ✅ 实测 75 秒链路稳定，LAGRQ/LAGRP 正常 |
| M4 | 收语音帧 + ulaw 解码 + 抖动缓冲 | ✅ 实测语音帧持续流入，迟到/溢出均为 0 |
| M5 | cpal 播放 | ✅ 输出流跑通，稳态零欠载。**音质待人耳确认** |
| M6 | 收尾：Ctrl-C 挂断、指数退避重连、漂移兜底 | ✅ 已实现，长时间稳定性待观察 |

M1 全部离线；M2 起需要连服务器。

## 8. 调试手段

- `RUST_LOG=iaxmon=debug cargo run` 看协议日志；每个收发的 full frame 打一行。
- `sudo tcpdump -i any -n udp port <你的端口> -w iax.pcap`，Wireshark 有内置 IAX2 解析器，能直接看出我们的帧格式对不对。M2 阶段最有用的工具。
- 服务端如果能上 Asterisk CLI：`iax2 set debug on` + `iax2 show channels`，能看到它对我们的帧的解读和拒绝原因。

## 9. 已确认的决策

| 决策 | 结论 |
|---|---|
| phone_mode | 忽略，按最普通的 IAX2 话机方式实现，配置项不保留 |
| CALLED NUMBER = 节点号 | ✅ 实测可用，服务端正常接通 |
| CALLED CONTEXT | 不发（多余）|
| CALLING NUMBER | 不发（留空的不发）|
| 端口 | 非标准端口，已确认无误 |
| DTMF | 不做（DTMF 属于发送）|
| 断线重连 | 做，指数退避 1s→30s 封顶；认证失败直接退出不重连 |
| 注册 | 不做（只呼出，用不上）|
| CallToken | **必须做**，这个服务端强制要求，见 §4.2.1 |

## 10. 关于 PTT

IAX2 协议本身**没有 PTT 概念**。RFC 5456 里没有任何「按下发射键 / 松开」的信令 —— 它是电话协议，一通呼叫就是一条双向的连续音频流。PTT 语义完全在 AllStarLink 的 **app_rpt** 应用层，不在 IAX2 里。

**服务端是持续推流的**，这一点有实测证据：75 秒的运行里抖动缓冲深度稳定在 9~12 帧、抖动欠载全程停在 2 没有增长。如果服务端只在有人上话时才发帧，没人说话的几十秒里缓冲必然被抽干、欠载数会一路涨。它没有。所以中继那边不管有没有人发射，服务端都按 50 帧/秒稳定推音频（包括底噪和静音）。

对我们这个只收听的客户端来说这是最省事的情况：**不需要关心 PTT，收到什么放什么**，没人上话时听到的就是静音或底噪，行为和 DVSwitch Mobile 的收听端一致。这也是 §4.9 里 SILENCE_TIMEOUT 能成立的前提。

至于发射方向（明确排除了的部分）：PTT 大概率就是「按下按钮才开始发 VOICE 帧，松开就停」，app_rpt 靠电话侧信道有没有音频帧到达来决定要不要开发射机，本质是帧级别的 VOX 而不是专门的信令。**但这一条没有实测过，只是理解，不可当结论用** —— 真要加发射，先抓包确认。协议层几乎不用动（复用同一条链路发 VOICE 帧），难点在麦克风采集和 PTT 交互。

## 11. 遗留

- **长时间稳定性待观察。** 最长只连续跑过 75 秒。时钟漂移的兜底（§5.2）已经加了，但没跑够时间验证它真的收敛 —— 两次运行里缓冲深度分别是 9~12 帧和 4 帧，这个跨运行的差异说明之前看到的「9→12」大概率只是抖动，不是漂移。
- **丢包隐藏没做。** 缺帧时插的是静音。实测网络很干净（迟到 0、欠载 0），暂时不值得做。真到了丢包率高的网络上，可以改成重复上一帧并衰减。

已消除的遗留：音质已由人耳确认正确（线性插值重采样够用，不需要换 rubato）；重连路径已通过强制断线实测，4 次重连均正常换新呼叫号、重新协商 CallToken、握手成功，退避严格按 1→2→4→8 秒走。
