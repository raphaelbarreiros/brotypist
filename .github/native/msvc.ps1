$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Use only Visual Studio tooling already supplied by the hosted Windows image.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
if (!(Test-Path -LiteralPath $vswhere)) { throw 'Hosted image is missing vswhere.exe.' }
$installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($installation)) {
    throw 'A native MSVC x64 installation is required.'
}
$developerCommand = Join-Path $installation 'Common7/Tools/VsDevCmd.bat'
if (!(Test-Path -LiteralPath $developerCommand)) { throw 'Installed Visual Studio has no VsDevCmd.bat.' }
$variables = & $env:ComSpec /d /s /c "`"`"$developerCommand`" -no_logo -arch=x64 -host_arch=x64 && set`""
if ($LASTEXITCODE -ne 0) { throw 'VsDevCmd.bat failed.' }
foreach ($line in $variables) {
    if ($line -match '^([^=]+)=(.*)$') {
        $name, $value = $Matches[1], $Matches[2]
        [Environment]::SetEnvironmentVariable($name, $value, 'Process')
    }
}
# Persist just the compiler/SDK environment needed by subsequent native steps,
# rather than copying the complete hosted runner environment into a file.
foreach ($name in @('PATH', 'INCLUDE', 'LIB', 'LIBPATH', 'VCINSTALLDIR', 'VCToolsInstallDir', 'VCToolsVersion', 'VSINSTALLDIR', 'VisualStudioVersion', 'WindowsSdkDir', 'WindowsSDKVersion', 'WindowsSdkBinPath', 'WindowsSdkVerBinPath', 'UniversalCRTSdkDir', 'UCRTVersion', 'VSCMD_ARG_HOST_ARCH', 'VSCMD_ARG_TGT_ARCH')) {
    $value = [Environment]::GetEnvironmentVariable($name, 'Process')
    if (![string]::IsNullOrWhiteSpace($value)) { "$name=$value" >> $env:GITHUB_ENV }
}
foreach ($command in @('cl.exe', 'link.exe', 'rc.exe', 'dumpbin.exe', 'cmake.exe')) {
    $resolved = Get-Command $command -CommandType Application -ErrorAction Stop
    "$command=$($resolved.Source)" | Tee-Object -FilePath "$env:NATIVE_REPORTS/msvc-tools.log" -Append
}
