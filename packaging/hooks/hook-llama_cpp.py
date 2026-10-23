"""Keep ctypes-loaded llama/ggml backends and their native resource layout."""
from PyInstaller.utils.hooks import collect_all, copy_metadata

# llama_cpp resolves libraries relative to its own __file__/lib, including Metal,
# CUDA and CPU backends. collect_all preserves those paths and lets PyInstaller
# analyze/rewrite the shared-library dependencies instead of treating them as
# opaque data. GPU kernels supplied separately by a wheel are retained as data.
datas, binaries, hiddenimports = collect_all(
    "llama_cpp",
    include_py_files=False,
    filter_submodules=lambda name: not name.startswith("llama_cpp.server"),
    exclude_datas=["**/*.gguf", "**/*.ggml", "**/*.safetensors", "**/*.pt", "**/*.pth"],
)
datas += copy_metadata("llama-cpp-python")
if not binaries:
    raise RuntimeError("llama-cpp-python has no native libraries; install a native local backend before building Inklow")
