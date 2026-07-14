# IAX2 协议参考

Inter-Asterisk eXchange v2（RFC 5456）的实现要点，以及 AllStarLink 侧的相关行为。

本文只记录**已确认的事实**，每条要么来自 RFC，要么来自实测抓包，要么来自源码（附行号）。不确定的另立 §10 单独标注。

传输层是 UDP，默认端口 4569，收发共用一个端口。所有多字节数值一律**大端序**。

---

## 1. 帧格式

一个 UDP 包携带一个帧。首字节的最高位（F 位）区分帧的种类。

### 1.1 Full Frame

12 字节头，之后跟 IE 序列（IAX/CONTROL 帧）或媒体负载（VOICE 帧）。

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

| 字段 | 宽度 | 说明 |
|---|---|---|
| F | 1 bit | 恒为 1，标识 full frame |
| Source Call Number | 15 bits | 发送方给本呼叫分配的编号，非 0 |
| R | 1 bit | 重传标志。重发同一帧时置位 |
| Destination Call Number | 15 bits | 对端给本呼叫分配的编号。首个 NEW 里为 0（尚不知道）|
| timestamp | 32 bits | 呼叫开始至今的毫秒数 |
| OSeqno | 8 bits | 发送方的出向序列号 |
| ISeqno | 8 bits | 发送方期望收到的下一个 OSeqno |
| C | 1 bit | 子类压缩标志，见 §2 |
| Subclass | 7 bits | 子类，含义取决于 Frame Type |

呼叫号 15 位，取值 1..=32767，0 不合法。

### 1.2 Mini Frame

4 字节头，之后是媒体负载。只用于语音。

