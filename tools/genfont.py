#!/usr/bin/env python3
"""把系统字体渲染成内核可用的点阵字模

输出三个文件（都入库，构建时不依赖本机字体）：
- kernel/src/font.rs    终端用：ASCII 8x16（JetBrains Mono）、汉字 16x16（Noto Sans CJK SC Medium）
- kernel/src/font_ui.rs 界面用：ASCII 10x16（Inter）
- kernel/src/icons.rs   侧栏线性图标：32x32 灰度

重新生成：python3 tools/genfont.py
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ASCII_W, ASCII_H = 8, 16
UI_W, UI_H = 10, 16
CJK_PX = 16

# DejaVu Sans Mono 的 p/g/y 在 8x16 里降部清楚；JetBrains Mono 的
# 双层 g 会被糊成一个 9，所以默认换掉，仍留作回退。
TERM_FONT = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/home/user/fonts/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/truetype/jetbrains/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
]
UI_FONT = [
    "/home/user/fonts/Inter.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
]
CJK_FONT = [
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Medium.ttc", 2),
    ("/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc", 0),
]

CJK_CHARS = list("·×—‖←→、。一三上下不与且东丢两个中串临为主举么之也习了事二于互些交人仅仍从他代令以们件价任份优会传伪但位低住体何余作使供依侧便保信借值偏做停像允充先光免兜入全共关其具兼内再写冲准减几出分切列则创初别到制刷刻削前剩力加务动助勿包化区十升协单占卡印即历原去参又反发取受变口另只可右号各合同名后向否含启命和哈响商喊嗓器回因固图在地场址坐坑块型基塞填壁处备复外多够大太失头套好如始子字存学它安完定实容宽宿寄密察对导射小少就尾局层屏属崩嵌工左己已布希带帧帮常幕平并广序库应底度建开异弃式引张弱强归当录形影往径待很得循必忙快态性总恒恢息恰意成我或户所扁手才打执扫批找把抢报抬拆拒拟拷拼拿持挂指按捕换据掉排接控描握搬搭摆摘播支收改放故效数整文断新方无旦旧时明易星映是显普暂替最有服未本机杀权束条来极构枚果某染柔查标栈栏校样核根格桌检槽模次止正此步死残段母每比毫气求汉没沿法注派流测浏浮消深混清渐渲源滚满演点然照片版物特状独率环现理生用由画界留登白的盖盘目直相看真着知短码硬确碎碰示离种秒积移稀程空穿窗立端符第等答简算管类粒粗系索累繁约级纯纸纹线组细终经绑结绘给络绝统继绪续缓编网置翻者而能自致色节芒若英获落蓝虚行表被装西要覆见观规视览角解触言计认让议记许设访证识诊译试询该语说请读调负责败费赖走起超越足跑跨路跳踩转轮软载辑输辨边达过运返还这进远连述退送选逐通造逻遍道避那部都配释里重量针钥钮钱链锁错键镜长闭问闲间阈队阱防阴阵阶阻限陷隔障零需静非靠面页顶项顺须预频题首驱验高默鼠齐（），：；？")

ROOT = Path(__file__).resolve().parent.parent


def source_cjk():
    chars = set()
    for pat in ("kernel/src/*.rs", "kernel/src/**/*.rs"):
        for f in ROOT.glob(pat):
            if f.name in ("font.rs", "font_ui.rs", "icons.rs"):
                continue
            for ch in f.read_text(encoding="utf-8", errors="ignore"):
                if ord(ch) > 0x7f and ch not in "\n\r\t":
                    chars.add(ch)
    return chars


def pick(cands):
    for c in cands:
        p = c if isinstance(c, str) else c[0]
        if Path(p).exists():
            return c
    raise SystemExit("找不到字体：" + str(cands))


def quantize(img, w, h):
    """灰度图 → 4-bit 点阵，每行 w/2 字节"""
    rows = []
    for yy in range(h):
        ba = bytearray(w // 2)
        for xx in range(w):
            v = img.getpixel((xx, yy))
            q = min(15, (v * 15 + 120) // 255)
            if xx % 2 == 0:
                ba[xx // 2] |= q << 4
            else:
                ba[xx // 2] |= q
        rows.append(bytes(ba))
    return rows


def render(ch, font, w, h, baseline):
    """定宽格子：按墨迹居中，基线对齐"""
    img = Image.new("L", (w, h), 0)
    d = ImageDraw.Draw(img)
    bbox = d.textbbox((0, 0), ch, font=font)
    tw = bbox[2] - bbox[0]
    x = (w - tw) // 2 - bbox[0]
    # 统一按基线对齐：以前用 bbox[3]（墨迹底）会按每个字的降部各自上移，
    # 结果 p/g/y 的降部被裁掉，p 看起来像 P、g 像 9。
    y = baseline - font.getmetrics()[0]
    d.text((x, y), ch, fill=255, font=font)
    return quantize(img, w, h)


def render_cjk(ch, font, w, h):
    """汉字：两倍画布按墨迹裁剪再居中"""
    big = Image.new("L", (w * 2, h * 2), 0)
    d = ImageDraw.Draw(big)
    d.text((w // 2, h // 2), ch, font=font, fill=255)
    bbox = big.getbbox()
    if not bbox:
        return [bytes(w // 2) for _ in range(h)]
    crop = big.crop(bbox)
    cw, chh = crop.size
    if cw > w or chh > h:
        sc = min(w / cw, h / chh)
        crop = crop.resize((max(1, int(cw * sc)), max(1, int(chh * sc))), Image.LANCZOS)
        cw, chh = crop.size
    out = Image.new("L", (w, h), 0)
    out.paste(crop, ((w - cw) // 2, (h - chh) // 2))
    return quantize(out, w, h)


# ---------------- 图标 ----------------

ICON_N = 32
ICONS = ["terminal", "cpu", "wifi", "globe"]


def icon_mask(name):
    """4 倍画布画线性图标（32 上合 2px 线宽），缩回 32x32"""
    S = ICON_N * 4
    img = Image.new("L", (S, S), 0)
    d = ImageDraw.Draw(img)
    lw = 8
    if name == "terminal":
        d.line([(S * 0.26, S * 0.32), (S * 0.46, S * 0.50), (S * 0.26, S * 0.68)], fill=255, width=lw, joint="curve")
        d.line([(S * 0.50, S * 0.68), (S * 0.72, S * 0.68)], fill=255, width=lw)
    elif name == "cpu":
        d.rounded_rectangle([S * 0.26, S * 0.26, S * 0.74, S * 0.74], radius=S * 0.06, outline=255, width=lw)
        d.rounded_rectangle([S * 0.41, S * 0.41, S * 0.59, S * 0.59], radius=S * 0.03, outline=255, width=lw)
        for i in (0.34, 0.5, 0.66):
            d.line([(S * i, S * 0.18), (S * i, S * 0.26)], fill=255, width=lw)
            d.line([(S * i, S * 0.74), (S * i, S * 0.82)], fill=255, width=lw)
            d.line([(S * 0.18, S * i), (S * 0.26, S * i)], fill=255, width=lw)
            d.line([(S * 0.74, S * i), (S * 0.82, S * i)], fill=255, width=lw)
    elif name == "wifi":
        cx, cy = S * 0.5, S * 0.64
        for r in (0.15, 0.28, 0.41):
            rr = S * r
            d.arc([cx - rr, cy - rr, cx + rr, cy + rr], 215, 325, fill=255, width=lw)
        d.ellipse([cx - S * 0.04, cy - S * 0.04, cx + S * 0.04, cy + S * 0.04], fill=255)
    elif name == "info":
        d.ellipse([S * 0.20, S * 0.20, S * 0.80, S * 0.80], outline=255, width=lw)
        d.line([(S * 0.5, S * 0.44), (S * 0.5, S * 0.66)], fill=255, width=lw)
        d.ellipse([S * 0.5 - S * 0.04, S * 0.5 - S * 0.20, S * 0.5 + S * 0.04, S * 0.5 - S * 0.12], fill=255)
    elif name == "globe":
        # 侧栏第 4 项是"浏览"，用地球而不是信息圆点
        d.ellipse([S * 0.18, S * 0.18, S * 0.82, S * 0.82], outline=255, width=lw)
        d.ellipse([S * 0.36, S * 0.18, S * 0.64, S * 0.82], outline=255, width=lw)
        d.line([(S * 0.18, S * 0.50), (S * 0.82, S * 0.50)], fill=255, width=lw)
    small = img.resize((ICON_N, ICON_N), Image.LANCZOS)
    return quantize(small, ICON_N, ICON_N)


def main():
    term_path = pick(TERM_FONT)
    # 13px 时 JetBrains Mono 的双层 g 在 8x16 里糊成一个 9；降到 12px 让
    # g/p/q/y 的下伸环有足够像素，字形才认得出。
    term_font = ImageFont.truetype(term_path, 11)
    baseline = ASCII_H - 3

    ui_path = pick(UI_FONT)
    ui_font = ImageFont.truetype(ui_path, 13)
    ui_ascent = ui_font.getmetrics()[0]

    cjk_path, cjk_idx = pick(CJK_FONT)
    cjk_font = ImageFont.truetype(cjk_path, CJK_PX, index=cjk_idx)

    out = []
    out.append("// 由 tools/genfont.py 生成，勿手改。")
    out.append("// ASCII: JetBrains Mono 8x16 灰度；汉字: Noto Sans CJK SC Medium 16x16 灰度。")
    out.append(f"pub const ASCII_W: usize = {ASCII_W};")
    out.append(f"pub const ASCII_H: usize = {ASCII_H};")
    out.append(f"pub const CJK_W: usize = {CJK_PX};")
    out.append(f"pub const CJK_H: usize = {CJK_PX};")
    out.append("pub const ASCII_FIRST: u8 = 32;")
    out.append("pub const ASCII_LAST: u8 = 126;")
    out.append("pub static ASCII: [[u8; 64]; 95] = [")
    for code in range(32, 127):
        ch = chr(code)
        rows = render(ch, term_font, ASCII_W, ASCII_H, baseline)
        flat = " ".join(f"0x{b:02x}," for r in rows for b in r)
        out.append(f"    [{flat}], // '{ch}'" if code != 32 else f"    [{flat}], // space")
    out.append("];")
    cjk = sorted(set(CJK_CHARS) | source_cjk())
    out.append(f"pub static CJK_CHARS: [char; {len(cjk)}] = [")
    out.append("    " + ", ".join(f"'{c}'" for c in cjk))
    out.append("];")
    out.append(f"pub static CJK_BITS: [[u8; 128]; {len(cjk)}] = [")
    for c in cjk:
        rows = render_cjk(c, cjk_font, CJK_PX, CJK_PX)
        flat = [b for r in rows for b in r]
        out.append("    [" + " ".join(f"0x{b:02x}," for b in flat) + f"], // '{c}'")
    out.append("];")
    (ROOT / "kernel/src/font.rs").write_text("\n".join(out) + "\n", encoding="utf-8")

    ui = []
    ui.append("// 由 tools/genfont.py 生成，勿手改。")
    ui.append("// 界面 ASCII：Inter 13px，10x16 灰度，每字符 80 字节。汉字取 font.rs 的 16x16。")
    ui.append(f"pub const UI_W: usize = {UI_W};")
    ui.append(f"pub const UI_H: usize = {UI_H};")
    ui.append("pub static UI_ASCII: [[u8; 80]; 95] = [")
    ub = min(UI_H - 3, max(ui_ascent, 12))
    for code in range(32, 127):
        ch = chr(code)
        rows = render(ch, ui_font, UI_W, UI_H, ub)
        flat = " ".join(f"0x{b:02x}," for r in rows for b in r)
        ui.append(f"    [{flat}], // '{ch}'" if code != 32 else f"    [{flat}], // space")
    ui.append("];")
    (ROOT / "kernel/src/font_ui.rs").write_text("\n".join(ui) + "\n", encoding="utf-8")

    ic = []
    ic.append("// 由 tools/genfont.py 生成，勿手改。")
    ic.append(f"pub const ICON_N: usize = {ICON_N};")
    ic.append(f"pub const ICON_COUNT: usize = {len(ICONS)};")
    ic.append(f"pub static ICONS: [[u8; {ICON_N * ICON_N // 2}]; {len(ICONS)}] = [")
    for name in ICONS:
        flat = [b for r in icon_mask(name) for b in r]
        ic.append("    [" + " ".join(f"0x{b:02x}," for b in flat) + f"], // {name}")
    ic.append("];")
    (ROOT / "kernel/src/icons.rs").write_text("\n".join(ic) + "\n", encoding="utf-8")

    print(f"font.rs   ASCII 95（{term_path.split('/')[-1]} 11px） + 汉字 {len(cjk)}（Noto SC Medium）")
    print("font_ui.rs ASCII 95（Inter 13px / 10x16）")
    print(f"icons.rs  {len(ICONS)} 个 32x32 线性图标")


if __name__ == "__main__":
    main()
