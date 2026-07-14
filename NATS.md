# Core NATS 音频发布协议

iaxmon 使用 Core NATS 把一条 IAX 音频源分发给多个 WebSocket Gateway。协议版本为
**1**，不使用 JetStream，不存储或回放历史音频。

## 启动与配置

```bash
iaxmon --nats
```

启用后，iaxmon 不初始化声卡，也不在本机播放。`config.toml` 必须包含：

```toml
[nats]
servers = [
  "nats://nats-1.internal:4222",
  "nats://nats-2.internal:4222",
  "nats://nats-3.internal:4222",
]
subject_prefix = "iaxmon.nodes.1999"
```

- `servers`：同一 NATS 集群的一个或多个初始入口，不能为空。连接任意入口后，客户端
  会接受集群通告并在节点故障时自动重连。
- `subject_prefix`：这个 IAX 音频源独占的 subject 根，不能含空白、`*` 或 `>`。
- 未使用 `--nats` 时 `[nats]` 可以省略，程序维持原来的声卡播放模式。
- 使用 `--nats` 而缺少 `[nats]` 时，程序在连接 IAX 之前直接退出并报错。

用户名密码认证：

```toml
[nats]
servers = ["nats://nats-1.internal:4222"]
subject_prefix = "iaxmon.nodes.1999"
username = "iaxmon"
password = "your-nats-password"
```

Token 认证：

```toml
[nats]
servers = ["nats://nats-1.internal:4222"]
subject_prefix = "iaxmon.nodes.1999"
token = "your-nats-token"
```

两种认证方式不能同时使用。TLS 可以使用 NATS URL 的 TLS scheme 和集群证书配置；
认证信息属于秘密，不应提交到 Git。

## Subjects

假设：

```toml
subject_prefix = "iaxmon.nodes.1999"
```

iaxmon 使用三个 subjects：

| Subject | 方向 | 用途 |
|---|---|---|
| `iaxmon.nodes.1999.audio` | iaxmon → Gateway | 实时有声 PCMU 二进制帧 |
| `iaxmon.nodes.1999.events` | iaxmon → Gateway | `state`、`start`、`stop` JSON 事件 |
| `iaxmon.nodes.1999.snapshot` | Gateway → iaxmon | Request/Reply 查询当前状态 |

每个 Gateway 必须使用普通 Core NATS subscription 独立订阅 `audio` 和 `events`。
**不要把 Gateway 放进同一个 queue group**；queue group 会让每帧只交给其中一个
Gateway，导致其他 Gateway 的浏览器音频残缺。

## 音频参数

| 属性 | 值 |
|---|---|
| 编码 | G.711 μ-law / PCMU |
| 采样率 | 8000 Hz |
| 声道 | 单声道 |
| IAX 帧长 | 通常为 20 ms / 160 字节 |
| 字节序 | 多字节整数均为 big-endian |

iaxmon 不转码，直接发布 IAX2 中的 μ-law payload。Gateway 或浏览器负责 μ-law 解码、
重采样、100 ms 抖动缓冲和静音填充。

## 二进制音频消息

`<subject_prefix>.audio` 的 payload：

| 偏移 | 长度 | 字段 | 说明 |
|---:|---:|---|---|
| 0 | 1 | `version` | 固定为 `1` |
| 1 | 1 | `message_type` | 固定为 `1`，表示音频 |
| 2 | 4 | `sequence` | 发布序号，`u32` |
| 6 | 4 | `timestamp` | IAX 媒体时间戳（毫秒），`u32` |
| 10 | 其余 | `payload` | 原始 PCMU 数据 |

通常一帧为 170 字节。`sequence` 在 iaxmon 进程生命周期内递增并按 `u32` 回绕；它只
对准备发布的有声帧计数。序号出现间隔表示 iaxmon 的实时发布队列曾丢帧，Gateway
应填静音而不是等待重传。

`timestamp` 来自当前 IAX 呼叫的媒体时钟。新呼叫可能重新从较小值开始；Gateway 在
收到 `online=false` 后必须清除旧呼叫的时间戳和播放基准。

## 事件消息

`<subject_prefix>.events` 使用 UTF-8 JSON。

IAX 握手完成或连接断开：

```json
{"type":"state","online":true,"speaking":false}
```

- `online=true`：IAX 握手已完成，呼叫正在运行。
- `online=false`：尚未连接、正在重连或呼叫已经结束。
- `speaking`：活动检测器当前是否认为有人上话。

第一帧有声数据之前：

```json
{"type":"start","timestamp":12340}
```

连续静音达到 `activity.hang_ms` 后：

```json
{"type":"stop","timestamp":18120,"duration_ms":5600}
```

`duration_ms` 从第一帧有声数据算到最后一帧有声数据，不包含用于判定结束的末尾
hang time。

## 状态快照

Core NATS 不保留历史事件，所以新启动的 Gateway 应当：

1. 先订阅 `<subject_prefix>.audio` 和 `<subject_prefix>.events`。
2. 向 `<subject_prefix>.snapshot` 发起 NATS Request。
3. 从 Reply 收取当前完整 `state` JSON。

快照回复示例：

```json
{"type":"state","online":true,"speaking":true}
```

先订阅再请求可以避免在建立 Gateway 状态期间漏掉新的实时事件。如果当前正在上话，
Gateway 不会获得之前的音频历史，从下一条实时音频开始转发即可。

## 静音和断线行为

iaxmon 仍持续接收 AllStarLink 的上游静音流，但低于 `activity.threshold` 的帧不会进入
NATS：

```text
IAX 静音帧 → 活动检测 → 丢弃
IAX 有声帧 → 活动检测 → Core NATS
```

短暂停顿不会产生新的 `start`/`stop`，其中的静音帧仍不发布。Gateway/浏览器根据媒体
时间戳在缺失位置填零。

NATS 连接断开时，iaxmon 会继续保持 IAX 呼叫，但丢弃断线期间的实时音频，不在重连后
补发陈旧内容。NATS 客户端会在配置的集群节点之间自动重连。

音频发布经过一个有界实时队列。NATS 或本机调度跟不上时，队列满后丢弃新帧并在日志
中报告累计数量；不会阻塞 IAX 接收，也不会无限堆积延迟。

## Gateway 扩容约定

推荐部署：

```text
AllStarLink
    │ 一条 IAX
    ▼
iaxmon --nats（唯一 active publisher）
    │
    ▼
Core NATS cluster
    ├── Gateway 1 → browsers
    ├── Gateway 2 → browsers
    └── Gateway N → browsers
```

- 同一个 `subject_prefix` 同时只能有一个 active iaxmon publisher，否则会产生重复音频。
- 多个 Gateway 各自接收完整流，再由 Nginx 对浏览器 WebSocket 做负载均衡。
- Gateway 可以任意扩缩容，不会增加 IAX 连接数。
- 如果需要 publisher 高可用，应部署 standby 和 leader election；standby 未成为 leader
  前不能建立 IAX 呼叫或向同一 subject 发布。