```
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|0|     Source Call Number      |          timestamp            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

- 时间戳只有 16 位，是完整时间戳的低 16 位。还原方法见 §7。
- 不携带子类：**编码格式沿用最近一个 full voice frame 的格式**。
- 不需要 ACK，也不参与序列号。

### 1.3 Meta Frame

F=0 且 Source Call Number=0 的是 meta 帧，用于 trunk 和视频。格式另有定义，本文不涉及。

**解析器必须先判断这个条件**，否则会把 meta 帧误解析成 mini frame。

---

## 2. 子类压缩

Subclass 字段只有 7 位，但子类值可以很大（媒体格式是位掩码，可达 2^31）。规则：

- **子类 < 0x80**：原样放进 Subclass 字段，C=0。
- **子类 ≥ 0x80**：必须是 2 的幂，存其 log2 值，C=1。不是 2 的幂则无法编码。
- **0xFF 是特例**：表示子类 -1，不是 `1 << 31`。解压时必须在通用路径之前判断。

解压时 log2 值要和 `0x1F` 相与（Asterisk 的 `IAX_MAX_SHIFT`）。

常见误解：ulaw = 0x04，小于 0x80，所以语音帧的子类字节是 **`0x04`（C=0）**，不是 `0x82`（C=1, log2=2）。

对应 Asterisk 的 `compress_subclass()` / `uncompress_subclass()`。

---

## 3. 帧类型与子类

### 3.1 Frame Type（full frame 头的第 11 字节）

| 值 | 名称 |
|---|---|
| 0x01 | DTMF |
| 0x02 | VOICE |
| 0x03 | VIDEO |
| 0x04 | CONTROL |
| 0x05 | NULL |
| 0x06 | IAX |
| 0x07 | TEXT |
| 0x08 | IMAGE |
| 0x09 | HTML |
| 0x0A | COMFORT NOISE |

### 3.2 IAX 子类（Frame Type = 0x06）

| 值 | 名称 | 说明 |
|---|---|---|
| 0x01 | NEW | 发起呼叫 |
| 0x02 | PING | 探测请求 |
| 0x03 | PONG | PING/POKE 的应答 |
| 0x04 | ACK | 显式确认 |
| 0x05 | HANGUP | 拆除呼叫 |
| 0x06 | REJECT | 拒绝呼叫 |
| 0x07 | ACCEPT | 接受呼叫 |
| 0x08 | AUTHREQ | 认证请求 |
| 0x09 | AUTHREP | 认证应答 |
| 0x0A | INVAL | 呼叫无效 |
| 0x0B | LAGRQ | 延迟测量请求 |
| 0x0C | LAGRP | 延迟测量应答 |
| 0x0D | REGREQ | 注册请求 |
| 0x0E | REGAUTH | 注册认证 |
| 0x0F | REGACK | 注册确认 |
| 0x10 | REGREJ | 注册拒绝 |
| 0x11 | REGREL | 注册释放 |
| 0x12 | VNAK | 请求重传 |
| 0x13 | DPREQ | 拨号方案请求 |
| 0x14 | DPREP | 拨号方案应答 |
| 0x15 | DIAL | 拨号 |
| 0x16 | TXREQ | 转移请求 |
| 0x17 | TXCNT | 转移连接 |
| 0x18 | TXACC | 转移接受 |
| 0x19 | TXREADY | 转移就绪 |
| 0x1A | TXREL | 转移释放 |
| 0x1B | TXREJ | 转移拒绝 |
| 0x1C | QUELCH | 暂停媒体 |
| 0x1D | UNQUELCH | 恢复媒体 |
| 0x1E | POKE | 探测请求（peer 级，不属于任何呼叫）|
| 0x20 | MWI | 留言等待指示 |
| 0x21 | UNSUPPORT | 不支持的消息 |
| 0x22 | TRANSFER | 远端转移请求 |

以下是 Asterisk 扩展，未在 RFC 5456/5457 注册：

| 值 | 名称 |
|---|---|
| 0x23 | PROVISION |
| 0x24 | FWDOWNL |
| 0x25 | FWDATA |
| 0x26 | TXMEDIA |
| 0x27 | RTKEY |
| 0x28 | **CALLTOKEN** |

### 3.3 CONTROL 子类（Frame Type = 0x04）

线上的 CONTROL 子类就是 Asterisk `frame.h` 里 `ast_control_frame_type` 的枚举值。

| 值 | 名称 | 说明 |
|---|---|---|
| 0x01 | HANGUP | |
| 0x02 | RING | 本地振铃 |
| 0x03 | RINGING | 对端振铃 |
| 0x04 | ANSWER | 对端接听 |
| 0x05 | BUSY | |
| 0x08 | CONGESTION | |
| 0x09 | FLASH | |
| 0x0B | OPTION | |
| **0x0C** | **RADIO_KEY** | 按下发射（12）|
| **0x0D** | **RADIO_UNKEY** | 松开发射（13）|
| 0x0E | PROGRESS | |
| 0x0F | PROCEEDING | |
| 0x10 | HOLD | |
| 0x11 | UNHOLD | |

CONTROL 帧全部需要 ACK。

值均取自 Asterisk 上游的 `include/asterisk/frame.h`。映射关系有实测佐证：我们在线上收到的 `RINGING` 和 `ANSWER` 的子类正是 3 和 4，与该枚举吻合。

---

## 4. Information Element

线格式 `type(1) | len(1) | data(len)`，紧跟在 full frame 头后面，可以有多个。`len` 为 0 合法 —— 有些 IE 靠**存在与否**传递信息（如 AUTOANSWER、以及 CallToken 握手第一步的 CALLTOKEN）。

解析时遇到不认识的 IE 按长度跳过，不要报错。

| 值 | 名称 | 类型 |
|---|---|---|
| 0x01 | CALLED NUMBER | 字符串 |
| 0x02 | CALLING NUMBER | 字符串 |
| 0x03 | CALLING ANI | 字符串 |
| 0x04 | CALLING NAME | 字符串 |
| 0x05 | CALLED CONTEXT | 字符串 |
| 0x06 | USERNAME | 字符串 |
| 0x07 | PASSWORD | 字符串 |
| 0x08 | CAPABILITY | u32 位掩码 |
| 0x09 | FORMAT | u32 位掩码 |
| 0x0A | LANGUAGE | 字符串 |
| 0x0B | VERSION | u16，恒为 2 |
| 0x0C | ADSICPE | u16 |
| 0x0D | DNID | 字符串 |
| 0x0E | AUTHMETHODS | u16 位掩码 |
| 0x0F | CHALLENGE | 字符串 |
| 0x10 | MD5 RESULT | 字符串（32 字符小写十六进制）|
| 0x11 | RSA RESULT | 字符串 |
| 0x12 | APPARENT ADDR | sockaddr |
| 0x13 | REFRESH | u16 |
| 0x14 | DPSTATUS | u16 |
| 0x15 | CALLNO | u16 |
| 0x16 | CAUSE | 字符串 |
| 0x17 | IAX UNKNOWN | u8 |
| 0x18 | MSGCOUNT | u16 |
| 0x19 | AUTOANSWER | 零长度 |
| 0x1A | MUSICONHOLD | 字符串 |
| 0x1B | TRANSFERID | u32 |
| 0x1C | RDNIS | 字符串 |
| 0x1F | DATETIME | u32（打包的日期时间）|
| 0x26 | CALLINGPRES | u8 |
| 0x27 | CALLINGTON | u8 |
| 0x28 | CALLINGTNS | u16 |
| 0x29 | SAMPLINGRATE | u16 |
| **0x2A** | **CAUSECODE** | **u8（Q.931 原因码）** |
| 0x2B | ENCRYPTION | u16 |
| 0x2C | ENCKEY | 裸字节 |
| 0x2D | CODEC PREFS | 字符串 |
| 0x2E | RR JITTER | u32 |
| 0x2F | RR LOSS | u32 |
| 0x30 | RR PKTS | u32 |
| 0x31 | RR DELAY | u16 |
| 0x32 | RR DROPPED | u32 |
| 0x33 | RR OOO | u32 |
| 0x34 | OSPTOKEN | 裸字节 |
| **0x36** | **CALLTOKEN** | **不透明字节**（Asterisk 扩展）|

> **CAUSECODE 是 0x2A，不是 0x2F。** 0x2F 是 RR LOSS（u32）。两者都在错误处理路径上出现，容易混淆。

注意 IE 空间和 IAX 子类空间是**两套独立编号**：IE 的 0x28 是 CALLINGTNS，IAX 子类的 0x28 是 CALLTOKEN。

### 4.1 AUTHMETHODS 位掩码

| 值 | 方式 |
|---|---|
| 0x0001 | 明文 |
| 0x0002 | **MD5** |
| 0x0004 | RSA |

即 Asterisk 的 `IAX_AUTH_PLAINTEXT = 1<<0`、`IAX_AUTH_MD5 = 1<<1`、`IAX_AUTH_RSA = 1<<2`。

### 4.2 媒体格式位掩码（CAPABILITY / FORMAT）

| 值 | 编解码 |
|---|---|
| 0x00000001 | G.723.1 |
| 0x00000002 | GSM |
| 0x00000004 | **G.711 μ-law** |
| 0x00000008 | G.711 A-law |
| 0x00000010 | G.726 |
| 0x00000020 | ADPCM |
| 0x00000040 | 16-bit 线性 PCM |
| 0x00000080 | LPC10 |
| 0x00000100 | G.729 |
| 0x00000200 | Speex |
| 0x00000400 | iLBC |

CAPABILITY 是「我支持哪些」，FORMAT 是「我想用哪个」。服务端在 ACCEPT 里用 FORMAT 告知最终选定的编码。

---

## 5. 认证

Asterisk 的 `auth=md5` 走挑战-应答：

1. 服务端在 AUTHREQ 里给出 AUTHMETHODS 和 CHALLENGE（一个字符串）。
2. 客户端计算 `MD5_RESULT = hex(md5(challenge || secret))`，小写十六进制，32 字符，作为**字符串** IE 送回。
3. `challenge` 取 CHALLENGE IE 的原始字节，不含结尾 NUL；`secret` 是共享密钥。两者直接拼接，中间无分隔符。

Asterisk 用 `strcasecmp` 比较，大小写实际上不敏感。

---

## 6. CallToken

Asterisk 的扩展，用于防御 IP 伪造的反射攻击。**Asterisk 20+（含 ASL3）默认要求**。

服务端把「时间戳 + 基于来源 IP 的哈希」交给客户端，客户端原样带回，服务端据此确认来源 IP 真实。

握手：

1. 客户端发 NEW，携带一个**长度为 0** 的 CALLTOKEN IE（`0x36 0x00`），含义是「我支持 CallToken，请给我一个」。
2. 服务端回 IAX/CALLTOKEN（子类 0x28），其中的 CALLTOKEN IE 装着令牌。
3. 客户端携带该令牌**重发** NEW，其余 IE 不变。
4. 服务端正常走 AUTHREQ。

令牌格式是 `"<unix时间戳>?<40字符sha1>"`，即 10+1+40 = **51 字节**。哈希覆盖来源地址、时间戳和服务端密钥，**不含呼叫号**。

实现要点：

- **第 2 步的应答由 Asterisk 的 `send_apathetic_reply()` 发出**，它在建立呼叫状态之前就返回。因此该帧的 **Source Call Number 是硬编码的 1**，不是真正的呼叫号；OSeqno=0，ISeqno=你的 OSeqno+1。这些字段**都不能采纳**。
- 重发 NEW 前要把 OSeqno、ISeqno、对端呼叫号全部归零，只保留令牌。
- **不要 ACK 这个 CALLTOKEN 帧** —— 服务端此时没有对应的呼叫状态，会回 INVAL。
- 自己的呼叫号**不需要**更换（哈希不含呼叫号）。
- 时间戳**不需要**重置。唯一的时间约束是令牌有效期（`max_calltoken_delay`，默认约 10 秒），立即重发即可。
- 客户端只要发了空 CALLTOKEN IE，Asterisk **不管服务端是否配置了 `requirecalltoken` 都会下发令牌**。所以实现了 CallToken 的客户端对两种服务端都兼容。

若客户端**完全不发** CALLTOKEN IE 而服务端要求它，服务端会通过 `send_apathetic_reply()` 回一个 **REJECT**，特征是：**不含任何 IE**，且 Source Call Number = 1。识别到这个特征即可判定是 CallToken 问题。

服务端侧可用 `requirecalltoken = no` 关闭（ASL 的 stock 模板就是这么做的，为了兼容老客户端）。

---

## 7. 时间戳

Full frame 带 32 位完整时间戳，单位毫秒，从呼叫开始起算。

Mini frame 只带低 16 位，窗口 65536ms，约 **65.5 秒回绕一次**。还原需要用最近一个 full frame 的时间戳作基准。

**必须双向判断**（对应 Asterisk 的 `unwrap_timestamp()`）：

```
候选值 = (基准 & 0xFFFF0000) | 低16位
差值   = 候选值 - 基准        (按有符号解释)

