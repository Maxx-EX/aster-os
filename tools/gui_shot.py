#!/usr/bin/env python3
"""带真鼠标输入的截图：Xvfb + QEMU GTK 窗口 + xdotool 注入。

用法：
  python3 tools/gui_shot.py out.png "m:40:200" "d:400:100:460:150" "c:40:330"
动作：m:x:y 移动 / c:x:y 单击 / d:x1:y1:x2:y2 从(x1,y1)按下拖到(x2,y2) / w 等待
"""
import subprocess, socket, time, os, sys, shutil
from pathlib import Path
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
out = ROOT / "build" / (sys.argv[1] if len(sys.argv) > 1 else "gui.png")
acts = sys.argv[2:]
DISP = ":99"
MON = "/tmp/gui-mon.sock"
PPM = "/tmp/gui-sd.ppm"
SPORT = 4491

if not os.path.exists("/tmp/.X11-unix/X99"):
    subprocess.Popen(["Xvfb", DISP, "-screen", "0", "1280x1024x24"],
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2)
env = dict(os.environ, DISPLAY=DISP)

for p in (MON, PPM):
    if os.path.exists(p):
        os.unlink(p)

Q = ["qemu-system-aarch64", "-M", "virt", "-cpu", "cortex-a72", "-m", "512M",
     "-device", "ramfb", "-device", "virtio-keyboard-device", "-device", "virtio-mouse-device",
     "-display", "gtk", "-serial", f"tcp:127.0.0.1:{SPORT},server,nowait",
     "-monitor", f"unix:{MON},server,nowait", "-kernel", str(ROOT / "build/aster-boot.elf")]
p = subprocess.Popen(Q, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(10)

wid = None
for _ in range(20):
    r = subprocess.run(["xdotool", "search", "--onlyvisible", "--class", "qemu"],
                       env=env, capture_output=True, text=True)
    ids = r.stdout.split()
    if ids:
        wid = ids[-1]
        break
    time.sleep(0.5)
print("window", wid)

def xd(*a):
    subprocess.run(["xdotool"] + [str(x) for x in a], env=env,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

if wid:
    xd("windowactivate", wid)
    time.sleep(0.5)

for a in acts:
    parts = a.split(":")
    k = parts[0]
    if k == "m" and wid:
        xd("mousemove", "--window", wid, parts[1], parts[2])
    elif k == "c" and wid:
        xd("mousemove", "--window", wid, parts[1], parts[2])
        time.sleep(0.3)
        xd("click", "1")
    elif k == "d" and wid:
        xd("mousemove", "--window", wid, parts[1], parts[2])
        time.sleep(0.25)
        xd("mousedown", "1")
        x1, y1, x2, y2 = map(int, parts[1:5])
        for i in range(1, 9):
            xd("mousemove", "--window", wid, x1 + (x2 - x1) * i // 8, y1 + (y2 - y1) * i // 8)
            time.sleep(0.12)
        xd("mouseup", "1")
    elif k == "w":
        time.sleep(float(parts[1]))
    time.sleep(0.6)

m = socket.socket(socket.AF_UNIX)
for _ in range(40):
    try:
        m.connect(MON); break
    except OSError:
        time.sleep(0.3)
m.sendall(b"stop\n")
time.sleep(0.4)
m.sendall(b"screendump " + PPM.encode() + b"\n")
time.sleep(1.2)
Image.open(PPM).save(out)
print("saved", out)
m.close()
p.terminate()
time.sleep(0.3)
