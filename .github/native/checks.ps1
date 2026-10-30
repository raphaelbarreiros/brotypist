param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('prerequisites', 'check', 'test', 'build', 'cli', 'worker', 'desktop', 'portable', 'installer-tools', 'package', 'installer')]
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
        try { $message = ConvertFrom-Json -InputObject $line -AsHashtable -ErrorAction Stop } catch { continue }
        if ($message -isnot [Collections.IDictionary] -or !$message.Contains('reason')) { continue }
        if ($message['reason'] -eq 'build-script-executed' -and ($message['package_id'] -match '#brotypist@|/brotypist#')) {
            if (!$message.Contains('out_dir') -or [string]::IsNullOrWhiteSpace($message['out_dir'])) { throw 'Current Cargo native build-script record has no output directory.' }
            $message['out_dir']
        }
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
function Assert-HostedInstallerEnvironment {
    if ((!$IsLinux -and !$IsWindows) -or $env:CI -cne 'true' -or $env:GITHUB_ACTIONS -cne 'true' -or $env:RUNNER_ENVIRONMENT -cne 'github-hosted') {
        throw 'Installer package QA is restricted to explicitly disposable GitHub-hosted Linux/Windows runners.'
    }
    foreach ($name in @('NATIVE_PACKAGE_BUNDLE', 'NATIVE_PACKAGES', 'NATIVE_REPORTS', 'CARGO_TARGET_DIR')) {
        $path = [Environment]::GetEnvironmentVariable($name)
        if ([string]::IsNullOrWhiteSpace($path) -or ![IO.Path]::IsPathRooted($path)) { throw "Missing absolute installer isolation path: $name" }
        $relative = [IO.Path]::GetRelativePath([IO.Path]::GetFullPath($env:RUNNER_TEMP), [IO.Path]::GetFullPath($path))
        if ($relative -eq '.' -or $relative -eq '..' -or $relative.StartsWith('../') -or $relative.StartsWith('..\') -or [IO.Path]::IsPathRooted($relative)) {
            throw "Installer isolation path must be strictly beneath RUNNER_TEMP: $name"
        }
    }
}
function Get-VerifiedTinyModel {
    # Read the canonical catalogue instead of guessing a GGUF basename/cache glob.
    $catalogue = Get-Content -LiteralPath 'crates/brotypist/assets/models.json' -Raw | ConvertFrom-Json
    $tiny = @($catalogue | Where-Object { $_.id -ceq 'tiny' })
    if ($tiny.Count -ne 1 -or [IO.Path]::GetFileName($tiny[0].filename) -cne $tiny[0].filename) { throw 'Expected one canonical Tiny preset with a plain filename.' }
    if ($IsWindows) {
        $cache = Join-Path $env:LOCALAPPDATA 'inklow/Cache/models'
    } elseif ($IsLinux -and ![string]::IsNullOrWhiteSpace($env:XDG_CACHE_HOME)) {
        $cache = Join-Path $env:XDG_CACHE_HOME 'inklow/models'
    } else { throw 'Installer worker requires an explicit native CI model cache.' }
    $model = Join-Path $cache $tiny[0].filename
    if (!(Test-Path -LiteralPath $model -PathType Leaf)) { throw 'Earlier native CLI phase did not download the Tiny model.' }
    if ((Get-Item -LiteralPath $model).Length -ne $tiny[0].size_bytes -or (Get-FileHash -LiteralPath $model -Algorithm SHA256).Hash.ToLowerInvariant() -cne $tiny[0].sha256) {
        throw 'Cached Tiny model differs from the pinned catalogue.'
    }
    return $model
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
        if ($IsMacOS) {
            @{
                schema = 1; platform = 'darwin'; installer_evidence = $false; full_matrix = $false
                scope = 'development bundle only; actual signed installer smoke remains local/manual'
                reason = 'CI has no authorized distribution identity; ad-hoc development signing is not release or installer evidence'
            } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $env:NATIVE_REPORTS 'installer-ci-scope.json')
        }
    }
    installer-tools {
        Assert-HostedInstallerEnvironment
        if (!$IsWindows) { throw 'The pinned WiX tool is needed only on native Windows.' }
        $tools = Join-Path $env:RUNNER_TEMP 'brotypist-wix-5.0.2'
        if (Test-Path -LiteralPath $tools) { throw 'Refusing an existing installer-tool destination.' }
        $nuget = Join-Path $env:RUNNER_TEMP 'brotypist-installer-nuget.config'
        if (Test-Path -LiteralPath $nuget) { throw 'Refusing an existing installer-tool NuGet configuration.' }
        # Build-only tool, pinned to the version enforced by xtask. No publisher
        # identity, repository NuGet credentials, or third-party bootstrap script.
        '<configuration><packageSources><clear /><add key="nuget.org" value="https://api.nuget.org/v3/index.json" /></packageSources></configuration>' | Set-Content -LiteralPath $nuget
        Invoke-Logged 'dotnet-info.log' 'dotnet' @('--info')
        Invoke-Logged 'wix-install.log' 'dotnet' @('tool', 'install', 'wix', '--version', '5.0.2', '--tool-path', $tools, '--configfile', $nuget)
        $env:PATH = $tools + [IO.Path]::PathSeparator + $env:PATH
        Invoke-Logged 'wix-version.log' (Join-Path $tools 'wix.exe') @('--version')
        $tools >> $env:GITHUB_PATH
    }
    package {
        Assert-HostedInstallerEnvironment
        # A separate canonical clean build: never remove a development marker
        # or re-label the preceding development bundle as release-eligible.
        $xtask = @('run', '--locked', '-p', 'xtask') + $targetArguments + @('--')
        Invoke-DesktopEnvironment 'clean-package-build.log' 'cargo' ($xtask + @('build', '--output', $env:NATIVE_PACKAGE_BUNDLE))
        Invoke-DesktopEnvironment 'clean-package-verify.log' 'cargo' ($xtask + @('verify', '--bundle', $env:NATIVE_PACKAGE_BUNDLE))
        Invoke-DesktopEnvironment 'native-package.log' 'cargo' ($xtask + @('package', '--bundle', $env:NATIVE_PACKAGE_BUNDLE, '--output', $env:NATIVE_PACKAGES))
    }
    installer {
        Assert-HostedInstallerEnvironment
        $worker = Join-Path $targetDirectory "examples/worker_smoke$suffix"
        if (!(Test-Path -LiteralPath $worker -PathType Leaf)) { throw 'The earlier native build must provide the actual worker_smoke executable.' }
        $model = Get-VerifiedTinyModel
        $output = Join-Path $env:NATIVE_REPORTS 'installer'
        if (Test-Path -LiteralPath $output) { throw 'Refusing an existing installer evidence destination.' }
        @{
            schema = 1; full_matrix = $false; publication = 'held; no publication or OTA'
            scope = if ($IsLinux) { 'hosted root DEB install/same-version reinstall/purge, portable tar, owned X11/AT-SPI Quit and real installed worker' } else { 'hosted unsigned per-user MSI install/repair/uninstall, portable ZIP, owned native Quit and real installed worker' }
            previous_version_upgrade = 'not exercised; no genuine previous canonical package supplied'
            rollback = 'not exercised; no genuine reviewed failure fixture supplied'
            interactive_typing = 'not exercised by installer smoke'
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $env:NATIVE_REPORTS 'installer-ci-scope.json')
        if ($IsWindows) {
            Import-NativeRuntime
            $env:BROTYPIST_INSTALLER_SMOKE_DISPOSABLE = '1'
            $env:BROTYPIST_INSTALLER_SMOKE_WORKER_EXE = $worker
            $env:BROTYPIST_INSTALLER_SMOKE_MODEL_PATH = $model
            $xtask = Join-Path $targetDirectory "xtask$suffix"
            Invoke-Logged 'installer-smoke.log' $xtask @('smoke-installer', '--bundle', $env:NATIVE_PACKAGE_BUNDLE, '--assets', $env:NATIVE_PACKAGES, '--output', $output)
        } else {
            # The root probe cannot inherit runner-owned loaders or a desktop
            # session. Copy its exact native worker closure for the owned root
            # workspace; the helper runs xtask with env -i, its own D-Bus/Xvfb.
            $probe = Join-Path $env:RUNNER_TEMP 'brotypist-native-installer-probe'
            if (Test-Path -LiteralPath $probe) { throw 'Refusing an existing installer probe directory.' }
            New-Item -ItemType Directory -Path $probe | Out-Null
            Copy-Item -LiteralPath $worker -Destination (Join-Path $probe 'worker_smoke')
            $native = Get-Content -LiteralPath (Join-Path $env:NATIVE_REPORTS 'native-libraries-build.json') -Raw | ConvertFrom-Json
            foreach ($library in $native.libraries) { Copy-Item -LiteralPath $library -Destination $probe }
            $env:BROTYPIST_INSTALLER_SMOKE_ALLOW_SYSTEM_PACKAGE = 'isolated-ci-root'
            Invoke-Logged 'installer-smoke.log' '/usr/bin/sudo' @('--non-interactive', '/usr/bin/env', '-i',
                'PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin', 'CI=true', 'GITHUB_ACTIONS=true',
                "RUNNER_ENVIRONMENT=$env:RUNNER_ENVIRONMENT", 'BROTYPIST_INSTALLER_SMOKE_ALLOW_SYSTEM_PACKAGE=isolated-ci-root',
                '/usr/bin/bash', (Join-Path $PSScriptRoot 'linux-installer-ci.sh'),
                (Join-Path $targetDirectory 'xtask'), $probe, $model, $env:NATIVE_PACKAGE_BUNDLE, $env:NATIVE_PACKAGES, $output)
        }
    }
}
