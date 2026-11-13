"""Converts Haiku Code's icon (assets/icon.svg) to a Haiku vector icon (HVIF).

usage: uv run --with skia-python --with svgelements python -I haiku/mkicon.py
           [--preview out.png] [--verify out.png]
       (rewrites haiku/haikucode.hvif; --preview renders the shapes that went
       in, --verify decodes the bytes just written and renders those, which is
       what catches a bad coordinate or gradient matrix before the VM does)

HVIF has fills and linear/radial gradients but no filters, masks or clipping,
so strokes become filled outlines and everything ends up as cubic curves on
the 64x64 HVIF grid. The icon is deliberately plain — a rounded tile, a
chevron stroke and a rounded rectangle — so nothing is lost.

The 64x64 grid and byte layout follow the format Haiku's Icon-O-Matic writes;
the encoder is the one that ships Temino's icon, so a working sibling icon is
the reference for anything that looks off here.
"""
import math
import os
import re
import struct
import sys
import xml.etree.ElementTree as ET

import skia
import svgelements

SVG = "{http://www.w3.org/2000/svg}"
SCALE = 64.0 / 512.0
INHERITED = ("fill", "stroke", "stroke-width", "stroke-linecap", "stroke-linejoin",
             "stroke-miterlimit", "fill-opacity", "stroke-opacity", "fill-rule")

here = os.path.dirname(os.path.abspath(__file__))
root = ET.parse(os.path.join(here, "..", "assets", "icon.svg")).getroot()
ids = {el.get("id"): el for el in root.iter() if el.get("id")}


def parse_color(text):
    text = text.strip()
    if text.startswith("#"):
        h = text[1:]
        if len(h) == 3:
            h = "".join(c * 2 for c in h)
        return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))
    c = svgelements.Color(text)
    return (c.red, c.green, c.blue)


def ref(text):
    m = re.match(r"url\(#(.+)\)", text or "")
    return ids[m.group(1)] if m else None


def outline(el):
    """The element's geometry as a skia path (no transforms in this SVG)."""
    tag = el.tag[len(SVG):]
    if tag == "rect":
        x, y = float(el.get("x") or 0), float(el.get("y") or 0)
        w, h = float(el.get("width")), float(el.get("height"))
        rx = float(el.get("rx") or el.get("ry") or 0)
        ry = float(el.get("ry") or el.get("rx") or 0)
        if rx or ry:
            return skia.Path().addRRect(skia.RRect.MakeRectXY(skia.Rect.MakeXYWH(x, y, w, h), rx, ry))
        return skia.Path().addRect(skia.Rect.MakeXYWH(x, y, w, h))
    if tag == "ellipse":
        cx, cy, rx, ry = (float(el.get(k)) for k in ("cx", "cy", "rx", "ry"))
        return skia.Path().addOval(skia.Rect.MakeLTRB(cx - rx, cy - ry, cx + rx, cy + ry))
    path = skia.Path()
    for seg in svgelements.Path(el.get("d")):
        if isinstance(seg, svgelements.Move):
            path.moveTo(seg.end.x, seg.end.y)
        elif isinstance(seg, svgelements.Close):
            path.close()
        elif isinstance(seg, svgelements.Line):
            path.lineTo(seg.end.x, seg.end.y)
        elif isinstance(seg, svgelements.QuadraticBezier):
            path.quadTo(seg.control.x, seg.control.y, seg.end.x, seg.end.y)
        elif isinstance(seg, svgelements.CubicBezier):
            path.cubicTo(seg.control1.x, seg.control1.y, seg.control2.x, seg.control2.y,
                         seg.end.x, seg.end.y)
        elif isinstance(seg, svgelements.Arc):
            for c in seg.as_cubic_curves():
                path.cubicTo(c.control1.x, c.control1.y, c.control2.x, c.control2.y,
                             c.end.x, c.end.y)
    return path


