#!/bin/bash
# ryzenSvr 构建环境 (source 本文件后再跑 cargo)
export RUSTUP_HOME=/data/dev/rustup
export CARGO_HOME=/data/dev/cargo
export CARGO_TARGET_DIR=/data/dev/winsshd-target
export PATH=$CARGO_HOME/bin:$PATH
