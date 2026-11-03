#!/bin/bash
set -e
curl -sSf https://mirrors.ustc.edu.cn/misc/rustup-install.sh | sh -s -- -y
. "$HOME/.cargo/env"
rustup install 1.84.0
rustup default 1.84.0
