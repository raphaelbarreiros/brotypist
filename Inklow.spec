# Native onedir build: run tools/build_app.py on the destination OS.
from pathlib import Path
import os
import runpy
import sys

ROOT = Path(SPECPATH)
ASSETS = ROOT / "src" / "inklow" / "assets"
VERSION = runpy.run_path(str(ROOT / "src" / "inklow" / "__init__.py"))["__version__"]
native_excludes = ["PySide6"] if sys.platform == "darwin" else []
codesign_identity = os.environ.get("INKLOW_CODESIGN_IDENTITY") if sys.platform == "darwin" else None
keyring_backend = ("keyring.backends.macOS" if sys.platform == "darwin" else
                   "keyring.backends.Windows" if sys.platform == "win32" else
                   "keyring.backends.SecretService")

analysis = Analysis(
    [str(ROOT / "packaging" / "launcher.py")],
    pathex=[str(ROOT / "src")],
    binaries=[],
    datas=[(str(ASSETS), "inklow/assets")],
    hiddenimports=["llama_cpp", keyring_backend],
    hookspath=[str(ROOT / "packaging" / "hooks")],
    hooksconfig={},
    runtime_hooks=[],
    # Neither tests/build tooling nor optional model-server stacks belong in the
    # resident application. User settings and model caches are never inputs.
    excludes=["pytest", "PySide6.QtWebEngineCore", "PySide6.QtWebEngineWidgets",
              "PySide6.QtQml", "PySide6.QtQuick", "llama_cpp.server"] + native_excludes,
    noarchive=False,
    optimize=0,
)
pyz = PYZ(analysis.pure)
exe = EXE(
    pyz,
    analysis.scripts,
    [],
    exclude_binaries=True,
    name="Inklow",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    codesign_identity=codesign_identity,
    icon=str(ASSETS / ("app.ico" if sys.platform == "win32" else "app.icns"))
         if sys.platform in ("win32", "darwin") else None,
)
executables = [exe]
if sys.platform == "win32":
    # A windowed bootloader intentionally has no stdin/stdout. Keep the public
    # CLI usable without opening a console for normal GUI launches.
    executables.append(EXE(
        pyz,
        analysis.scripts,
        [],
        exclude_binaries=True,
        name="Inklow-cli",
        debug=False,
        bootloader_ignore_signals=False,
        strip=False,
        upx=False,
        console=True,
        icon=str(ASSETS / "app.ico"),
    ))
collection = COLLECT(
    *executables,
    analysis.binaries,
    analysis.datas,
    strip=False,
    upx=False,
    name="Inklow",
)
if sys.platform == "darwin":
    app = BUNDLE(
        collection,
        name="Inklow.app",
        icon=str(ASSETS / "app.icns"),
        bundle_identifier="com.inklow.Inklow",
        version=VERSION,
        codesign_identity=codesign_identity,
        info_plist={
            "CFBundleName": "Inklow",
            "CFBundleDisplayName": "Inklow",
            "CFBundleShortVersionString": VERSION,
            "CFBundleVersion": VERSION,
            "CFBundleIconFile": "app.icns",
            "NSPrincipalClass": "NSApplication",
            "NSHighResolutionCapable": True,
            "NSAppleScriptEnabled": False,
            "NSHumanReadableCopyright": "Inklow — independent, local-first predictive typing",
            # A resident typing assistant has settings, not a document window.
            "LSUIElement": True,
        },
    )
