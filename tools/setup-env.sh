#!/bin/sh
# 云主机被回收后的一键恢复：apt 源、交叉工具链、QEMU。
# 用法： sh tools/setup-env.sh
set -e
sudo rm -f /etc/apt/sources.list.d/nodesource.sources
sudo sed -i 's|http://archive.ubuntu.com/ubuntu/|https://mirrors.tuna.tsinghua.edu.cn/ubuntu/|g; s|http://security.ubuntu.com/ubuntu/|https://mirrors.tuna.tsinghua.edu.cn/ubuntu/|g' /etc/apt/sources.list
sudo rm -rf /var/lib/apt/lists/partial
sudo apt-get update
sudo apt-get install -y gcc-aarch64-linux-gnu binutils-aarch64-linux-gnu qemu-system-arm
which aarch64-linux-gnu-gcc aarch64-linux-gnu-objcopy qemu-system-aarch64
