"""Converts G HUB's shipped preset data into the compact files ghub-rs embeds or ships.

Inputs: the G HUB files fetch_from_ghub.py copied beside this script. G HUB is not needed.
Outputs: ghub-core/data/*.json (embedded) and assets/presets/games.json (loaded by the window).
"""
import json, os, re, sys

GHUB = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(GHUB, "..", ".."))
CORE_DATA = os.path.join(ROOT, "crates", "ghub-core", "data")
ASSETS = os.path.join(ROOT, "assets", "presets")
os.makedirs(CORE_DATA, exist_ok=True)
os.makedirs(ASSETS, exist_ok=True)


def load(path):
    return json.loads(open(path, "rb").read().decode("utf-8"))


def dump(path, obj):
    text = json.dumps(obj, separators=(",", ":"), ensure_ascii=False, sort_keys=False)
    open(path, "w", encoding="utf-8", newline="\n").write(text + "\n")
    print(f"{os.path.relpath(path, ROOT)}: {len(text.encode('utf-8')):,} bytes")


# ------------------------------------------------------------------ strings
strings = {}
for line in open(os.path.join(GHUB, "strings", "en-US.yaml"), encoding="utf-8"):
    m = re.match(r"^([A-Z0-9_]+): (.*)$", line.rstrip("\n"))
    if m:
        value = m.group(2).strip()
        if len(value) >= 2 and value[0] == value[-1] == '"':
            value = value[1:-1]
        strings[m.group(1)] = value

# ------------------------------------------------------------------ per-key slot codes
SLOT_PREFIX = {
    "PERKEY_KEYBOARD": "K",
    "PERKEY_CONSUMER": "C",
    "PERKEY_GKEY": "G",
    "PERKEY_BRANDING": "B",
    "PERKEY_INDICATOR": "I",
}


def slot_key(slot, code):
    prefix = SLOT_PREFIX.get(slot)
    return None if prefix is None else f"{prefix}{code}"


# ------------------------------------------------------------------ keyframe animations
prefabs = load(os.path.join(GHUB, "lighting", "effect_prefabs.json"))["list"]
animations = []
for prefab in prefabs:
    if prefab["type"] != "KEYFRAME" or "KEYBOARD_RGB_PER_KEY" not in prefab["deviceSupport"]:
        continue
    info = load(os.path.join(GHUB, "lighting", "effects", prefab["infoPath"]))["keyframeInfo"]
    frames = []
    for index in sorted(info["keyframeMap"], key=int):
        frame = info["keyframeMap"][index]
        colors = {}
        for slot, value in frame["lightingSlots"]["infoMap"].items():
            for code, color in value.get("perKeyMap", {}).get("colorCodeMap", {}).items():
                key = slot_key(slot, code)
                if key:
                    colors[key] = color["hex"].lstrip("#").lower()
        grouped = {}
        for key, hex_ in colors.items():
            grouped.setdefault(hex_, []).append(key)
        # Grouped by colour: the same frame in a quarter of the bytes, because most keys share one.
        frames.append({"length": frame["length"], "curve": frame.get("curveType") or "LINEAR",
                       "colors": {h: " ".join(sorted(k)) for h, k in grouped.items()}})
    animations.append({
        "id": prefab["id"],
        "name": prefab["name"],
        "source": prefab["infoPath"],
        "direction": info.get("direction", "CYCLE"),
        "speed": info.get("speed", 100),
        "frames": frames,
    })
dump(os.path.join(CORE_DATA, "ghub_keyframes.json"), {"animations": animations})

# ------------------------------------------------------------------ screen sampler (keyboard)
sampler = load(os.path.join(GHUB, "lighting", "effects", "screen_sampler-kb.json"))["screen_sampler_info"]
regions = [
    {k: r[k] for k in ("id", "name", "left", "top", "right", "bottom")}
    for r in sorted(sampler["region_map"].values(), key=lambda r: r["left"])
]
keys = {}
for slot, value in sampler["lighting_slots"]["info_map"].items():
    for code, entry in value.get("perKeyMap", {}).get("colorCodeMap", {}).items():
        key = slot_key(slot, code)
        if key:
            keys[key] = entry["tag"]
zones = {slot: v["zone_color"]["tag"] for slot, v in sampler["lighting_slots"]["info_map"].items() if "zone_color" in v}
dump(os.path.join(CORE_DATA, "ghub_screen_sampler.json"), {"regions": regions, "keys": keys, "zones": zones})

# ------------------------------------------------------------------ audio visualizer (keyboard)
def hz(token):
    return int(re.search(r"(\d+)HZ$", token).group(1))