若 差值 < -32768：  候选值 += 65536    // 实际在下一个窗口
若 差值 >  32768：  候选值 -= 65536    // 实际在上一个窗口
否则：              候选值不变
```

**基准只能在时间戳前进时推进。** 乱序到达的旧帧不能把基准拖回去 —— 否则紧接着的正常帧会相对被拖回的基准误判窗口，且高位一旦错误进位就无法回退，此后每帧永久偏移 65536ms。

只处理单向（低位变小就进位）会在跨回绕边界的乱序帧上出错：一个真值属于上一窗口的旧帧会被算高 65536ms。

Asterisk 在发送侧的规律：**完整时间戳的高 16 位发生变化时，会发一个 full voice frame**。所以每约 65.5 秒至少有一次基准对表的机会。

---

## 8. 可靠传输

Full frame 需要 ACK，mini frame 不需要。

- **OSeqno**：发出的 full frame 计数，从 0 开始。
- **ISeqno**：期望收到的下一个 full frame 的 OSeqno。
- ACK 帧要携带**被确认帧的 timestamp**，而不是自己的当前时间。

**不占用序列号的子类**（Frame Type = IAX 时）：

```
ACK (0x04)  INVAL (0x0A)  VNAK (0x12)  TXCNT (0x17)  TXACC (0x18)
```

**这条规则收发两侧都要执行**：

- 发送侧：发这些帧不递增自己的 OSeqno。
- 接收侧：收到这些帧不消耗 ISeqno。收到的 ACK 里带的 OSeqno 是对端「下一个要发的」，并未被消耗；若照常推进 ISeqno，紧接着到来的真帧就会被误判成重复帧而丢弃。

收到重复的 full frame（OSeqno 小于期望值）应重发一次 ACK 并丢弃。

Asterisk 有隐式 ACK 窗口逻辑：一个 full frame 的 ISeqno 字段会隐式确认对端此前的所有帧。所以不必对每个帧都单独回 ACK，只要后续帧的 ISeqno 正确即可。

重传：未收到 ACK 时按退避重发，重发时置 R 位。

---

## 9. AllStarLink 侧行为

以下基于 `app_rpt` 源码（行号对应 AllStarLink 的 `apps/app_rpt.c`）和 ASL 的 stock 配置。

### 9.1 接入方式

ASL 的 `iax.conf` 有两个面向客户端的模板：

- **`[iaxclient]`** → `context = iax-client`。给 DVSwitch Mobile、Zoiper 这类**话机**用。
- **`[iaxrpt]`** → `context = iaxrpt`。给 iaxRpt 这类**PC 节点客户端**用。

两者的 dialplan 不同，客户端行为要求也不同。**走哪个 context 由服务端的 peer 配置决定，客户端无法选择。**

### 9.2 `[iax-client]` dialplan

ASL stock 配置（`app_rpt/configs/rpt/extensions.conf`）：

```
[iax-client]
exten => _XXXX!,1,Set(NODENUM=${CALLERID(num)})
	same => n,ExecIf($[!${RPT_NODE(${EXTEN},exists)}]?Hangup)
	same => n,Ringing()
	same => n,Wait(10)
	same => n,Answer()
	same => n,Set(CALLSIGN=${CALLERID(name)})
	same => n,GotoIf(${ISNULL(${CALLSIGN})}?hangit)
	same => n,Playback(rpt/connected-to&rpt/node)
	same => n,SayDigits(${EXTEN})
	same => n,Rpt(${EXTEN},P,${CALLSIGN}-P)
	same => n(hangit),NoOp(No Caller ID Name)
	same => n,Playback(connection-failed)
	same => n,Wait(1)
	same => n,Hangup
