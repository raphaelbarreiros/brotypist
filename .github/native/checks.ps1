param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('prerequisites', 'check', 'test', 'build', 'cli', 'worker', 'desktop', 'portable')]
    [string] $Phase
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
Set-StrictMode -Version Latest

foreach ($name in @('RUNNER_TEMP', 'NATIVE_TARGET', 'CARGO_TARGET_DIR', 'INKLOW_HOME', 'NATIVE_REPORTS', 'NATIVE_PORTABLE')) {
    if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($name))) { throw "Missing CI isolation setting: $name" }
}
if (![IO.Path]::IsPathRooted($env:INKLOW_HOME) -or !$env:INKLOW_HOME.StartsWith($env:RUNNER_TEMP, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Native CI must use an absolute disposable runner-temporary profile.'
}
if ($env:BROTYPIST_LLAMA_LIB_DIR) { throw 'CI must compile the pinned llama source, not an external native library.' }
if ($env:VIRTUAL_ENV) { throw 'Native CI must not depend on a Python virtual environment.' }
$targetDirectory = Join-Path $env:CARGO_TARGET_DIR "$env:NATIVE_TARGET/release"
$suffix = if ($IsWindows) { '.exe' } else { '' }
$app = Join-Path $targetDirectory "BroTypist-cli$suffix"
$nativeFeatures = @('--features', 'brotypist/native-smoke')
$targetArguments = @('--target', $env:NATIVE_TARGET)

function Invoke-Logged {
    param([string] $Name, [string] $Executable, [string[]] $Arguments)
    # A pipe must never hide the exit status of an earlier native command.
    & $Executable @Arguments 2>&1 | Tee-Object -FilePath (Join-Path $env:NATIVE_REPORTS $Name) | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "$Executable exited with $LASTEXITCODE; see $Name" }
}
function Invoke-DesktopEnvironment {
    param([string] $Name, [string] $Executable, [string[]] $Arguments)
    if ($IsLinux) {
        Invoke-Logged $Name 'dbus-run-session' (@('--', 'xvfb-run', '-a', '-s', '-screen 0 1440x1000x24', $Executable) + $Arguments)
    } else { Invoke-Logged $Name $Executable $Arguments }
}
function Save-NativeManifest {
    param([string] $Log, [string] $Kind)
    $outDirectories = @(@(foreach ($line in Get-Content -LiteralPath (Join-Path $env:NATIVE_REPORTS $Log)) {
        try { $message = ConvertFrom-Json -InputObject $line -ErrorAction Stop } catch { continue }
        if ($message.reason -eq 'build-script-executed' -and ($message.package_id -match '#brotypist@|/brotypist#')) { $message.out_dir }
    }) | Select-Object -Unique)
    if (@($outDirectories).Count -ne 1) { throw 'Expected exactly one current BroTypist build-script output directory.' }
    $manifest = Join-Path $outDirectories[0] 'native-libraries.json'
    $native = Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json
    if ($native.source_kind -ne 'pinned-source' -or $native.target -ne $env:NATIVE_TARGET -or $native.schema -ne 1 -or $native.llama_revision -ne '0c1e57098bba43ac29e6e3b677cdceebdd22334f' -or @($native.libraries).Count -eq 0) {
        throw 'Native library provenance does not match the pinned-source native target.'
    }
    foreach ($library in $native.libraries) {
        if (!(Test-Path -LiteralPath $library -PathType Leaf)) { throw "Missing actual native library: $library" }
    }
    Copy-Item -LiteralPath $manifest -Destination (Join-Path $env:NATIVE_REPORTS "native-libraries-$Kind.json")
}
function Import-NativeRuntime {
    $manifest = Join-Path $env:NATIVE_REPORTS 'native-libraries-build.json'
    if (!(Test-Path -LiteralPath $manifest)) { $manifest = Join-Path $env:NATIVE_REPORTS 'native-libraries-check.json' }
    $native = Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json
    # MSVC import libraries and runtime DLLs may be in different CMake directories.
    # Use only the exact successful build's manifest, never a target-cache glob.
    $directories = @($native.libraries | ForEach-Object { Split-Path -Parent $_ } | Select-Object -Unique)
    if ($IsWindows) { $env:PATH = ($directories -join [IO.Path]::PathSeparator) + [IO.Path]::PathSeparator + $env:PATH }
}

