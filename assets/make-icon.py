"""Makes pocketmedia.ico, pocketmedia.res and the installer images from the logo.

The logo: a lavender circle (#AB93FF) with a dark bold P (#201533).
Run with Python 3 and Pillow:  python assets/make-icon.py
Needs Inter Bold (https://rsms.me/inter, SIL Open Font License); pass its path
as the first argument if it is not at the default Linux location.
"""
import io, struct, sys, os
from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
FONT = sys.argv[1] if len(sys.argv) > 1 else "/usr/share/fonts/opentype/inter/Inter-Bold.otf"
ACCENT, INK = (0xAB, 0x93, 0xFF, 255), (0x20, 0x15, 0x33, 255)
SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
VERSION = (1, 0, 0, 0)

def master(s=1024):
    im = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    d.ellipse([0, 0, s - 1, s - 1], fill=ACCENT)
    f = ImageFont.truetype(FONT, int(s * 0.74))
    bb = d.textbbox((0, 0), "P", font=f)
    w, h = bb[2] - bb[0], bb[3] - bb[1]
    # Optically centred: the bowl makes a P look right-heavy, so nudge it right a little.
    d.text(((s - w) / 2 - bb[0] + s * 0.02, (s - h) / 2 - bb[1]), "P", font=f, fill=INK)
    return im

def png_bytes(im):
    b = io.BytesIO(); im.save(b, "PNG"); return b.getvalue()

def dib_bytes(im):
    """32-bit BGRA DIB with an AND mask, the classic icon image format."""
    w, h = im.size
    hdr = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    px = im.tobytes("raw", "BGRA")
    rows = [px[y * w * 4:(y + 1) * w * 4] for y in range(h)][::-1]
    mask_row = ((w + 31) // 32) * 4
    return hdr + b"".join(rows) + b"\0" * mask_row * h

def images():
    m = master()
    out = []
    for s in SIZES:
        im = m.resize((s, s), Image.LANCZOS)
        out.append((s, png_bytes(im) if s >= 256 else dib_bytes(im)))
    return m, out

def ico(imgs):
    head = struct.pack("<HHH", 0, 1, len(imgs))
    off = 6 + 16 * len(imgs)
    dirs, data = b"", b""
    for s, b in imgs:
        dirs += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(b), off + len(data))
        data += b
    return head + dirs + data

def res_entry(rtype, name, data, lang=0x0409, flags=0x1030):
    def ident(v):
        return struct.pack("<HH", 0xFFFF, v)
    th = ident(rtype) + ident(name)
    hsize = 8 + len(th) + 16
    hdr = struct.pack("<II", len(data), hsize) + th + struct.pack("<IHHII", 0, flags, lang, 0, 0)
    pad = b"\0" * ((4 - len(data) % 4) % 4)
    return hdr + data + pad

def version_info():
    def s(t):
        return (t + "\0").encode("utf-16-le")
    def block(key, value=b"", vtype=1, children=b"", vlen=None):
        body = s(key)
        body += b"\0" * ((4 - (6 + len(body)) % 4) % 4)
        body += value
        if children:
            body += b"\0" * ((4 - (6 + len(body)) % 4) % 4)
        body += children
        length = 6 + len(body)
        if vlen is None:
            vlen = len(value) // 2 if vtype == 1 else len(value)
        out = struct.pack("<HHH", length, vlen, vtype) + body
        return out + b"\0" * ((4 - len(out) % 4) % 4)
    ver = ".".join(map(str, VERSION))
    strings = b"".join(block(k, s(v)) for k, v in [
        ("FileDescription", "PocketMedia"), ("FileVersion", ver), ("InternalName", "pocketmedia"),
        ("OriginalFilename", "pocketmedia.exe"), ("ProductName", "PocketMedia"), ("ProductVersion", ver)])
    table = block("040904b0", children=strings, vlen=0)
    sfi = block("StringFileInfo", children=table, vlen=0)
    var = block("Translation", struct.pack("<HH", 0x0409, 0x04B0), vtype=0)
    vfi = block("VarFileInfo", children=var, vlen=0)
    ms, ls = (VERSION[0] << 16) | VERSION[1], (VERSION[2] << 16) | VERSION[3]
    fixed = struct.pack("<13I", 0xFEEF04BD, 0x00010000, ms, ls, ms, ls, 0x3F, 0, 0x40004, 1, 0, 0, 0)
    return block("VS_VERSION_INFO", fixed, vtype=0, children=sfi + vfi)

def res(imgs):
    out = res_entry(0, 0, b"", lang=0, flags=0)  # the empty first entry every .res starts with
    group = struct.pack("<HHH", 0, 1, len(imgs))
    for i, (s, b) in enumerate(imgs, start=1):
        out += res_entry(3, i, b)  # RT_ICON
        group += struct.pack("<BBBBHHIH", s % 256, s % 256, 0, 0, 1, 32, len(b), i)
    out += res_entry(14, 1, group)  # RT_GROUP_ICON, id 1
    out += res_entry(16, 1, version_info(), flags=0x30)  # RT_VERSION
    return out

def bmp32(im):
    """32-bit BMP with straight (not premultiplied) alpha, for WizardImageAlphaFormat=defined."""
    w, h = im.size
    px = im.convert("RGBA").tobytes("raw", "BGRA")
    rows = b"".join(px[y * w * 4:(y + 1) * w * 4] for y in range(h - 1, -1, -1))
    info = struct.pack("<IiiHHIIiiII", 40, w, h, 1, 32, 0, len(rows), 2835, 2835, 0, 0)
    return struct.pack("<2sIHHI", b"BM", 14 + 40 + len(rows), 0, 0, 54) + info + rows

def wizard_image(m, size):
    im = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    logo = m.resize((int(size * 0.82),) * 2, Image.LANCZOS)
    o = (size - logo.size[0]) // 2
    im.paste(logo, (o, o), logo)
    return bmp32(im)

m, imgs = images()
open(os.path.join(HERE, "pocketmedia.ico"), "wb").write(ico(imgs))
open(os.path.join(HERE, "pocketmedia.res"), "wb").write(res(imgs))
m.resize((256, 256), Image.LANCZOS).save(os.path.join(HERE, "logo-256.png"))
# Inno's small wizard image (top right of each page): 55x58 at 100 %, bigger sizes for high DPI.
for scale, size in [(100, 58), (125, 72), (150, 87), (200, 116)]:
    open(os.path.join(HERE, f"wizard-small-{scale}.bmp"), "wb").write(wizard_image(m, size))
print("ok")
