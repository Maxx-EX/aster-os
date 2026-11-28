Aster（星芒）0.9 · Termux 本地跑
====================

1) 装 QEMU：
   pkg install qemu-system-aarch64-headless

2) 启动（无盘）：
   qemu-system-aarch64 -M virt -cpu cortex-a72 -m 512M \
     -global virtio-mmio.force-legacy=false \
     -device virtio-gpu-device -device virtio-keyboard-device -device virtio-mouse-device \
     -kernel aster-boot.elf

3) 带盘（AsterFS 要盘）：
   qemu-system-aarch64 ... -device virtio-blk-device,drive=d0 -drive if=none,id=d0,file=disk.img,format=raw

4) 进系统后先解锁：
   ex → dl unlock 9337BDCD → fmt → mkfs

本次新增（0.9 第五批）：
- WIFI-1 骨架：board 加 EMMC 基址（Pi3 0x3F300000 / Pi4 0xFE340000）、sdhci.rs（CAPABILITIES 探测/软复位/时钟/CMD/CMD5）、wifi.rs（后端+SDIO 探测+扫描+四步进度）；QEMU 上如实输出"没有 SDHCI 控制器 / 0 个 AP"，不编造热点
- PKG-3 真实包体：`pkg install <名> <url>` 走 HTTP 把包体拉下来写盘（144 字节 git.ase 实测）
- 通用下载：`fetch <host[:port]/path> <名字>`，实测 4499 字节 logo 45ms 落盘
- AsterFS 多块写：单文件 4 KiB → 40 KiB（NDIRECT=10 循环），vfs 打开缓冲 4 KiB → 16 KiB
- APK-23x 核对完毕：AOSP 表 AA|op CC|BB（低字节 vB、高字节 vC）与内核读法一致，旧疑点作废

0.9 第四批（保留）：
- NET-4 DHCP 客户端：手写 BOOTP，开机 DISCOVER→OFFER→REQUEST→ACK，1–2ms 拿地址；`dhcp` 命令
- NET-4 DNS：查 A 记录（`dns example.com`），`web neverssl.com` 直接拉公网页面
- 修 TX 描述符泄漏：发包几十个之后 TX 环会空，旧版 `web` 会 panic
- 跨网段按网关 MAC 转发（route_mac）

0.9 第三批（保留）：
- PERF-4 离屏合成：三窗口 18ms → 2ms
- PERF-5 只贴脏矩形：静止帧 0ms（零拷贝）、打字帧 0ms（只贴两行）
- SEC-2 组/目录权限/passwd 落盘（chgrp、根目录 0o755、mkfs 自动写 passwd、开机加载）
- SEC-3 setuid/setgid/sticky（chmod 4755 → -rwsr-xr-x、chmod 1755 → -rwxr-xr-t）
