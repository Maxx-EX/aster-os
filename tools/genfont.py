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
CJK_CHARS = list("·×—‖←→、。一三上下不与且东丢两个中串临为主举么之也习了事二于互些交人仅仍从他代令以们件价任份优会传伪但位低住体何余作使供依侧便保信借值偏做停像允充先光免兜入全共关其具兼内再写冲准减几出分切列则创初别到制刷刻削前剩力加务动助勿包化区十升协单占卡印即历原去参又反发取受变口另只可右号各合同名后向否含启命和哈响商喊嗓器回因固图在地场址坐坑块型基塞填壁处备复外多够大太失头套好如始子字存学它安完定实容宽宿寄密察对导射小少就尾局层屏属崩嵌工左己已布希带帧帮常幕平并广序库应底度建开异弃式引张弱强归当录形影往径待很得循必忙快态性总恒恢息恰意成我或户所扁手才打执扫批找把抢报抬拆拒拟拷拼拿持挂指按捕换据掉排接控描握搬搭摆摘播支收改放故效数整文断新方无旦旧时明易星映是显普暂替最有服未本机杀权束条来极构枚果某染柔查标栈栏校样核根格桌检槽模次止正此步死残段母每比毫气求汉没沿法注派流测浏浮消深混清渐渲源滚满演点然照片版物特状独率环现理生用由画界留登白的盖盘目直相看真着知短码硬确碎碰示离种秒积移稀程空穿窗立端符第等答简算管类粒粗系索累繁约级纯纸纹线组细终经绑结绘给络绝统继绪续缓编网置翻者而能自致色节芒若英获落蓝虚行表被装西要覆见观规视览角解触言计认让议记许设访证识诊译试询该语说请读调负责败费赖走起超越足跑跨路跳踩转轮软载辑输辨边达过运返还这进远连述退送选逐通造逻遍道避那部都配释里重量针钥钮钱链锁错键镜长闭问闲间阈队阱防阴阵阶阻限陷隔障零需静非靠面页顶项顺须预频题首驱验高默鼠齐（），：；？")


ROOT = Path(__file__).resolve().parent.parent


def source_cjk():
    """扫内核源码里真正出现过的非 ASCII 字符，免得文案一改就缺字"""
    chars = set()
    for pat in ("kernel/src/*.rs", "kernel/src/**/*.rs"):
        for f in ROOT.glob(pat):
            if f.name == "font.rs":
                continue
            for ch in f.read_text(encoding="utf-8", errors="ignore"):
                if ord(ch) > 0x7f and ch not in "\n\r\t":
                    chars.add(ch)
    return chars


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
    cjk = sorted(set(CJK_CHARS) | source_cjk())
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
