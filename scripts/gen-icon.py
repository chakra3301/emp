#!/usr/bin/env python3
"""EMP app icon: a system-font wordmark in EMP red on a white macOS-grid squircle.

Usage: python3 -I scripts/gen-icon.py [out.png]   (default design/icon/emp-icon.png, 1024x1024)
Then:  npx -w @emp/desktop tauri icon design/icon/emp-icon.png -o apps/desktop/src-tauri/icons
       python3 -I scripts/gen-icon.py --web   (full-bleed phone/home-screen icons in apps/desktop/public;
                                               iOS and Android round the corners themselves)
Font: EMP_ICON_FONT, defaulting to macOS Helvetica.
"""

import os
import sys

from PIL import Image, ImageDraw, ImageFilter, ImageFont

SIZE = 1024
BODY = 824  # Apple's icon grid: 824px body centred on a 1024 canvas
RADIUS = 185
RED = (232, 23, 63, 255)  # tokens.json color.accent #e8173f
FONT = os.environ.get("EMP_ICON_FONT", "/System/Library/Fonts/Helvetica.ttc")

def wordmark(img, width_ratio):
    draw = ImageDraw.Draw(img)
    size = 400
    font = ImageFont.truetype(FONT, size)
    l, t, r, b = draw.textbbox((0, 0), "EMP", font=font)
    size = int(size * (img.width * width_ratio) / (r - l))
    font = ImageFont.truetype(FONT, size)
    l, t, r, b = draw.textbbox((0, 0), "EMP", font=font)
    draw.text((img.width / 2 - (l + r) / 2, img.height / 2 - (t + b) / 2), "EMP", font=font, fill=RED)


if "--web" in sys.argv:
    public = os.path.join(os.path.dirname(__file__), "..", "apps", "desktop", "public")
    big = Image.new("RGBA", (SIZE, SIZE), (255, 255, 255, 255))
    wordmark(big, 0.6)  # inside the maskable safe zone (centre 80%)
    for name, px in (("apple-touch-icon.png", 180), ("icon-192.png", 192), ("icon-512.png", 512)):
        big.convert("RGB").resize((px, px), Image.LANCZOS).save(os.path.join(public, name))
        print(f"wrote apps/desktop/public/{name}")
    sys.exit(0)

out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "..", "design", "icon", "emp-icon.png")
os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)

off = (SIZE - BODY) // 2
box = (off, off, off + BODY, off + BODY)
img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))

shadow = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
ImageDraw.Draw(shadow).rounded_rectangle((box[0], box[1] + 12, box[2], box[3] + 12), RADIUS, fill=(0, 0, 0, 70))
img.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(18)))

draw = ImageDraw.Draw(img)
draw.rounded_rectangle(box, RADIUS, fill=(255, 255, 255, 255), outline=(0, 0, 0, 22), width=2)

# Fit the wordmark to ~74% of the body width, centred on its ink box.
size = 400
font = ImageFont.truetype(FONT, size)
l, t, r, b = draw.textbbox((0, 0), "EMP", font=font)
size = int(size * (BODY * 0.74) / (r - l))
font = ImageFont.truetype(FONT, size)
l, t, r, b = draw.textbbox((0, 0), "EMP", font=font)
x = SIZE / 2 - (l + r) / 2
y = SIZE / 2 - (t + b) / 2
draw.text((x, y), "EMP", font=font, fill=RED)

img.save(out)
print(f"wrote {out}")
