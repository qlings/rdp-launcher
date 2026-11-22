# -*- coding: utf-8 -*-
"""生成 RDP 启动器图标：圆角蓝底 + 白色显示器 + 双向箭头。

输出：
  assets/app.ico       多尺寸（256/128/64/48/32/16）
  assets/app-256.png   窗口图标用
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
ASSETS.mkdir(exist_ok=True)

S = 1024


def vertical_gradient(size, top, bottom):
    img = Image.new("RGBA", (size, size))
    px = img.load()
    for y in range(size):
        t = y / (size - 1)
        r = round(top[0] + (bottom[0] - top[0]) * t)
        g = round(top[1] + (bottom[1] - top[1]) * t)
        b = round(top[2] + (bottom[2] - top[2]) * t)
        for x in range(size):
            px[x, y] = (r, g, b, 255)
    return img


def rounded_mask(size, radius, inset=0):
    mask = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(mask)
    d.rounded_rectangle(
        [inset, inset, size - 1 - inset, size - 1 - inset],
        radius=radius,
        fill=255,
    )
    return mask


base = vertical_gradient(S, (91, 141, 255), (30, 66, 199))

# 外侧留一点透明边，Windows 图标显得不闷
outer = rounded_mask(S, 220, inset=40)
canvas = Image.new("RGBA", (S, S), (0, 0, 0, 0))
canvas.paste(base, (0, 0), outer)

d = ImageDraw.Draw(canvas, "RGBA")

# 顶部高光
glow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
gd = ImageDraw.Draw(glow)
gd.ellipse([120, -260, 904, 460], fill=(255, 255, 255, 46))
glow = glow.filter(ImageFilter.GaussianBlur(90))
canvas.alpha_composite(Image.composite(glow, Image.new("RGBA", (S, S), (0, 0, 0, 0)), outer))

# 显示器外框
d.rounded_rectangle([238, 286, 786, 648], radius=54, outline=(255, 255, 255, 255), width=42)
# 屏幕内的深色底：白色箭头必须有足够对比
d.rounded_rectangle([274, 322, 750, 612], radius=30, fill=(14, 32, 88, 205))

# 支架 + 底座
d.rounded_rectangle([470, 648, 554, 706], radius=10, fill=(255, 255, 255, 255))
d.rounded_rectangle([376, 700, 648, 752], radius=24, fill=(255, 255, 255, 255))

# 上面一支箭头：本机 -> 远端（白）
d.line([(368, 412), (610, 412)], fill=(255, 255, 255, 255), width=34)
d.polygon([(600, 372), (664, 412), (600, 452)], fill=(255, 255, 255, 255))

# 下面一支箭头：远端 -> 本机（浅青）
cyan = (150, 232, 255, 255)
d.line([(656, 520), (414, 520)], fill=cyan, width=34)
d.polygon([(424, 480), (360, 520), (424, 560)], fill=cyan)

# 在线小绿点
d.ellipse([806, 128, 930, 252], fill=(34, 37, 46, 255))
d.ellipse([818, 140, 918, 240], fill=(61, 213, 138, 255))
d.ellipse([836, 158, 900, 222], fill=(120, 245, 180, 90))

# 轻微内阴影，让圆角更立体
edge = rounded_mask(S, 220, inset=40)
edge = edge.filter(ImageFilter.GaussianBlur(2))
# canvas.putalpha 保留原 alpha，不做整体裁剪

# 缩放到各尺寸
sizes = [256, 128, 64, 48, 32, 16]
frames = []
for s in sizes:
    frames.append(canvas.resize((s, s), Image.LANCZOS))

frames[0].save(ASSETS / "app-256.png")
frames[0].save(
    ASSETS / "app.ico",
    format="ICO",
    sizes=[(s, s) for s in sizes],
    append_images=frames[1:],
)
print("wrote", ASSETS / "app.ico", ASSETS / "app-256.png")