```

对客户端实现的三个硬约束：

1. **CALLED NUMBER 填节点号**，匹配 `_XXXX!`（4 位及以上）。节点不存在则直接 Hangup。
2. **CALLING NAME 必需**。为空会走到 `hangit`，放一句 "connection failed" 后挂断。
3. **RINGING 到 ANSWER 之间有 `Wait(10)`，期间完全静默**。任何基于「多久没收到包」的断线判断，阈值必须大于 10 秒，否则会在接听前一刻自判断线并陷入重连死循环。

CALLING NUMBER 在此 context 里只被塞进 `NODENUM` 打日志，可以不发。（`[iaxrpt]` context 不同：其配置注释明确要求主叫号码必须是 `<0>`，非 0 会出问题。）

接听后立即播放 "connected to node <号码>" 语音提示，之后才是正常的中继音频。

### 9.3 PTT

**IAX2 协议本身没有 PTT 概念。** PTT 语义完全在 app_rpt 应用层。

`Rpt()` 的模式选项决定按键方式：

| 模式 | 按键触发 |
|---|---|
| **`P`** PHONE_CONTROL | **DTMF `*6`**（`cop,6`，源码注释：*Simulate COR being activated (phone only)*）；`#` 松开。也接受 `AST_CONTROL_RADIO_KEY` / `_UNKEY` 控制帧 |
| **`D`** DUMB_DUPLEX（无 `v`） | **整通呼叫全程保持按下**（`app_rpt.c:7258-7259`）|
| **`S`** DUMB_SIMPLEX | DTMF：`*` 切换，`#` 松开 |
| 任意 + `v`/`V` | 能量 VOX（`dovox()`，带去抖）|

