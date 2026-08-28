#!/usr/bin/env python3
import json
import struct
import sys


def read_inx(path):
    with open(path, "rb") as f:
        magic = f.read(8)
        if magic != b"TRNSRTS\x00":
            print(f"Wrong magic: {magic}")
            return None

        payload_len = struct.unpack(">I", f.read(4))[0]
        payload_bytes = f.read(payload_len)
        return json.loads(payload_bytes)


def find_nodes(node, name_pattern, depth=0):
    results = []
    node_name = node.get("name", "")
    if name_pattern.lower() in node_name.lower():
        results.append((depth, node_name, node))

    for child in node.get("children", []):
        results.extend(find_nodes(child, name_pattern, depth + 1))

    return results


payload = read_inx("example_models/reference/reference.inx")
if payload:
    nodes = payload.get("nodes", {})
    for pattern in ["mouth", "lip"]:
        print(f"\n=== Searching for '{pattern}' ===")
        if isinstance(nodes, dict):
            matches = find_nodes(nodes, pattern)
        else:
            matches = []
            for root in nodes:
                if isinstance(root, dict):
                    matches.extend(find_nodes(root, pattern))

        for depth, name, node in matches:
            indent = "  " * depth
            print(f"{indent}{name} (type={node.get('type', 'Unknown')})")
            print(f"{indent}  z-order: {node.get('zsort', 'N/A')}")
            print(f"{indent}  blend_mode: {node.get('blend_mode', 'N/A')}")
            print(f"{indent}  opacity: {node.get('opacity', 1.0)}")

            if "tint" in node:
                print(f"{indent}  tint: {node['tint']}")
            if "screen_tint" in node:
                print(f"{indent}  screen_tint: {node['screen_tint']}")

            textures = node.get("textures", [])
            if textures:
                print(f"{indent}  textures: {textures}")
