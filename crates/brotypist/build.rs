use std::{env, fs::{self, OpenOptions}, io::Write, path::{Path, PathBuf}, process::Command};
const LLAMA_REVISION: &str = "0c1e57098bba43ac29e6e3b677cdceebdd22334f";

fn main() {
    println!("cargo:rerun-if-env-changed=BROTYPIST_LLAMA_LIB_DIR");
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    println!("cargo:rerun-if-changed=native/llama_bridge.c");
    println!("cargo:rerun-if-changed=native/include");
    println!("cargo:rerun-if-changed=vendor/llama.cpp/CMakeLists.txt");
    println!("cargo:rustc-env=BROTYPIST_LLAMA_REVISION={LLAMA_REVISION}");
    let target = env::var("CARGO_CFG_TARGET_OS").expect("Cargo target OS");
    if target == "windows" { windows_resources(); }
    let (library_dir, runtime_dirs, source_kind) = if let Some(directory) = env::var_os("BROTYPIST_LLAMA_LIB_DIR") {
        let directory = PathBuf::from(directory).canonicalize().expect("BROTYPIST_LLAMA_LIB_DIR must be an existing native library directory");
        assert!(directory.is_dir(), "BROTYPIST_LLAMA_LIB_DIR must be a directory containing libraries from {LLAMA_REVISION}");
        (directory.clone(), vec![directory], "external-library-directory")
    } else {
        let source = Path::new("vendor/llama.cpp");
        assert!(source.join("CMakeLists.txt").is_file(), "Initialize the pinned vendor/llama.cpp source submodule ({LLAMA_REVISION}), or explicitly set BROTYPIST_LLAMA_LIB_DIR to matching native libraries. Python is never used.");
        if source.join(".git").exists() {
            let revision = Command::new("git").args(["-C", "vendor/llama.cpp", "rev-parse", "HEAD"]).output().expect("Reading pinned llama.cpp source revision");
            assert!(revision.status.success() && String::from_utf8_lossy(&revision.stdout).trim() == LLAMA_REVISION, "vendor/llama.cpp must be pinned at {LLAMA_REVISION}");
        }
        let mut native = cmake::Config::new(source);
        if target == "macos" {
            native.define("CMAKE_OSX_DEPLOYMENT_TARGET", env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_else(|_| "15.0".into()));
        }
        if target == "windows" {
            // Match packaged Rust binaries' -C target-feature=+crt-static.
            // This covers both cmake's compiler flags and CMake target policy.
            native.static_crt(true).define("CMAKE_MSVC_RUNTIME_LIBRARY", "MultiThreaded");
        }
        native.define("BUILD_SHARED_LIBS", "ON")
            .define("LLAMA_BUILD_TESTS", "OFF").define("LLAMA_BUILD_EXAMPLES", "OFF")
            .define("LLAMA_BUILD_COMMON", "OFF").define("LLAMA_BUILD_APP", "OFF")
            .define("LLAMA_BUILD_TOOLS", "OFF").define("LLAMA_BUILD_SERVER", "OFF")
            .define("LLAMA_BUILD_UI", "OFF").define("LLAMA_USE_PREBUILT_UI", "OFF")
            .define("LLAMA_OPENSSL", "OFF").define("LLAMA_SUBPROCESS", "OFF")
            .define("GGML_NATIVE", "OFF")
            // macOS/Windows releases must not acquire undeclared Homebrew or
            // LLVM OpenMP runtimes. Linux packaging declares system OpenMP.
            .define("GGML_OPENMP", if target == "linux" { "ON" } else { "OFF" })
            .define("GGML_OPENMP_FETCH", "OFF")
            .define("GGML_METAL", if target == "macos" { "ON" } else { "OFF" })
            .define("GGML_METAL_EMBED_LIBRARY", "ON")
            .define("CMAKE_INSTALL_RPATH", if target == "macos" { "@loader_path" } else { "$ORIGIN" })
            .define("CMAKE_INSTALL_RPATH_USE_LINK_PATH", "FALSE")
            .profile("Release");
        let installed = native.build();
        let lib = installed.join("lib");
        let library_dir = if lib.is_dir() { lib } else { installed.join("lib64") }.canonicalize().expect("Native installation must provide its library directory");
        let mut runtime_dirs = vec![library_dir.clone()];
        if target == "windows" && installed.join("bin").is_dir() { runtime_dirs.push(installed.join("bin").canonicalize().expect("Native DLL directory")); }
        (library_dir, runtime_dirs, "pinned-source")
    };
    let mut shim = cc::Build::new();
    shim.file("native/llama_bridge.c").include("native/include").warnings(true);
    shim.define("LLAMA_SHARED", None);
    if target == "windows" { shim.static_crt(true); }
    if target == "macos" { shim.flag("-x").flag("objective-c").flag("-fno-objc-arc"); }
    shim.compile("brotypist_llama_bridge");
    println!("cargo:rustc-link-search=native={}", library_dir.display());
    println!("cargo:rustc-link-lib=dylib=llama");
    // Local builds use their own exact engine; distributed applications package
    // that engine beside the executable/in Frameworks, not in a Python install.
    if target == "macos" {
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", library_dir.display());
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,NaturalLanguage");
    } else if target != "windows" {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib");
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", library_dir.display());
    }
    let manifest = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo native output directory")).join("native-libraries.json");
    write_native_manifest(&manifest, &target, &library_dir, &runtime_dirs, source_kind);
    println!("cargo::metadata=native_libraries_manifest={}", manifest.display());
}

