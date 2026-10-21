#!/usr/bin/env python3
"""Exercise staged desktop aliases without installing anything."""
from pathlib import Path
import subprocess
import shutil
import tempfile

script = Path(__file__).with_name("desktop-compat.sh").resolve()
current = "io.github.treeforjs.pdfcraft"
# Existing user MIME associations may still use this historical identifier.
legacy = "ai.storyteller.pdfcraft"
with tempfile.TemporaryDirectory(prefix="pdfcraft-desktop-compat-") as directory:
    prefix = Path(directory)
    applications = prefix / "share/applications"
    icons = prefix / "share/icons/hicolor/256x256/apps"
    applications.mkdir(parents=True)
    icons.mkdir(parents=True)
    desktop = f"[Desktop Entry]\nType=Application\nName=PdfCraft\nExec=pdfcraft %U\nIcon={current}\n"
    (applications / f"{current}.desktop").write_text(desktop, encoding="utf-8", newline="\n")
    (icons / f"{current}.png").write_bytes(b"original fixture icon")
    subprocess.run([shutil.which("bash") or "bash", script.as_posix(), prefix.as_posix()], check=True)
    alias = (applications / f"{legacy}.desktop").read_text(encoding="utf-8")
    assert "NoDisplay=true" in alias
    assert "Exec=pdfcraft %U" in alias and f"Icon={current}" in alias
    assert (applications / f"{current}.desktop").read_text(encoding="utf-8") == desktop
    assert (icons / f"{legacy}.png").read_bytes() == (icons / f"{current}.png").read_bytes()
print("Desktop association and icon compatibility passed; no installation was performed.")
