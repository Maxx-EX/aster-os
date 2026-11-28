#!/usr/bin/env python3
"""无头截图：启动 QEMU，可选挂磁盘、下发串口命令，再用 monitor 抓一帧。

用法：
  python3 tools/shot.py out.png                 # 只看桌面
  python3 tools/shot.py out.png disk ls         # 挂 build/aster.img 并依次执行命令
"""
import socket, subprocess, time, os, sys
from pathlib import Path
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
args = sys.argv[1:]
out = ROOT / "build" / (args[0] if args else "shot.png")
disk = "disk" in args
cmds = [a for a in args[1:] if a != "disk"]

MON = "/tmp/shot-mon.sock"
PPM = "/tmp/shot-sd.ppm"
SPORT = 4481
for p in (MON, PPM):
    if os.path.exists(p):
        os.unlink(p)

img_arg = []
if disk:
    img_arg = [
        "-global", "virtio-mmio.force-legacy=false",
        "-drive", "if=none,file=" + str(ROOT / "build/aster.img") + ",format=raw,id=d0,file.locking=off,cache=writethrough",
        "-device", "virtio-blk-device,drive=d0",
    ]
GPU = os.environ.get("GFX", "ramfb")
gpu_arg = (
    ["-global", "virtio-mmio.force-legacy=false", "-device", "virtio-gpu-device"]
    if GPU == "virtio"
    else ["-device", "ramfb"]
)
Q = ["qemu-system-aarch64", "-M", "virt", "-cpu", "cortex-a72", "-m", "512M"] + gpu_arg + img_arg + [
    "-device", "virtio-keyboard-device", "-device", "virtio-mouse-device",
    "-display", "none",
    "-serial", f"tcp:127.0.0.1:{SPORT},server,nowait",
    "-monitor", f"unix:{MON},server,nowait",
    "-kernel", str(ROOT / "build/aster-boot.elf"),
]
p = subprocess.Popen(Q, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(9)
s = socket.create_connection(("127.0.0.1", SPORT), timeout=3)
s.settimeout(1.5)
log = []
for c in cmds:
    s.sendall(c.encode() + b"\n")
    time.sleep(1.7)
    try:
        d = s.recv(65536)
        if d:
            log.append(d.decode("utf-8", "replace"))
    except socket.timeout:
        pass
if os.environ.get("LOG") and log:
    print("--- 串口 ---")
    print("".join(log))

m = socket.socket(socket.AF_UNIX)
for _ in range(40):
    try:
        m.connect(MON)
        break
    except OSError:
        time.sleep(0.3)
m.sendall(b"stop\n")
time.sleep(0.4)
m.sendall(b"screendump " + PPM.encode() + b"\n")
time.sleep(1.2)
Image.open(PPM).save(out)
print("saved", out)
m.close()
s.close()
p.terminate()
