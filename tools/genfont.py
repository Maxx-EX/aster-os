#!/usr/bin/env python3
"""把系统字体渲染成内核可用的点阵字模（生成 kernel/src/font.rs）

- ASCII 32..126：8x16 点阵，每字符 16 字节，bit7 在最左
- 界面用到的汉字：16x16 点阵，每字符 32 字节（16 行 x 2 字节）

生成物入库（体积小、可复现），这样别人构建时不需要装字体。
重新生成：python3 tools/genfont.py
"""
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ASCII_W, ASCII_H = 8, 16
CJK_PX = 16

ASCII_FONT_CANDIDATES = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
]
CJK_FONT_CANDIDATES = [
    "/usr/share/fonts/truetype/arphic/uming.ttc",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
]

# 界面上真正会出现的汉字（只生成这些，避免把几 MB 字库塞进内核）
CJK_CHARS = list("星芒内核任务调度内存状态运行远程图形界面启动系统版本校验原创值总共切换时间毫秒已就绪等待输入用户抢占输出心表轮转空闲个每当前进程独立页表网络串口终端显示大小")


def pick(cands):
    for c in cands:
        if Path(c).exists():
            return c
    raise SystemExit("找不到字体：" + " / ".join(cands))


def render(ch, font, w, h, baseline_ratio=0.8):
    """把单个字符渲染成 w x h 的二值点阵，返回 h 行、每行 ceil(w/8) 字节"""
    img = Image.new("L", (w, h), 0)
    d = ImageDraw.Draw(img)
    bbox = d.textbbox((0, 0), ch, font=font)
    tw = bbox[2] - bbox[0]
    th = bbox[3] - bbox[1]
    x = (w - tw) // 2 - bbox[0]
    y = int(h * baseline_ratio) - bbox[3]
    d.text((x, y), ch, fill=255, font=font)
    rows = []
    row_bytes = (w + 7) // 8
    for yy in range(h):
        ba = bytearray(row_bytes)
        for xx in range(w):
            if img.getpixel((xx, yy)) >= 110:
                ba[xx >> 3] |= 0x80 >> (xx & 7)
        rows.append(bytes(ba))
    return rows


def main():
    ascii_font = ImageFont.truetype(pick(ASCII_FONT_CANDIDATES), 14)
    cjk_font = ImageFont.truetype(pick(CJK_FONT_CANDIDATES), CJK_PX, index=0)

    out = []
    out.append("// 由 tools/genfont.py 生成，勿手改。")
    out.append("// ASCII: 8x16，每字符 16 字节；汉字: 16x16，每字符 32 字节。")
    out.append(f"pub const ASCII_W: usize = {ASCII_W};")
    out.append(f"pub const ASCII_H: usize = {ASCII_H};")
    out.append(f"pub const CJK_W: usize = {CJK_PX};")
    out.append(f"pub const CJK_H: usize = {CJK_PX};")
    out.append(f"pub const ASCII_FIRST: u8 = 32;")
    out.append(f"pub const ASCII_LAST: u8 = 126;")

    # ---- ASCII ----
    out.append("pub static ASCII: [[u8; 16]; 95] = [")
    for code in range(32, 127):
        ch = chr(code)
        rows = render(ch, ascii_font, ASCII_W, ASCII_H)
        flat = " ".join(f"0x{b:02x}," for r in rows for b in r)
        out.append(f"    [{flat}], // '{ch}'" if code != 32 else f"    [{flat}], // space")
    out.append("];")

    # ---- CJK ----
    cjk = sorted(set(CJK_CHARS))
    out.append(f"pub static CJK_CHARS: [char; {len(cjk)}] = [")
    out.append("    " + ", ".join(f"'{c}'" for c in cjk))
    out.append("];")
    out.append(f"pub static CJK_BITS: [[u8; 32]; {len(cjk)}] = [")
    for c in cjk:
        rows = render(c, cjk_font, CJK_PX, CJK_PX, baseline_ratio=0.85)
        flat = []
        for r in rows:
            flat.extend(r)
        out.append("    [" + " ".join(f"0x{b:02x}," for b in flat) + f"], // '{c}'")
    out.append("];")

    dst = Path(__file__).resolve().parent.parent / "kernel" / "src" / "font.rs"
    dst.write_text("\n".join(out) + "\n", encoding="utf-8")
    print(f"写出 {dst}：ASCII 95 个 + 汉字 {len(cjk)} 个")


if __name__ == "__main__":
    main()