**语音帧的到达与否不会触发按键。** app_rpt 里确有基于帧到达的按键逻辑（`app_rpt.c:4701-4703`），但它以 `link_newkey == RADIO_KEY_NOT_ALLOWED` 为条件，而电话连接被显式排除（`app_rpt.c:7208-7212`）：

```c
l->link_newkey = RADIO_KEY_ALLOWED;
if ((phone_mode == RPT_PHONE_MODE_NONE) && ...) {
    l->link_newkey = RADIO_KEY_NOT_ALLOWED;
}
```

`phone_mode` 非 NONE（P/D/S）时保持 `RADIO_KEY_ALLOWED`，那条路径永不触发。**帧到达即按键是节点间互联的机制，不是电话门户的机制。**

> ⚠️ **`D` 模式的后果**：仅仅接入呼叫就会让中继发射机全程按下，一帧音频都不发也一样。一个「只收听」的客户端能否真的不发射，**取决于服务端 dialplan 用的是 `P` 还是 `D`**，客户端无法控制。ASL stock 用 `P`。

### 9.4 出向音频是持续推流

出向门控在 `app_rpt.c:4875-4895`，两个 `ast_write()` 分支。对一条从未按键的电话链路（`lastrx == 0`），无论 `altlink()` 取何值都必有一个分支命中；其中的 newkey 条件对电话链路恒真。帧来自 `RPT_CONF` 混音桥。