fn write_native_manifest(path: &Path, target_os: &str, library_dir: &Path, runtime_dirs: &[PathBuf], source_kind: &str) {
    assert!(path.is_absolute(), "Cargo must provide an absolute native output directory");
    let mut libraries = Vec::new();
    for directory in runtime_dirs {
        for entry in fs::read_dir(directory).expect("Reading native runtime artifacts") {
            let entry = entry.expect("Reading native runtime artifact entry");
            let path = entry.path();
            if !path.is_file() { continue; }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else { continue; };
            let name = name.to_ascii_lowercase();
            let engine = name.starts_with("libllama.") || name.starts_with("libggml") || name.starts_with("llama.") || name.starts_with("ggml");
            let shared = match target_os {
                "macos" => name.ends_with(".dylib"),
                "windows" => name.ends_with(".dll"),
                _ => name.ends_with(".so") || name.contains(".so."),
            };
            if engine && shared { libraries.push(path); }
        }
    }
    libraries.sort();
    libraries.dedup();
    assert!(libraries.iter().any(|path| path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("libllama.") || name.starts_with("llama."))), "Native manifest cannot omit the llama runtime library");
    let manifest = serde_json::json!({
        "schema": 1,
        "llama_revision": LLAMA_REVISION,
        "target": env::var("TARGET").expect("Cargo target triple"),
        "target_os": target_os,
        "source_kind": source_kind,
        "library_dir": library_dir,
        "libraries": libraries,
    });
    // Packaging consumes this invocation's Cargo build-script-executed OUT_DIR,
    // never a glob or a previously selected target-cache directory. A failed
    // build is not an artifact receipt; the caller must require Cargo success.
    let mut file = OpenOptions::new().write(true).create(true).truncate(true).open(path).expect("Opening this build's native artifact manifest");
    serde_json::to_writer_pretty(&mut file, &manifest).expect("Writing native artifact manifest");
    file.write_all(b"\n").expect("Completing native artifact manifest");
    file.sync_all().expect("Persisting native artifact manifest");
}

fn windows_resources() {
    // winresource's MSVC path invokes the native Windows SDK rc.exe. Never
    // substitute GNU windres/Unix llvm-rc or retrofit resources into a PE file.
    let host = env::var("HOST").expect("Cargo build host");
    let target_env = env::var("CARGO_CFG_TARGET_ENV").expect("Cargo target environment");
    assert!(host.contains("-windows-") && target_env == "msvc", "Build Windows executables natively with the Windows SDK and an MSVC Rust target to embed their real PE resources");
    if let Some(compiler) = env::var_os("RC_PATH") {
        assert!(Path::new(&compiler).file_name().and_then(|name| name.to_str()).is_some_and(|name| name.eq_ignore_ascii_case("rc.exe")), "RC_PATH must select the Windows SDK rc.exe, not a replacement resource compiler");
    }
    println!("cargo:rerun-if-env-changed=RC_PATH");
    println!("cargo:rerun-if-changed=assets/app.ico");
    let version = env::var("CARGO_PKG_VERSION").expect("Cargo package version");
    let mut numeric_version = 0u64;
    for component in ["CARGO_PKG_VERSION_MAJOR", "CARGO_PKG_VERSION_MINOR", "CARGO_PKG_VERSION_PATCH"] {
        let value = env::var(component).expect("Cargo package version component").parse::<u16>()
            .expect("Windows VS_VERSIONINFO version components must fit unsigned 16-bit integers");
        numeric_version = (numeric_version << 16) | u64::from(value);
    }
    numeric_version <<= 16; // Exact major.minor.patch.0 VS numeric tuple.
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/app.ico")
        .set("ProductName", "BroTypist")
        .set("FileDescription", "BroTypist")
        .set("ProductVersion", &version)
        .set("FileVersion", &version)
        .set_version_info(winresource::VersionInfo::PRODUCTVERSION, numeric_version)
        .set_version_info(winresource::VersionInfo::FILEVERSION, numeric_version)
        .set_version_info(winresource::VersionInfo::FILEOS, 0x0004_0004)
        .set_version_info(winresource::VersionInfo::FILETYPE, 1);
    // compile emits a Cargo-wide MSVC .res linker argument, so both explicit
    // application binaries (BroTypist and BroTypist-cli) receive the resource.
    resource.compile().expect("Compiling BroTypist icon/version resources with the native Windows SDK failed");
}
