#!/usr/bin/env python3
"""Generates the demo game's models and textures (stdlib only).

    python3 tools/gen_demo_assets.py [game_folder]

Writes models/*.gltf + .bin (glTF 2.0) and textures/*.png.
"""
import json
import math
import os
import struct
import sys
import zlib

ROOT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "..", "game")


# ---------------------------------------------------------------- PNG writer
def write_png(path, width, height, pixel):
    rows = bytearray()
    for y in range(height):
        rows.append(0)  # filter: none
        for x in range(width):
            rows.extend(pixel(x, y))

    def chunk(kind, data):
        c = kind + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(rows), 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


def checker(x, y):
    on = ((x // 32) + (y // 32)) % 2 == 0
    v = 200 if on else 120
    return (v, v, v + 8, 255)


CRATE_SIZE = 128


def crate(x, y):
    size = CRATE_SIZE
    border = x < 10 or y < 10 or x >= size - 10 or y >= size - 10
    diagonal = abs(x - y) < 7 or abs(x - (size - 1 - y)) < 7
    grain = int(12 * math.sin(y * 0.35 + math.sin(x * 0.05) * 3))
    if border or diagonal:
        return (130 + grain // 2, 82, 40, 255)
    return (196 + grain, 140 + grain, 76, 255)


# ------------------------------------------------------------- glTF writer
def cross(a, b):
    return (a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0])


def write_gltf(name, positions, normals, uvs, indices, material):
    vbin = b"".join(struct.pack("<3f", *p) for p in positions)
    nbin = b"".join(struct.pack("<3f", *n) for n in normals)
    tbin = b"".join(struct.pack("<2f", *t) for t in uvs)
    ibin = b"".join(struct.pack("<I", i) for i in indices)
    blob = vbin + nbin + tbin + ibin
    offsets = [0, len(vbin), len(vbin) + len(nbin), len(vbin) + len(nbin) + len(tbin)]
    lengths = [len(vbin), len(nbin), len(tbin), len(ibin)]

    lo = [min(p[i] for p in positions) for i in range(3)]
    hi = [max(p[i] for p in positions) for i in range(3)]
    gltf = {
        "asset": {"version": "2.0", "generator": "basalt gen_demo_assets.py"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0, "name": name}],
        "meshes": [{
            "name": name,
            "primitives": [{
                "attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2},
                "indices": 3,
                "material": 0,
            }],
        }],
        "materials": [material],
        "buffers": [{"uri": f"{name}.bin", "byteLength": len(blob)}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": offsets[0], "byteLength": lengths[0], "target": 34962},
            {"buffer": 0, "byteOffset": offsets[1], "byteLength": lengths[1], "target": 34962},
            {"buffer": 0, "byteOffset": offsets[2], "byteLength": lengths[2], "target": 34962},
            {"buffer": 0, "byteOffset": offsets[3], "byteLength": lengths[3], "target": 34963},
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": len(positions), "type": "VEC3", "min": lo, "max": hi},
            {"bufferView": 1, "componentType": 5126, "count": len(normals), "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": len(uvs), "type": "VEC2"},
            {"bufferView": 3, "componentType": 5125, "count": len(indices), "type": "SCALAR"},
        ],
    }
    if "pbrMetallicRoughness" in material and "baseColorTexture" in material["pbrMetallicRoughness"]:
        gltf["textures"] = [{"source": 0}]
        gltf["images"] = [{"uri": material.pop("_image")}]
    models = os.path.join(ROOT, "models")
    with open(os.path.join(models, f"{name}.gltf"), "w") as f:
        json.dump(gltf, f, indent=2)
        f.write("\n")
    with open(os.path.join(models, f"{name}.bin"), "wb") as f:
        f.write(blob)


def quad_mesh(faces, half, uv_scale=1.0):
    """faces: list of (normal, u_axis, v_axis); builds CCW quads facing out."""
    P, N, T, I = [], [], [], []
    for n, u, v in faces:
        assert cross(u, v) == n, (n, u, v)
        base = len(P)
        for su, sv, uv in [(-1, -1, (0, 1)), (1, -1, (1, 1)), (1, 1, (1, 0)), (-1, 1, (0, 0))]:
            P.append(tuple(half * (n[i] + su * u[i] + sv * v[i]) for i in range(3)))
            N.append(n)
            T.append((uv[0] * uv_scale, uv[1] * uv_scale))
        I += [base, base + 1, base + 2, base, base + 2, base + 3]
    return P, N, T, I


def textured(image, color=(1, 1, 1, 1)):
    return {
        "name": image,
        "pbrMetallicRoughness": {"baseColorFactor": list(color), "baseColorTexture": {"index": 0}},
        "_image": f"../textures/{image}",
    }


def main():
    os.makedirs(os.path.join(ROOT, "models"), exist_ok=True)
    os.makedirs(os.path.join(ROOT, "textures"), exist_ok=True)
    write_png(os.path.join(ROOT, "textures", "checker.png"), 256, 256, checker)
    write_png(os.path.join(ROOT, "textures", "crate.png"), CRATE_SIZE, CRATE_SIZE, crate)

    cube_faces = [
        ((1, 0, 0), (0, 0, -1), (0, 1, 0)),
        ((-1, 0, 0), (0, 0, 1), (0, 1, 0)),
        ((0, 1, 0), (1, 0, 0), (0, 0, -1)),
        ((0, -1, 0), (1, 0, 0), (0, 0, 1)),
        ((0, 0, 1), (1, 0, 0), (0, 1, 0)),
        ((0, 0, -1), (-1, 0, 0), (0, 1, 0)),
    ]
    write_gltf("crate", *quad_mesh(cube_faces, 0.5), textured("crate.png"))

    # 40x40 ground plane at y=0, texture tiled 20 times.
    P, N, T, I = quad_mesh([((0, 1, 0), (1, 0, 0), (0, 0, -1))], 20.0, uv_scale=20.0)
    P = [(p[0], 0.0, p[2]) for p in P]
    write_gltf("ground", P, N, T, I, textured("checker.png"))

    # Flat-shaded octahedron "gem", untextured, colored by the material factor.
    tips = [(0, 1, 0), (0, -1, 0)]
    ring = [(1, 0, 0), (0, 0, -1), (-1, 0, 0), (0, 0, 1)]
    P, N, T, I = [], [], [], []
    for tip in tips:
        for k in range(4):
            a, b = ring[k], ring[(k + 1) % 4]
            tri = [a, b, tip] if tip[1] > 0 else [b, a, tip]
            # Scale first: a non-uniform scale changes face orientation, so
            # the normal must come from the final triangle.
            tri = [(p[0] * 0.5, p[1] * 0.8, p[2] * 0.5) for p in tri]
            e1 = tuple(tri[1][i] - tri[0][i] for i in range(3))
            e2 = tuple(tri[2][i] - tri[0][i] for i in range(3))
            n = cross(e1, e2)
            ln = math.sqrt(sum(c * c for c in n))
            n = tuple(c / ln for c in n)
            for p in tri:
                I.append(len(P))
                P.append(p)
                N.append(n)
                T.append((0.0, 0.0))
    write_gltf("gem", P, N, T, I, {
        "name": "gem",
        "pbrMetallicRoughness": {"baseColorFactor": [0.25, 0.85, 0.95, 1.0]},
    })
    print(f"wrote demo assets to {os.path.abspath(ROOT)}")


if __name__ == "__main__":
    main()