switch ($Phase) {
    prerequisites {
        Invoke-Logged 'rustup.log' 'rustup' @('show', 'active-toolchain')
        $revision = & git -C crates/brotypist/vendor/llama.cpp rev-parse HEAD
        if ($LASTEXITCODE -ne 0) { throw 'Pinned llama.cpp submodule is unavailable.' }
        $revision | Set-Content -LiteralPath (Join-Path $env:NATIVE_REPORTS 'llama-revision.log')
        if ($revision.Trim() -ne '0c1e57098bba43ac29e6e3b677cdceebdd22334f') { throw 'llama.cpp submodule revision differs from build.rs pin.' }
        $compiler = & rustc --version --verbose
        if ($LASTEXITCODE -ne 0 -or !($compiler -contains "host: $env:NATIVE_TARGET")) { throw 'CI must compile and run on the declared native Rust target.' }
        if ($IsMacOS) { Invoke-Logged 'xcode.log' '/usr/bin/xcodebuild' @('-version') }
        Invoke-Logged 'cmake.log' 'cmake' @('--version')
        $tag = $env:REQUESTED_TAG
        if ($env:RELEASE_BRANCH) { $tag = 'v' + $env:RELEASE_BRANCH.Substring('release/'.Length) }
        $arguments = @('run', '--locked', '-p', 'xtask') + $targetArguments + @('--', 'version')
        if (![string]::IsNullOrWhiteSpace($tag)) { $arguments += @('--tag', $tag) }
        Invoke-Logged 'version.log' 'cargo' $arguments
    }
    check {
        Invoke-Logged 'cargo-check.jsonl' 'cargo' (@('check', '--locked', '--workspace', '--all-targets', '--message-format=json-render-diagnostics') + $nativeFeatures + $targetArguments)
        Save-NativeManifest 'cargo-check.jsonl' 'check'
    }
    test {
        Import-NativeRuntime
        Invoke-DesktopEnvironment 'cargo-test.log' 'cargo' (@('test', '--locked', '--workspace', '--all-targets') + $nativeFeatures + $targetArguments)
    }
    build {
        Invoke-Logged 'cargo-build.jsonl' 'cargo' (@('build', '--locked', '--release', '--workspace', '--bins', '--examples', '--message-format=json-render-diagnostics') + $nativeFeatures + $targetArguments)
        Save-NativeManifest 'cargo-build.jsonl' 'build'
    }
    cli {
        Import-NativeRuntime
        Invoke-Logged 'cli-version.log' $app @('--version')
        Invoke-Logged 'cli-help.log' $app @('--help')
        Invoke-DesktopEnvironment 'doctor-before.log' $app @('doctor')
        Invoke-Logged 'model-download.log' $app @('model', 'download', '--preset', 'tiny', '--no-select')
    }
    worker {
        Import-NativeRuntime
        Invoke-Logged 'worker-smoke.log' 'cargo' (@('run', '--locked', '--release', '-p', 'brotypist', '--features', 'native-smoke') + $targetArguments + @('--example', 'worker_smoke', '--', '--app', $app, '--output', (Join-Path $env:NATIVE_REPORTS 'worker'), '--preset', 'tiny', '--timeout-seconds', '180'))
    }
    desktop {
        Import-NativeRuntime
        # Canonical UI smoke requires a distinct, genuinely empty disposable
        # profile, not the configuration touched by CLI/worker smoke.
        $env:INKLOW_HOME = Join-Path $env:RUNNER_TEMP 'brotypist-native-ui-profile'
        if (Test-Path -LiteralPath $env:INKLOW_HOME) { throw 'UI smoke profile must not already exist.' }
        if ($IsWindows) {
            New-Item -ItemType Directory -Path $env:INKLOW_HOME | Out-Null
        } else {
            # Only create this new owned path; never chmod an existing profile.
            Invoke-Logged 'ui-profile-create.log' '/usr/bin/install' @('-d', '-m', '700', $env:INKLOW_HOME)
        }
        $env:CI = 'true'
        Invoke-DesktopEnvironment 'native-desktop-smoke.log' 'cargo' (@('run', '--locked', '--release', '-p', 'brotypist') + $targetArguments + @('--features', 'native-smoke', '--example', 'native_desktop_smoke', '--', (Join-Path $env:NATIVE_REPORTS 'desktop')))
    }
    portable {
        # Canonical assembly owns DLL selection, relocatable loader paths,
        # resources and binary identity. Development mode is never releasable.
        # Deliberate macOS ad-hoc signing needs no secret and grants no TCC access.
        if ($IsMacOS) { $env:INKLOW_CODESIGN_IDENTITY = '-' }
        $bundleName = if ($IsMacOS) { 'BroTypist.app' } else { 'BroTypist' }
        $bundle = Join-Path $env:NATIVE_PORTABLE $bundleName
        $xtask = @('run', '--locked', '-p', 'xtask') + $targetArguments + @('--')
        Invoke-DesktopEnvironment 'development-build.log' 'cargo' ($xtask + @('build', '--development', '--output', $bundle))
        Invoke-DesktopEnvironment 'development-verify.log' 'cargo' ($xtask + @('verify', '--development', '--bundle', $bundle))
        # tar preserves Unix executable modes and hidden development provenance;
        # the GitHub artifact transport alone does not preserve file modes.
        $archive = Join-Path $env:NATIVE_PORTABLE 'BroTypist-development-not-for-ota.tar.gz'
        Invoke-Logged 'development-archive.log' 'tar' @('-czf', $archive, '-C', $env:NATIVE_PORTABLE, $bundleName)
        Get-FileHash -Algorithm SHA256 -LiteralPath $archive | Format-List | Out-String | Set-Content -LiteralPath (Join-Path $env:NATIVE_REPORTS 'development-archive-sha256.log')
    }
}