visual = load(os.path.join(GHUB, "lighting", "effects", "audio-visualizer-kb-per-key.json"))["audioVisualizerInfo"]
columns = []
for column in visual["frequencyLayout"]:
    slots = []
    for s in column["slots"]:
        if "slotCode" in s:
            key = slot_key(s["perKeyType"], s["slotCode"])
            if key:
                slots.append(key)
            # else: a light strip on another device, not part of a keyboard column
        else:
            slots.append(None)  # a gap G HUB leaves where a keyboard has no key
    if not any(slots):
        continue  # cosmetic-only columns (light strips) have nothing on a keyboard
    columns.append({"low": hz(column["range"]["start"]), "high": hz(column["range"]["end"]), "keys": slots})
default_range = load(os.path.join(GHUB, "lighting", "effects", "audio-visualizer-generic.json"))
generic = default_range["audioVisualizerInfo"]["frequencyDefaultRange"]
dump(os.path.join(CORE_DATA, "ghub_audio_visualizer.json"), {
    "defaultLow": hz(generic["start"]), "defaultHigh": hz(generic["end"]), "columns": columns,
})

# ------------------------------------------------------------------ lighting effect list
effects = [
    {"name": p["name"], "type": p["type"], "deviceSupport": p["deviceSupport"], "infoPath": p.get("infoPath")}
    for p in prefabs
]

# ------------------------------------------------------------------ command cards
cards = load(os.path.join(GHUB, "card_presets.json"))["categories"]
SUBMENU = {
    "Mouse": "Mouse", "Media": "Media", "Audio": "Audio", "Editing": "Editing",
    "Device": "Device", "Webcam": "Webcam", "Lighting": "Lighting", "OS": "System", "GHUB": "G HUB",
}
out_cards = []
for category, value in cards.items():
    for preset in value["presets"]:
        if preset.get("platform") == "OSX":
            continue
        name = preset["name"]
        display = strings.get(f"ASSIGNMENT_NAME_{name}")
        if display is None:
            for key in (f"ASSIGNMENT_NAME_LIGHTING_{name}", f"ASSIGNMENT_NAME_GHUB_{name}", "ASSIGNMENT_NAME_SYSTEM_LOCK_SCREEN" if name.startswith("LOCK_SCREEN") else ""):
                if key in strings:
                    display = strings[key]
                    break
        if display is None:
            display = name.replace("_", " ").title()
        entry = {"id": name, "name": display, "category": SUBMENU.get(category, category)}
        if "keystroke" in preset:
            entry["keystroke"] = preset["keystroke"]
        if "macro" in preset:
            entry["macro"] = preset["macro"]
        out_cards.append(entry)

apps = load(os.path.join(GHUB, "applications.json"))["applications"]
desktop = next(a for a in apps if a["name"] == "APPLICATION_NAME_DESKTOP")
desktop_commands = [{"category": c["category"], "name": c["name"], "keystroke": c["keystroke"]} for c in desktop["commands"]]
dump(os.path.join(CORE_DATA, "ghub_commands.json"), {
    "cards": out_cards,
    "desktop": {"name": strings.get("APPLICATION_NAME_DESKTOP", "Desktop"), "commands": desktop_commands,
                "categoryColors": {c["tag"]: c["hex"] for c in desktop.get("categoryColors", [])}},
    "lightingEffects": effects,
})

# ------------------------------------------------------------------ game command library
games = []
for app in apps:
    if app is desktop or not app.get("commands"):
        continue
    if app.get("platform") not in (None, "win", "any", "ANY"):
        continue
    detection = {}
    for d in app.get("detection", []):
        for kind, value in d.items():
            if kind == "steam":
                detection.setdefault("steam", []).append(value["appId"])
            elif kind == "epicGames":
                detection.setdefault("epic", []).append(value["appName"])
            elif kind == "gogGalaxy":
                detection.setdefault("gog", []).append(value["productId"])
            elif kind == "winRegistry":
                detection.setdefault("registry", []).append(value)
            elif kind == "glob":
                detection.setdefault("glob", []).append(value)
            elif kind == "uplay":
                detection.setdefault("uplay", []).append(value)
    games.append({
        "id": app["applicationId"],
        "name": app["name"],
        "detection": detection,
        "colors": {c["tag"]: c["hex"] for c in app.get("categoryColors", [])},
        "commands": [[c["category"], c["name"], c["keystroke"]] for c in app["commands"] if c["keystroke"]],
    })
games.sort(key=lambda g: g["name"].lower())
dump(os.path.join(ASSETS, "games.json"), {"source": "G HUB applications.json", "games": games})
print("animations", [a["name"] for a in animations], "cards", len(out_cards), "games", len(games),
      "commands", sum(len(g["commands"]) for g in games))
