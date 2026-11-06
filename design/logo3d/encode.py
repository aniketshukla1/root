#!/usr/bin/env python3
"""Downsample and average the captures, prove the loop closes, encode animated WebP.
usage: encode.py <raw dir> <out dir> <px> <ms per frame> <colour quality> <alpha quality>"""
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np
from PIL import Image

raw_dir, out_dir, px, ms, q, aq = sys.argv[1], Path(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5]), int(sys.argv[6])


def premultiplied(path):
    # Pillow premultiplies RGBA when it resizes, so glow edges stay clean. Averaging has to as well.
    a = np.asarray(Image.open(path).convert('RGBA').resize((px, px), Image.LANCZOS), dtype=np.float32) / 255
    return np.dstack((a[..., :3] * a[..., 3:], a[..., 3]))


for theme in ('dark', 'light'):
    groups = defaultdict(list)
    for f in sorted(Path(raw_dir, theme).glob('*.png')):
        groups[f.stem.split('-')[0]].append(f)
    means = [np.mean([premultiplied(f) for f in groups[k]], axis=0) for k in sorted(groups)]
    steps = [float(np.abs(means[i + 1] - means[i]).mean()) * 255 for i in range(len(means) - 1)]
    seam = float(np.abs(means[0] - means[-1]).mean()) * 255
    print(f'{theme}: {len(means)} frames, median step {np.median(steps):.2f}, largest {max(steps):.2f}, seam {seam:.2f}')
    assert seam <= max(steps), 'the loop does not close'
    images = []
    for m in means:
        alpha = m[..., 3:]
        rgb = np.where(alpha > 1e-4, m[..., :3] / np.maximum(alpha, 1e-4), 0)
        images.append(Image.fromarray((np.clip(np.dstack((rgb, alpha)), 0, 1) * 255 + 0.5).astype(np.uint8), 'RGBA'))
    out = out_dir / f'logo-3d-{theme}.webp'
    images[0].save(out, save_all=True, append_images=images[1:], duration=ms, loop=0, lossless=False, quality=q, alpha_quality=aq, method=6)
    print(f'{out}: {out.stat().st_size / 1048576:.2f} MB')
