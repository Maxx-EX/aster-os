#!/usr/bin/env python3
"""抓开机动画的任意时刻：停表截图，保证拿到的是想要的帧。

用法：python3 tools/animshot.py 前缀 0.6 1.3 2.0
      会在 build/ 下生成 前缀-0.6.png 等。时间从发送 bootanim 那一刻算。
"""
import socket, subprocess, time, os, sys
from pathlib import Path
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
prefix = sys.argv[1] if len(sys.argv) > 1 else "anim"
marks = [float(x) for x in sys.argv[2:]] or [0.6, 1.3, 2.0]

MON = "/tmp/anim-mon.sock"
SPORT = 4491
if os.path.exists(MON):
    os.unlink(MON)

Q = ["qemu-system-aarch64", "-M", "virt", "-cpu", "cortex-a72", "-m", "512M",
     "-device", "ramfb", "-device", "virtio-keyboard-device", "-device", "virtio-mouse-device",
     "-display", "none",
     "-serial", f"tcp:127.0.0.1:{SPORT},server,nowait",
     "-monitor", f"unix:{MON},server,nowait",
     "-kernel", str(ROOT / "build/aster-boot.elf")]
p = subprocess.Popen(Q, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(9)

s = socket.create_connection(("127.0.0.1", SPORT), timeout=3)
s.settimeout(1.0)
m = socket.socket(socket.AF_UNIX)
for _ in range(40):
    try:
        m.connect(MON)
        break
    except OSError:
        time.sleep(0.3)

s.sendall(b"bootanim\n")
t0 = time.time()
stopped = 0.0
for t in marks:
    target = t0 + t + stopped
    wait = target - time.time()
    if wait > 0:
        time.sleep(wait)
    ppm = f"/tmp/anim-{t}.ppm"
    # 冻结再抓：ramfb 的 texture 是分块上传的，运行中直接抓会拿到撕裂帧
    t_stop = time.time()
    m.sendall(b"stop\n")
    time.sleep(0.15)
    m.sendall(b"screendump " + ppm.encode() + b"\n")
    time.sleep(0.9)
    m.sendall(b"cont\n")
    stopped += time.time() - t_stop
    Image.open(ppm).save(ROOT / "build" / f"{prefix}-{t}.png")
    print("saved", f"{prefix}-{t}.png")

# 最后等动画收尾，确认回到桌面
time.sleep(0.8)
ppm = "/tmp/anim-end.ppm"
m.sendall(b"stop\n"); time.sleep(0.15)
m.sendall(b"screendump " + ppm.encode() + b"\n"); time.sleep(0.9)
Image.open(ppm).save(ROOT / "build" / f"{prefix}-end.png")
print("saved", f"{prefix}-end.png")
m.close(); s.close(); p.terminate()