**出向路径上没有「RF 侧有没有人上话」的门控** —— 服务端按 50 帧/秒持续推流，没人上话时推的是静音或底噪。

**空闲时推的是精确的数字静音。** 实测：连续采样 1929 帧，其中 1644 帧全部是 `0xFF`（解码后全 0），空闲期的 RMS 中位数和 P95 均为 0.0；那 285 个非零帧对应的是接听后约 5.7 秒的语音提示。真实音频即使在词间停顿也是 RMS 100~600 的非零值，只有真正空闲才是精确的零。

这一点让基于能量的活动检测非常可靠 —— 但它是**这台服务端**的行为，别的节点可能发舒适噪声而非纯静音，阈值仍应可配。

推论：客户端要判断「现在有没有人上话」，**没有协议层信号可用**，只能自己算解码后音频的能量。`app_rpt.c:4647` 处的 `ast_indicate(AST_CONTROL_RADIO_KEY/_UNKEY)` 被 `phone_mode == RPT_PHONE_MODE_NONE` 守卫，**电话连接永远收不到这两个控制帧**。

### 9.5 文本协议

app_rpt 的链路文本协议是 `!NEWKEY!` / `!NEWKEY1!`（`app_rpt.h:495-496`）。

服务端对电话连接**从不发送**它们（每个 `send_newkey()` 调用点都有 `phone_mode == RPT_PHONE_MODE_NONE` 守卫）。

但接收侧的 `handle_link_data` 从 `AST_FRAME_TEXT` 分支进入（`app_rpt.c:4788-4795`），**没有电话模式守卫**。

> ⚠️ **客户端不要发 `!NEWKEY1!`**。它会把链路翻成 `RADIO_KEY_NOT_ALLOWED`，后果有二：启用帧到达即按键（可能意外按下发射机）；出向音频变成以 `l->lasttx` 为条件（会收不到音频）。

`!IAXKEY!` 是 iaxrpt 的遗留消息，app_rpt 解析后直接忽略（`app_rpt.c:1931-1934`）。

链路状态会以 `AST_FRAME_TEXT` 周期性推送（`"L "` 开头的节点列表，每 `linkpost_time` 秒一次，`app_rpt.c:3465-3487`）。那是链路状态，不是发射活动。

---

## 10. 未确认

以下条目缺乏可引用的来源，使用前需自行验证。

- **DVSwitch Mobile / iaxRpt 客户端实际发送的内容。** app_rpt 源码只说明它**接受**什么（DTMF `*6`/`#`、RADIO_KEY/UNKEY、能量 VOX），客户端侧行为不在该仓库内。
- **`qualify=yes` 是否会对未注册的 `host=dynamic` peer 发送 POKE。** 实测中未观测到 POKE，保活帧只出现过 LAGRQ（来自 chan_iax2 的每呼叫调度器，与 `qualify` 无关）。POKE 是 peer 级探测，携带 `dcallno=0`，不属于任何呼叫。

---

## 11. 调试

- **Wireshark 内置 IAX2 解析器**，能直接看出帧格式是否正确。

  ```bash
  sudo tcpdump -i any -n udp port 4569 -w iax.pcap
  ```

- 服务端若能上 Asterisk CLI：`iax2 set debug on` + `iax2 show channels`，可看到它对收到的帧的解读和拒绝原因。

- REJECT / HANGUP 帧里的 CAUSE（0x16，字符串）和 CAUSECODE（0x2A，u8）说明原因。**注意不含任何 IE 的 REJECT 是 CallToken 问题的特征**，见 §6。
