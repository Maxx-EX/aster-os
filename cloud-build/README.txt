Aster（星芒）0.9 · 云端构建产物
====================
- aster-boot.elf   引导器 + 内核（QEMU virt，直接 -kernel 启动）
- aster-kernel.ast AsterFS/更新用的内核镜像

启动：qemu-system-aarch64 -M virt -cpu cortex-a72 -m 512M \
  -global virtio-mmio.force-legacy=false -device virtio-gpu-device \
  -device virtio-keyboard-device -device virtio-mouse-device \
  -kernel aster-boot.elf

新增：PERF-4 离屏合成（三窗 2ms）、SEC-2 组/目录权限/passwd 落盘。
