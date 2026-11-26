#!/usr/bin/env python3
"""端到端网络验证：宿主发起 TCP 连接 -> Aster 内核自己应答 -> 返回页面

同时挂着串口 TCP，方便在测试中间打 net 命令看收发计数。
用法：python3 tools/nettest.py [host_port] [serial_port]
"""
import socket
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

HOST_PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 28082
SER_PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 4445


def main():
    qemu = [
        "qemu-system-aarch64", "-M", "virt", "-cpu", "cortex-a72", "-m", "512M",
        # QEMU 的 virtio-mmio 默认 force-legacy=1，这台 6.2 上 legacy 队列
        # 不会真正被设备激活（DRIVER_OK 认了，used ring 永不推进）。
        # 一律用 modern（version=2），内核两种都支持。
        "-global", "virtio-mmio.force-legacy=false",
        "-device", "virtio-net-device,netdev=n0",
        "-netdev", f"user,id=n0,hostfwd=tcp::{HOST_PORT}-:80",
        "-device", "ramfb", "-display", "none",
        "-serial", f"tcp:127.0.0.1:{SER_PORT},server,nowait",
        "-monitor", "none",
        "-kernel", str(ROOT / "build" / "aster-boot.elf"),
    ]
    log = open("/tmp/nettest-qemu.log", "wb")
    proc = subprocess.Popen(qemu, stdout=log, stderr=subprocess.STDOUT)
    try:
        time.sleep(8.5)
        ser = socket.create_connection(("127.0.0.1", SER_PORT), timeout=6)
        ser.settimeout(3)
        time.sleep(0.5)
        ser.sendall(b"net\n")
        time.sleep(1.2)
        try:
            d = ser.recv(2000)
            print("[串口·连接前]")
            for l in d.decode(errors="replace").splitlines():
                if l.strip():
                    print("   ", l)
        except socket.timeout:
            pass

        print(f"[宿主] curl http://127.0.0.1:{HOST_PORT}/")
        r = subprocess.run(
            ["curl", "-s", "-m", "12", "-o", "/tmp/aster-page.html",
             "-w", "HTTP %{http_code}  %{size_download} bytes  %{time_total}s\n",
             f"http://127.0.0.1:{HOST_PORT}/"],
            capture_output=True, text=True,
        )
        print("   ", r.stdout.strip() or r.stderr.strip())

        time.sleep(0.5)
        ser.sendall(b"net\n")
        time.sleep(1.2)
        try:
            d = ser.recv(2000)
            print("[串口·连接后]")
            for l in d.decode(errors="replace").splitlines():
                if l.strip():
                    print("   ", l)
        except socket.timeout:
            print("    (无输出)")
        ser.close()

        p = Path("/tmp/aster-page.html")
        if p.exists() and p.stat().st_size > 100:
            head = p.read_text(errors="replace")[:300]
            print("[页面开头]", head.replace("\n", " ")[:200])
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            proc.kill()


if __name__ == "__main__":
    main()
