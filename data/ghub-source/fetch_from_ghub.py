"""Copies the G HUB data files ghub-rs's presets are built from into this folder.

Run once on a machine with G HUB installed. After that, `gen_presets.py` (beside this file) reads
only from here, so building and running ghub-rs never needs G HUB.
"""
import os
import shutil

HERE = os.path.dirname(os.path.abspath(__file__))
INSTALL = r"C:\Program Files\LGHUB"
DEPOT = r"C:\ProgramData\LGHUB\depots"

FILES = [
    (os.path.join(INSTALL, "data", "card_presets.json"), "card_presets.json"),
    (os.path.join(INSTALL, "data", "applications.json"), "applications.json"),
    (os.path.join(INSTALL, "version"), "ghub-version.txt"),
]


def strings_file():
    for depot in os.listdir(DEPOT):
        path = os.path.join(DEPOT, depot, "core", "data", "strings", "common", "en-US.yaml")
        if os.path.isfile(path):
            return path
    raise SystemExit("G HUB's en-US.yaml was not found under " + DEPOT)


def main():
    for source, name in FILES + [(strings_file(), os.path.join("strings", "en-US.yaml"))]:
        target = os.path.join(HERE, name)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        shutil.copy2(source, target)
        print("copied", name)
    lighting = os.path.join(HERE, "lighting")
    shutil.rmtree(lighting, ignore_errors=True)
    shutil.copytree(os.path.join(INSTALL, "data", "lighting"), lighting)
    print("copied lighting/")


if __name__ == "__main__":
    main()
