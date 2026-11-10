"""Generates assets/wispshot.ico (16..256 px PNG frames) with the same mark as the tray
icon (src/logo.rs) and assets/logo.svg. Pure stdlib: python -I tools/make_icon.py"""
import os, struct, zlib

C0, C1 = 0x0EA5E9, 0x8B5CF6
LO, HI, R = 4.0, 60.0, 15.0
START = (14.0, 25.0)
WAVE = [[(17, 40), (21, 44), (25, 37)], [(28, 31), (36, 31), (39, 37)], [(43, 44), (47, 40), (50, 25)]]
WIDTH = 6.0
SPARK = (32.0, 18.0, 3.6)


def wave_points():
    pts, p0 = [START], START
    for c1, c2, p3 in WAVE:
        for i in range(1, 17):
            t = i / 16; u = 1 - t
            pts.append(tuple(u**3 * p0[j] + 3 * u * u * t * c1[j] + 3 * u * t * t * c2[j] + t**3 * p3[j] for j in (0, 1)))
        p0 = p3
    return pts


def seg_dist(p, a, b):
    dx, dy = b[0] - a[0], b[1] - a[1]
    l2 = dx * dx + dy * dy
    t = max(0.0, min(1.0, ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2)) if l2 else 0.0
    return ((a[0] + t * dx - p[0]) ** 2 + (a[1] + t * dy - p[1]) ** 2) ** 0.5


def render(n, wave=wave_points()):
    ss, k = 4, 64 / n
    c, h = (LO + HI) / 2, (HI - LO) / 2
    segs = list(zip(wave, wave[1:]))
    ww, sr = (8.0, 4.6) if n <= 24 else (WIDTH, SPARK[2])  # bolder at tray sizes
    rows = bytearray()
    for y in range(n):
        rows.append(0)
        for x in range(n):
            bg = fg = 0
            for sy in range(ss):
                for sx in range(ss):
                    u = (x + (sx + .5) / ss) * k; v = (y + (sy + .5) / ss) * k
                    dx = max(abs(u - c) - (h - R), 0); dy = max(abs(v - c) - (h - R), 0)
                    if (dx * dx + dy * dy) ** .5 > R:
                        continue
                    bg += 1
                    if any(seg_dist((u, v), a, b) <= ww / 2 for a, b in segs) or ((u - SPARK[0]) ** 2 + (v - SPARK[1]) ** 2) ** .5 <= sr:
                        fg += 1
            if bg == 0:
                rows += b'\0\0\0\0'; continue
            u, v = (x + .5) * k, (y + .5) * k
            t = max(0.0, min(1.0, ((u - 6) - (v - 58)) / 104))
            base = [int(((C0 >> s) & 255) + (((C1 >> s) & 255) - ((C0 >> s) & 255)) * t) for s in (16, 8, 0)]
            mix = lambda b_: (b_ * (bg - fg) + 255 * fg) // bg
            rows += bytes([mix(base[0]), mix(base[1]), mix(base[2]), bg * 255 // (ss * ss)])

    def chunk(t, d):
        return struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', n, n, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(bytes(rows), 9)) + chunk(b'IEND', b'')


sizes = [16, 20, 24, 32, 40, 48, 64, 256]
imgs = [render(s) for s in sizes]
out = struct.pack('<HHH', 0, 1, len(sizes)); off = 6 + 16 * len(sizes)
for s, d in zip(sizes, imgs):
    out += struct.pack('<BBBBHHII', s % 256, s % 256, 0, 0, 1, 32, len(d), off); off += len(d)
here = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'assets')
open(os.path.join(here, 'wispshot.ico'), 'wb').write(out + b''.join(imgs))
open(os.path.join(here, 'logo-256.png'), 'wb').write(imgs[-1])