def stroked(path, attrs):
    paint = skia.Paint(Style=skia.Paint.kStroke_Style,
                       StrokeWidth=float(attrs.get("stroke-width", "1")))
    paint.setStrokeCap({"round": skia.Paint.kRound_Cap, "square": skia.Paint.kSquare_Cap}
                       .get(attrs.get("stroke-linecap"), skia.Paint.kButt_Cap))
    paint.setStrokeJoin({"round": skia.Paint.kRound_Join, "bevel": skia.Paint.kBevel_Join}
                        .get(attrs.get("stroke-linejoin"), skia.Paint.kMiter_Join))
    paint.setStrokeMiter(float(attrs.get("stroke-miterlimit", "4")))
    out = skia.Path()
    paint.getFillPath(path, out)
    return out


def paint_style(paint, alpha):
    """An HVIF style for a fill: ("solid", rgba) or ("gradient", ...)."""
    grad = ref(paint)
    if grad is None:
        return ("solid", parse_color(paint) + (round(255 * alpha),))
    get = grad.get
    assert get("gradientUnits") == "userSpaceOnUse" and not get("gradientTransform")
    out = []
    for stop in grad.iter(SVG + "stop"):
        style = dict(kv.split(":") for kv in (stop.get("style") or "").split(";") if ":" in kv)
        off = stop.get("offset", "0")
        off = float(off[:-1]) / 100 if off.endswith("%") else float(off)
        color = parse_color(style.get("stop-color", stop.get("stop-color", "#000")))
        a = float(style.get("stop-opacity", stop.get("stop-opacity", "1")))
        out.append((round(255 * min(max(off, 0), 1)), color + (round(255 * a * alpha),)))
    if grad.tag == SVG + "linearGradient":
        x1, y1, x2, y2 = (float(get(k)) * SCALE for k in ("x1", "y1", "x2", "y2"))
        # HVIF's linear gradient runs along x from -64 to 64.
        dx, dy = (x2 - x1) / 128, (y2 - y1) / 128
        matrix = (dx, dy, -dy, dx, (x1 + x2) / 2, (y1 + y2) / 2)
        return ("gradient", 0, matrix, tuple(out))
    cx, cy, r = (float(get(k)) * SCALE for k in ("cx", "cy", "r"))
    # HVIF's circular gradient has radius 64 around the origin.
    return ("gradient", 1, (r / 64, 0, 0, r / 64, cx, cy), tuple(out))


def walk(el, attrs, alpha, clip, out):
    attrs = dict(attrs)
    for k in INHERITED:
        if el.get(k) is not None:
            attrs[k] = el.get(k)
    alpha *= float(el.get("opacity", "1"))
    if el.get("filter") or el.get("mask"):
        return
    if el.get("clip-path"):
        c = skia.Path()
        for child in ref(el.get("clip-path")):
            c = skia.Op(c, outline(child), skia.PathOp.kUnion_PathOp)
        clip = c if clip is None else skia.Op(clip, c, skia.PathOp.kIntersect_PathOp)
    tag = el.tag[len(SVG):]
    if tag in ("g", "svg"):
        for child in el:
            walk(child, attrs, alpha, clip, out)
        return
    if tag not in ("path", "rect", "ellipse"):
        return
    assert not el.get("transform") and not (el.tag == SVG + "g" and el.get("transform")), \
        "transforms are not handled: bake them into the geometry"
    path = outline(el)
    if attrs.get("fill-rule") == "evenodd":
        path.setFillType(skia.PathFillType.kEvenOdd)
    layers = []
    if attrs.get("fill", "#000") != "none":
        layers.append((path, attrs.get("fill", "#000"), float(attrs.get("fill-opacity", "1"))))
    if attrs.get("stroke", "none") != "none":
        layers.append((stroked(path, attrs), attrs["stroke"],
                       float(attrs.get("stroke-opacity", "1"))))
    for geometry, paint, opacity in layers:
        if clip is not None:
            geometry = skia.Op(geometry, clip, skia.PathOp.kIntersect_PathOp)
        # Non-overlapping contours fill the same under even-odd and non-zero.
        geometry = skia.Simplify(geometry)
        if geometry.isEmpty():
            continue
        out.append((paint_style(paint, alpha * opacity), geometry))


