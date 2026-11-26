#!/usr/bin/env python3
"""启动 QEMU（带 ramfb），从 monitor 抓一张屏幕快照并存成 PNG

用法：
    python3 tools/screenshot.py out.png [等待秒数]

原理：QEMU 的 human monitor 有个 screendump 命令，会把当前显示内容写成 PPM。
这里用 unix socket 挂 monitor，等内核画完界面再抓。
"""
import socket
import subprocess
import sys
import time
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
BOOT = ROOT / "build" / "aster-boot.elf"
MON = "/tmp/aster-mon.sock"
PPM = "/tmp/aster-screen.ppm"

QEMU = [
    "qemu-system-aarch64",
    "-M", "virt",
    "-cpu", "cortex-a72",
    "-m", "512M",
    "-device", "ramfb",
    "-display", "none",
    "-serial", "file:/tmp/aster-serial.txt",
    "-monitor", f"unix:{MON},server,nowait",
    "-kernel", str(BOOT),
]


def main():
    out = Path(sys.argv[1] if len(sys.argv) > 1 else "aster-screen.png")
    wait = float(sys.argv[2]) if len(sys.argv) > 2 else 6.0

    for p in (MON, PPM):
        Path(p).unlink(missing_ok=True)

    proc = subprocess.Popen(QEMU, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        time.sleep(wait)
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(5)
        s.connect(MON)
        s.recv(4096)
        s.sendall(f"screendump {PPM}\n".encode())
        time.sleep(1.5)
        s.close()
        if not Path(PPM).exists():
            raise SystemExit("screendump 没生成文件，检查 QEMU 是否正常运行")
        Image.open(PPM).save(out)
        print(f"已保存 {out}")
    finally:
        proc.terminate()
        proc.wait(timeout=10)


if __name__ == "__main__":
    main()
