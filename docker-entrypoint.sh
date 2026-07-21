#!/bin/sh
# 让 iaxmon 自己解析配置并列出节点，再为每个节点启动一个子进程。
# 任一子进程退出时终止其余进程并以该退出码退出。
set -eu

CONFIG="${IAXMON_CONFIG:-/app/config.toml}"
STATUS_DIR=$(mktemp -d)

node_output=$(iaxmon --config "$CONFIG")
NODES=$(printf '%s\n' "$node_output" | sed -n 's/^  //p')
if [ -z "$NODES" ]; then
    echo "错误: $CONFIG 中没有可启动的节点" >&2
    exit 1
fi

PIDS=""

forward_signal() {
    for pid in $PIDS; do
        # 异步 shell 按 POSIX 会继承“忽略 SIGINT”；用 SIGTERM 唤醒监督子进程，
        # 再由它的 trap 向真正的 iaxmon 发送 SIGINT 以触发 IAX HANGUP。
        kill -TERM "$pid" 2>/dev/null || true
    done
}

cleanup() {
    rm -rf -- "$STATUS_DIR"
}

shutdown() {
    trap - INT TERM
    forward_signal
    set +e
    for pid in $PIDS; do
        wait "$pid" 2>/dev/null
    done
    cleanup
    exit 0
}
trap shutdown INT TERM

run_node() {
    node=$1
    index=$2
    shift 2
    child=""
    stopping=0
    stop_child() {
        stopping=1
        if [ -n "$child" ]; then
            kill -INT "$child" 2>/dev/null || true
        fi
    }
    trap stop_child INT TERM

    iaxmon --nats --config "$CONFIG" "$node" "$@" &
    child=$!
    set +e
    wait "$child"
    code=$?
    set -e
    if [ "$stopping" -eq 0 ]; then
        printf '%s\n' "$code" >"$STATUS_DIR/done-$index"
    fi
    exit "$code"
}

index=0
while IFS= read -r node; do
    run_node "$node" "$index" "$@" &
    PIDS="$PIDS $!"
    index=$((index + 1))
done <<EOF
$NODES
EOF

# POSIX sh 没有 wait -n。每个监督子进程在节点退出时写一个状态文件，父进程短轮询
# 这些文件，因此不依赖 PID 是否仍以 zombie 形式存在，也不会卡在第一个 PID 上。
while :; do
    for result in "$STATUS_DIR"/done-*; do
        if [ -f "$result" ]; then
            code=$(sed -n '1p' "$result")
            forward_signal
            set +e
            for remaining in $PIDS; do
                wait "$remaining" 2>/dev/null
            done
            set -e
            cleanup
            exit "$code"
        fi
    done
    sleep 0.2
done