def contours(path):
    """Splits a path into closed contours of HVIF vertices (point, in, out)."""
    result, verts = [], []
    path = skia.Path(path)
    path.transform(skia.Matrix.Scale(SCALE, SCALE))
    it = skia.Path.Iter(path, False)

    def cubic(c1, c2, end):
        verts[-1][2] = c1
        verts.append([end, c2, end])

    def quad(a, b, c):
        cubic((a[0] + 2 / 3 * (b[0] - a[0]), a[1] + 2 / 3 * (b[1] - a[1])),
              (c[0] + 2 / 3 * (b[0] - c[0]), c[1] + 2 / 3 * (b[1] - c[1])), c)

    while True:
        verb, pts = it.next()
        p = [(q.x(), q.y()) for q in pts]
        if verb == skia.Path.Verb.kDone_Verb:
            break
        if verb == skia.Path.Verb.kMove_Verb:
            verts = [[p[0], p[0], p[0]]]
            result.append(verts)
        elif verb == skia.Path.Verb.kLine_Verb:
            verts.append([p[1], p[1], p[1]])
        elif verb == skia.Path.Verb.kQuad_Verb:
            quad(*p)
        elif verb == skia.Path.Verb.kConic_Verb:
            q = [(v.x(), v.y()) for v in
                 skia.Path.ConvertConicToQuads(*pts, it.conicWeight(), 2)]
            for i in range(0, len(q) - 2, 2):
                quad(q[i], q[i + 1], q[i + 2])
        elif verb == skia.Path.Verb.kCubic_Verb:
            cubic(p[1], p[2], p[3])
    out = []
    for verts in result:
        # The closing vertex repeats the first: fold its "in" into the first.
        if len(verts) > 1 and math.dist(verts[-1][0], verts[0][0]) < 1e-6:
            verts[0][1] = verts.pop()[1]
        if len(verts) >= 2:
            out.append(verts)
    return out


def coord(v):
    n = round((v + 128.0) * 102.0)
    assert 0 <= n < 0x8000, v
    return bytes([0x80 | (n >> 8), n & 0xff])


def float24(v):
    if v == 0:
        return bytes(3)
    bits = struct.unpack(">I", struct.pack(">f", v))[0]
    exponent = ((bits >> 23) & 0xff) - 127 + 32
    assert 0 < exponent < 64, v
    value = (bits >> 31) << 23 | exponent << 17 | (bits & 0x7fffff) >> 6
    return value.to_bytes(3, "big")


def unfloat24(b):
    value = int.from_bytes(b, "big")
    sign = (value >> 23) & 1
    exponent = ((value >> 17) & 0x3f) - 32
    mantissa = value & 0x1ffff
    if value == 0:
        return 0.0
    f = (1 + mantissa / float(1 << 17)) * 2.0 ** exponent
    return -f if sign else f


def style_bytes(style):
    if style[0] == "solid":
        return bytes([0x01, *style[1]])                 # solid color
    _, kind, matrix, stops = style
    out = bytearray([0x02, kind, 0x02, len(stops)])     # gradient, has transform
    for v in matrix:
        out += float24(v)
    for offset, rgba in stops:
        out += bytes([offset, *rgba])
    return bytes(out)


def hvif(items):
    styles, paths, shapes = [], [], []
    for style, geometry in items:
        if style not in styles:
            styles.append(style)
        first = len(paths)
        for verts in contours(geometry):
            assert len(verts) < 256
            paths.append(verts)
        shapes.append((styles.index(style), list(range(first, len(paths)))))
    assert len(styles) < 256 and len(paths) < 256 and len(shapes) < 256
    out = bytearray(b"ncif")
    out.append(len(styles))
    for style in styles:
        out += style_bytes(style)
    out.append(len(paths))
    for verts in paths:
        out += bytes([0x02, len(verts)])                # closed, with curves
        for point in verts:
            for x, y in point:
                out += coord(x) + coord(y)
    out.append(len(shapes))
    for style, members in shapes:
        out += bytes([0x0a, style, len(members), *members, 0x00])
    print(f"{len(styles)} styles, {len(paths)} paths, {len(shapes)} shapes")
    return bytes(out)


