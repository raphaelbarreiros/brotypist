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

linux_binaries, linux_datas, linux_imports = [], [], []
if sys.platform == "linux":
    # Distro bindings must match the build interpreter's ABI (Ubuntu 24.04:
    # CPython 3.12). Append, do not prepend PYTHONPATH: distro packages must not
    # shadow the project's pinned wheel dependencies. This is build-time only.
    distro_bindings = "/usr/lib/python3/dist-packages"
    if distro_bindings not in sys.path:
        sys.path.append(distro_bindings)
    from PyInstaller import isolated
    from PyInstaller.utils.hooks import collect_submodules
    from PyInstaller.utils.hooks.gi import GiModuleInfo

    @isolated.decorate
    def require_linux_bindings():
        import gi
        import gi._gi
        import pyatspi
        import Xlib
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        return callable(Atspi.get_desktop)

    require_linux_bindings()  # Fail the build, never ship an UnavailableBridge.
    wayland_library = ROOT / "src/inklow/native/libinklow_wayland.so"
    if not wayland_library.is_file():
        raise RuntimeError("Build the native Wayland client with tools/build_wayland.py before freezing Linux.")
    linux_binaries.append((str(wayland_library), "inklow/native"))
    linux_imports = collect_submodules("pyatspi") + ["gi", "gi._gi", "Xlib"]
    pending = [("Atspi", "2.0")]
    collected = set()
    while pending:
        namespace, version = pending.pop()
        if (namespace, version) in collected:
            continue
        collected.add((namespace, version))
        info = GiModuleInfo(namespace, version)
        if not info.available:
            raise RuntimeError(f"Missing required GI namespace: {namespace}-{version}")
        binaries, datas, imports = info.collect_typelib_data()
        if len(binaries) < len(info.sharedlibs):
            raise RuntimeError(f"Unresolved native libraries for {namespace}-{version}")
        linux_binaries.extend(binaries)
        linux_datas.extend(datas)
        linux_imports.extend(imports)
        pending.extend(tuple(dependency.rsplit("-", 1)) for dependency in info.dependencies)

analysis = Analysis(
    [str(ROOT / "packaging" / "launcher.py")],
    pathex=[str(ROOT / "src")],
    binaries=linux_binaries,
    datas=[(str(ASSETS), "inklow/assets")] + linux_datas,
    hiddenimports=["llama_cpp", keyring_backend] + linux_imports,
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
    name="BroTypist",
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
        name="BroTypist-cli",
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
    name="BroTypist",
)
if sys.platform == "darwin":
    app = BUNDLE(
        collection,
        name="BroTypist.app",
        icon=str(ASSETS / "app.icns"),
        bundle_identifier="com.inklow.Inklow",
        version=VERSION,
        codesign_identity=codesign_identity,
        info_plist={
            "CFBundleName": "BroTypist",
            "CFBundleDisplayName": "BroTypist",
            "CFBundleShortVersionString": VERSION,
            "CFBundleVersion": VERSION,
            "CFBundleIconFile": "app.icns",
            "NSPrincipalClass": "NSApplication",
            "NSHighResolutionCapable": True,
            "NSAppleScriptEnabled": False,
            "NSHumanReadableCopyright": "BroTypist — independent, local-first predictive typing",
            # A resident typing assistant has settings, not a document window.
            "LSUIElement": True,
        },
    )
