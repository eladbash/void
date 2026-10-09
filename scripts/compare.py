#!/usr/bin/env python3
"""Put two screenshot runs side by side for review.

    scripts/compare.py <before-dir> <after-dir> <out-dir> [glob]

For every PNG present in both directories, writes <out-dir>/<name> with the
"before" image on the left and "after" on the right, at half size. macOS only
(uses AppKit, which ships with the system Python's pyobjc).
"""

import glob
import os
import sys

from AppKit import (
    NSBitmapImageRep,
    NSCompositingOperationCopy,
    NSImage,
    NSMakeRect,
    NSPNGFileType,
)


def side_by_side(a, b, out, scale=0.5, gap=16):
    ia = NSImage.alloc().initWithContentsOfFile_(a)
    ib = NSImage.alloc().initWithContentsOfFile_(b)
    ra, rb = ia.representations()[0], ib.representations()[0]
    wa, ha = ra.pixelsWide() * scale, ra.pixelsHigh() * scale
    wb, hb = rb.pixelsWide() * scale, rb.pixelsHigh() * scale
    h = max(ha, hb)
    canvas = NSImage.alloc().initWithSize_((wa + wb + gap, h))
    canvas.lockFocus()
    ia.drawInRect_fromRect_operation_fraction_(NSMakeRect(0, h - ha, wa, ha), NSMakeRect(0, 0, 0, 0), NSCompositingOperationCopy, 1.0)
    ib.drawInRect_fromRect_operation_fraction_(NSMakeRect(wa + gap, h - hb, wb, hb), NSMakeRect(0, 0, 0, 0), NSCompositingOperationCopy, 1.0)
    canvas.unlockFocus()
    rep = NSBitmapImageRep.imageRepWithData_(canvas.TIFFRepresentation())
    rep.representationUsingType_properties_(NSPNGFileType, None).writeToFile_atomically_(out, True)


def main():
    if len(sys.argv) < 4:
        print(__doc__)
        sys.exit(2)
    before, after, out = sys.argv[1:4]
    pattern = sys.argv[4] if len(sys.argv) > 4 else "*.png"
    os.makedirs(out, exist_ok=True)
    n = 0
    for a in sorted(glob.glob(os.path.join(before, pattern))):
        b = os.path.join(after, os.path.basename(a))
        if os.path.exists(b):
            side_by_side(a, b, os.path.join(out, os.path.basename(a)))
            n += 1
    print(f"{n} pairs")


if __name__ == "__main__":
    main()
