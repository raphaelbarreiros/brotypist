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
entitlements_file = str(ROOT / "packaging" / "macos" / "entitlements.plist") if sys.platform == "darwin" else None
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
        """Import required desktop bindings in isolation before collecting them."""
        import gi
        import gi._gi
        import pyatspi
        import Xlib
        gi.require_version("Atspi", "2.0")
        gi.require_version("Pango", "1.0")
        gi.require_version("PangoCairo", "1.0")
        from gi.repository import Atspi, Pango, PangoCairo
        return (callable(Atspi.get_desktop) and callable(Pango.FontDescription)
                and callable(PangoCairo.FontMap.get_default))

    require_linux_bindings()  # Fail the build, never ship an UnavailableBridge.
    wayland_library = ROOT / "src/inklow/native/libinklow_wayland.so"
    if not wayland_library.is_file():
        raise RuntimeError("Build the native Wayland client with tools/build_wayland.py before freezing Linux.")
    linux_binaries.append((str(wayland_library), "inklow/native"))
    linux_imports = collect_submodules("pyatspi") + ["gi", "gi._gi", "Xlib"]
    pending = [("Atspi", "2.0"), ("Pango", "1.0"), ("PangoCairo", "1.0")]
    # Traverse typelib dependencies once so frozen GI imports retain their
    # matching native libraries, not just the top-level accessibility namespace.
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

# An independent onefile runtime keeps the transactional updater alive while the
# resident application's complete directory is exchanged underneath it.
native_binaries = list(linux_binaries)
if sys.platform in ("darwin", "linux"):
    updater_analysis = Analysis(
        [str(ROOT / "packaging" / "update_launcher.py")],
        pathex=[str(ROOT / "src")],
        binaries=[],
        datas=[],
        hiddenimports=["cryptography.hazmat.primitives.asymmetric.ed25519"],
        hookspath=[str(ROOT / "packaging" / "hooks")],
        excludes=["pytest", "PySide6", "llama_cpp", "tkinter",
                  "AppKit", "Foundation", "Quartz", "objc"],
        noarchive=False,
        optimize=0,
    )
    updater_executable = EXE(
        PYZ(updater_analysis.pure),
        updater_analysis.scripts,
        updater_analysis.binaries,
        updater_analysis.datas,
        [],
        name="BroTypist-updater",
        debug=False,
        strip=False,
        upx=False,
        console=True,
        codesign_identity=codesign_identity,
    )
    native_binaries.append((updater_executable.name, "helpers"))

analysis = Analysis(
    [str(ROOT / "packaging" / "launcher.py")],
    pathex=[str(ROOT / "src")],
    binaries=native_binaries,
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
    entitlements_file=entitlements_file,
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
    # Compile into PyInstaller's per-build work directory, before BUNDLE seals
    # its resources. No source .icon package or runtime renderer is shipped.
    icon_tools = runpy.run_path(str(ROOT / "tools" / "generate_icons.py"))
    native_icon_directory = Path(workpath) / "native-icons"
    native_icon_info = icon_tools["compile_native_icons"](native_icon_directory)
    native_icon_resources = [
        ("Assets.car", str(native_icon_directory / "Assets.car"), "DATA"),
    ]
    app = BUNDLE(
        collection,
        native_icon_resources,
        name="BroTypist.app",
        icon=str(ASSETS / "app.icns"),
        bundle_identifier="com.inklow.Inklow",
        version=VERSION,
        # PyInstaller inherits signing identity and entitlements from collection,
        # which inherits them from exe; BUNDLE does not accept signing kwargs.
        info_plist={
            **native_icon_info,
            "CFBundleName": "BroTypist",
            "CFBundleDisplayName": "BroTypist",
            "CFBundleShortVersionString": VERSION,
            "CFBundleVersion": VERSION,
            "CFBundleIconFile": "app.icns",
            "CFBundleURLTypes": [{
                "CFBundleURLName": "BroTypist Settings",
                "CFBundleURLSchemes": ["brotypist"],
            }],
            "NSPrincipalClass": "NSApplication",
            "NSHighResolutionCapable": True,
            "NSAppleScriptEnabled": False,
            "NSAppleEventsUsageDescription": "With your permission, BroTypist reads the active Terminal or iTerm session and font metadata to align local suggestions. It does not read shell history or run commands.",
            "NSHumanReadableCopyright": "BroTypist — independent, local-first predictive typing",
            # A resident typing assistant has settings, not a document window.
            "LSUIElement": True,
            "LSFileQuarantineEnabled": True,
        },
    )