def decode(data):
    """Reads an HVIF back into (style, path) pairs, to check what was written."""
    assert data[:4] == b"ncif"
    i = 5  # past "ncif" and the style count
    styles = []
    for _ in range(data[4]):
        kind = data[i]
        if kind == 0x01:
            styles.append(("solid", tuple(data[i + 1:i + 5])))
            i += 5
        else:
            assert kind == 0x02, kind
            gkind, flags, count = data[i + 1], data[i + 2], data[i + 3]
            i += 4
            assert flags & 0x02, "expected a transform"
            matrix = tuple(unfloat24(data[i + 3 * k:i + 3 * k + 3]) for k in range(6))
            i += 18
            stops = []
            for _ in range(count):
                stops.append((data[i], tuple(data[i + 1:i + 5])))
                i += 5
            styles.append(("gradient", gkind, matrix, tuple(stops)))
    count = data[i]
    i += 1
    paths = []
    for _ in range(count):
        flags, count = data[i], data[i + 1]
        i += 2
        assert flags == 0x02, flags
        verts = []
        for _ in range(count):
            point = []
            for _ in range(3):
                xs, ys = data[i], data[i + 2]
                point.append((((xs & 0x7f) << 8 | data[i + 1]) / 102.0 - 128.0,
                              ((ys & 0x7f) << 8 | data[i + 3]) / 102.0 - 128.0))
                i += 4
            verts.append(point)
        paths.append(verts)
    shapes = []
    count = data[i]
    i += 1
    for _ in range(count):
        assert data[i] == 0x0a, data[i]
        style, count = data[i + 1], data[i + 2]
        members = list(data[i + 3:i + 3 + count])
        i += 3 + count + 1
        shapes.append((styles[style], [paths[m] for m in members]))
    return shapes


def draw(items, path, size=512, ground=(0x6f, 0x8f, 0xb8)):
    """Draws (style, contours) pairs the way HVIF will, as a check."""
    surface = skia.Surface(size, size)
    canvas = surface.getCanvas()
    canvas.clear(skia.Color(*ground, 255))
    s = size / 64.0
    for style, contours_ in items:
        paint = skia.Paint(AntiAlias=True)
        if style[0] == "solid":
            r, g, b, a = style[1]
            paint.setColor(skia.Color(r, g, b, a))
        else:
            _, kind, (a, b, c, d, tx, ty), stops = style
            colors = [skia.Color(r, g, bl, al) for _, (r, g, bl, al) in stops]
            positions = [o / 255 for o, _ in stops]
            m = skia.Matrix.MakeAll(a, c, tx, b, d, ty, 0, 0, 1)
            if kind == 0:
                shader = skia.GradientShader.MakeLinear([(-64, 0), (64, 0)], colors, positions,
                                                        skia.TileMode.kClamp, 0, m)
            else:
                shader = skia.GradientShader.MakeRadial((0, 0), 64, colors, positions,
                                                        skia.TileMode.kClamp, 0, m)
            paint.setShader(shader)
        p = skia.Path()
        for verts in contours_:
            p.moveTo(*verts[0][0])
            for i in range(1, len(verts) + 1):
                a, b = verts[i - 1], verts[i % len(verts)]
                p.cubicTo(*a[2], *b[1], *b[0])
            p.close()
        canvas.save()
        canvas.scale(s, s)
        canvas.drawPath(p, paint)
        canvas.restore()
    surface.makeImageSnapshot().save(path, skia.kPNG)


items = []
walk(root, {}, 1.0, None, items)
data = hvif(items)
out = os.path.join(here, "haikucode.hvif")
with open(out, "wb") as f:
    f.write(data)
if "--preview" in sys.argv:
    draw([(style, contours(geometry)) for style, geometry in items],
         sys.argv[sys.argv.index("--preview") + 1])
if "--verify" in sys.argv:
    draw(decode(data), sys.argv[sys.argv.index("--verify") + 1])
print(f"{out}: {len(data)} bytes")
