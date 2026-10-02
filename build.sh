#!/bin/bash
# 同步源码到 ryzenSvr 并执行远程 cargo 命令
# 用法: ./build.sh check|build|build-win [远程参数透传]
set -e
LOCAL=/home/fila/jqdDev_2025/winsshd
REMOTE_HOST=ryzenSvr
REMOTE_DIR=/home/fila/jqdDev_2025/winsshd

rsync -az --delete --exclude target --exclude .git "$LOCAL/" "$REMOTE_HOST:$REMOTE_DIR/"
ssh "$REMOTE_HOST" "source $REMOTE_DIR/env.sh && cd $REMOTE_DIR && cargo $*"
