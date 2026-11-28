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

本次新增：
- PERF-4 离屏合成：每窗一块离屏层按 z 序 blit，三窗口 18ms → 2ms（单窗 1ms）
- SEC-2 组与目录权限：gid 进 inode、can() 认组位、chgrp、根目录 0o755 + list 查读位
- passwd 落盘：mkfs 自动写、useradd 写回、开机 load_from_disk() 恢复；新命令 passwd
