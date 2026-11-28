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

本次新增（0.9 第三批）：
- PERF-4 离屏合成：三窗口 18ms → 2ms
- PERF-5 只贴脏矩形：静止帧 0ms（零拷贝）、打字帧 0ms（只贴两行）
- SEC-2 组/目录权限/passwd 落盘（chgrp、根目录 0o755、mkfs 自动写 passwd、开机加载）
- SEC-3 setuid/setgid/sticky（chmod 4755 → -rwsr-xr-x、chmod 1755 → -rwxr-xr-t）
