# Real Windows Installer + extracted portable CLI + native settings-window smoke.
# Run on a disposable Windows x86_64 CI runner, never on a user's installation.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Msi,
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$SourceCommit,
    [string]$PackageReceipt = "reports/windows-package.json",
    [string]$Report = "reports/windows-installer-smoke.json"
)
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
if (-not [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Windows)) {
    throw "This smoke requires native Windows."
}
if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne [System.Runtime.InteropServices.Architecture]::X64) {
    throw "This smoke requires a Windows x86_64 runner."
}

function Assert-Equal($Actual, $Expected, [string]$Description) {
    if ($Actual -cne $Expected) { throw "${Description}: expected '$Expected', got '$Actual'." }
}

function Assert-Hash([string]$Path, [string]$Expected) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "Missing file: $Path" }
    Assert-Equal ((Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()) ($Expected.ToLowerInvariant()) "SHA256 of $Path"
}

function Assert-Payload([string]$Directory, $Manifest) {
    foreach ($item in $Manifest) {
        $path = Join-Path $Directory $item.path
        Assert-Hash $path $item.sha256
        Assert-Equal ((Get-Item -LiteralPath $path).Length) ([long]$item.size) "Length of $path"
    }
    $count = @(Get-ChildItem -LiteralPath $Directory -File -Recurse -Force).Count
    Assert-Equal $count (@($Manifest).Count) "Complete payload file count"
}

function Invoke-Msi([string[]]$Arguments, [string]$Log) {
    $process = Start-Process -FilePath "$env:SystemRoot\System32\msiexec.exe" -ArgumentList (
        $Arguments + @("/qn", "/norestart", "/L*v", "`"$Log`"")
    ) -PassThru
    [void]$process.Handle
    if (-not $process.WaitForExit(180000)) {
        throw "Windows Installer exceeded 180 seconds; the transaction may still be running. See $Log"
    }
    $code = $process.ExitCode
    # 3010 is success with a deferred reboot. Never retry an MSI failure. 1641
    # means a reboot was actually initiated, which /norestart must prevent here.
    if ($code -eq 1641) { throw "msiexec initiated an unexpected reboot despite /norestart; smoke cannot complete. See $Log" }
    if ($code -notin @(0, 3010)) { throw "msiexec failed with exit code $code; see $Log" }
    return $code
}

function Invoke-Version([string]$Executable, [string]$Prefix) {
    $stdout = Join-Path $reportDirectory "$Prefix-version.stdout.txt"
    $stderr = Join-Path $reportDirectory "$Prefix-version.stderr.txt"
    $process = Start-Process -FilePath $Executable -ArgumentList "--version" -PassThru -NoNewWindow `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    [void]$process.Handle
    if (-not $process.WaitForExit(30000)) {
        $process.Kill()
        $process.WaitForExit()
        throw "CLI --version timed out: $Executable"
    }
    Assert-Equal $process.ExitCode 0 "CLI --version exit code"
    $actual = (Get-Content -LiteralPath $stdout -Raw).Trim()
    Assert-Equal $actual "Inklow $Version" "CLI executable version"
    return $actual
}

function Read-MsiProperty($Database, [string]$Name) {
    $view = $Database.OpenView("SELECT ``Value`` FROM ``Property`` WHERE ``Property`` = '$Name'")
    try {
        $view.Execute()
        $row = $view.Fetch()
        if ($null -eq $row) { return "" }
        try { return $row.StringData(1) }
        finally { [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($row) }
    }
    finally {
        $view.Close()
        [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($view)
    }
}

function Read-Autostart {
    $key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
    if (-not (Test-Path -LiteralPath $key)) { return $null }
    return (Get-Item -LiteralPath $key).GetValue("Inklow", $null)
}

$msiPath = (Resolve-Path -LiteralPath $Msi).Path
$package = Get-Content -LiteralPath $PackageReceipt -Raw | ConvertFrom-Json
$reportPath = [System.IO.Path]::GetFullPath($Report)
$reportDirectory = Split-Path -Parent $reportPath
[void][System.IO.Directory]::CreateDirectory($reportDirectory)
Assert-Equal $package.application "com.inklow.Inklow" "Package application"
Assert-Equal $package.version $Version "Package version"
Assert-Equal $package.source_commit $SourceCommit "Package source commit"
Assert-Equal $package.architecture "x86_64" "Package architecture"
Assert-Equal $package.scope "perUser" "Package install scope"
Assert-Equal $package.signing "unsigned" "Package signing declaration"
Assert-Hash $msiPath $package.artifacts.msi.sha256
$zipPath = (Resolve-Path -LiteralPath $package.artifacts.zip.path).Path
Assert-Hash $zipPath $package.artifacts.zip.sha256

$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.OpenDatabase($msiPath, 0)
try {
    $productCode = Read-MsiProperty $database "ProductCode"
    Assert-Equal ($productCode.ToUpperInvariant()) ("{" + $package.product_code.ToUpperInvariant() + "}") "MSI ProductCode"
    Assert-Equal (Read-MsiProperty $database "ProductName") "Inklow" "MSI ProductName"
    Assert-Equal (Read-MsiProperty $database "ProductVersion") $Version "MSI ProductVersion"
    Assert-Equal ((Read-MsiProperty $database "UpgradeCode").ToUpperInvariant()) ("{" + $package.upgrade_code.ToUpperInvariant() + "}") "MSI UpgradeCode"
    Assert-Equal (Read-MsiProperty $database "INKLOW_SOURCE_COMMIT") $SourceCommit "MSI source metadata"
    Assert-Equal (Read-MsiProperty $database "INKLOW_ARCHITECTURE") "x86_64" "MSI architecture metadata"
    Assert-Equal (Read-MsiProperty $database "ALLUSERS") "" "MSI must not request machine installation"
}
finally {
    [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($database)
    [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($installer)
}

$installDirectory = Join-Path ([Environment]::GetFolderPath("LocalApplicationData")) "Programs\Inklow"
$shortcut = Join-Path ([Environment]::GetFolderPath("Programs")) "Inklow.lnk"
$uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$productCode"
if ((Test-Path -LiteralPath $installDirectory) -or (Test-Path -LiteralPath $uninstallKey) -or (Test-Path -LiteralPath $shortcut)) {
    throw "Refusing to modify an existing Inklow installation. Use a disposable CI runner."
}
$isolated = Join-Path ([System.IO.Path]::GetTempPath()) ("inklow-installer-smoke-" + [guid]::NewGuid().ToString("N"))
[void][System.IO.Directory]::CreateDirectory($isolated)
$settings = Join-Path $isolated "settings.json"
@{
    global_enabled = $false; setup_completed = $true; launch_at_login = $false
    record_history = $false; record_unaccepted = $false; statistics_enabled = $false
    model_path = ""; clipboard_context = $false; screen_context = $false
} | ConvertTo-Json | Set-Content -LiteralPath $settings -Encoding utf8NoBOM
$dataSentinel = Join-Path $isolated "preserve-user-data.txt"
$modelDirectory = Join-Path $isolated "models"
[void][System.IO.Directory]::CreateDirectory($modelDirectory)
$modelSentinel = Join-Path $modelDirectory "synthetic-preserve.gguf"
$historySentinel = Join-Path $isolated "history.enc"
# Deliberately not a model: this is an external data-preservation sentinel.
"synthetic data; no user writing" | Set-Content -LiteralPath $dataSentinel -Encoding utf8NoBOM
"synthetic model-cache sentinel, not model weights" | Set-Content -LiteralPath $modelSentinel -Encoding utf8NoBOM
"synthetic encrypted-history sentinel" | Set-Content -LiteralPath $historySentinel -Encoding utf8NoBOM
$sentinels = @($settings, $dataSentinel, $modelSentinel, $historySentinel) | ForEach-Object {
    @{ path = $_; sha256 = (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant() }
}
$receipt = [ordered]@{
    application = "com.inklow.Inklow"; version = $Version; source_commit = $SourceCommit
    product_code = $productCode; upgrade_code = $package.upgrade_code
    install_directory = $installDirectory; start_menu_shortcut = $shortcut
    msi_sha256 = $package.artifacts.msi.sha256; signing = "unsigned"
    external_profile = $isolated; sentinels = $sentinels
    install_exit_code = $null; uninstall_exit_code = $null
    portable_cli_version = $null; installed_cli_version = $null
    native_settings_visible = $false; native_settings_quit_invoked = $false; gui_exit_code = $null
    payload_verified = $false; uninstall_verified = $false; external_data_preserved = $false
    interactive_typing_verified = $false; status = "running"; errors = @()
}
$errors = [System.Collections.Generic.List[string]]::new()
$gui = $null
$installAttempted = $false
$oldHome = [Environment]::GetEnvironmentVariable("INKLOW_HOME", "Process")
$oldQt = [Environment]::GetEnvironmentVariable("QT_QPA_PLATFORM", "Process")
$oldRun = Read-Autostart
try {
    $env:INKLOW_HOME = $isolated
    $env:QT_QPA_PLATFORM = "windows"
    $portable = Join-Path $isolated "portable"
    Expand-Archive -LiteralPath $zipPath -DestinationPath $portable
    $portableBundle = Join-Path $portable "Inklow"
    Assert-Payload $portableBundle $package.files
    $receipt.portable_cli_version = Invoke-Version (Join-Path $portableBundle "Inklow-cli.exe") "portable"

    $installAttempted = $true
    $receipt.install_exit_code = Invoke-Msi @("/i", "`"$msiPath`"") (Join-Path $reportDirectory "windows-msi-install.log")
    Assert-Payload $installDirectory $package.files
    $receipt.payload_verified = $true
    $installedMarker = Get-Content -LiteralPath (Join-Path $installDirectory ".inklow-build.json") -Raw | ConvertFrom-Json
    Assert-Equal $installedMarker.version $Version "Installed bundle version"
    Assert-Equal $installedMarker.source_commit $SourceCommit "Installed bundle source"
    Assert-Equal $installedMarker.architecture "x86_64" "Installed bundle architecture"
    $arp = Get-ItemProperty -LiteralPath $uninstallKey
    Assert-Equal $arp.DisplayName "Inklow" "Uninstall entry name"
    Assert-Equal $arp.DisplayVersion $Version "Uninstall entry version"
    Assert-Equal ($arp.InstallLocation.TrimEnd('\')) ($installDirectory.TrimEnd('\')) "Uninstall entry install directory"
    if ([string]::IsNullOrWhiteSpace($arp.DisplayIcon)) { throw "Uninstall entry has no icon." }
    $displayIcon = ($arp.DisplayIcon -replace ',\s*-?\d+$', '').Trim('"')
    if (-not (Test-Path -LiteralPath $displayIcon -PathType Leaf)) { throw "Uninstall entry icon is missing: $displayIcon" }
    if (-not (Test-Path -LiteralPath $shortcut -PathType Leaf)) { throw "Start Menu shortcut was not installed." }
    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($shortcut)
    try {
        Assert-Equal $link.TargetPath (Join-Path $installDirectory "Inklow.exe") "Start Menu executable target"
        Assert-Equal ($link.WorkingDirectory.TrimEnd('\')) ($installDirectory.TrimEnd('\')) "Start Menu working directory"
    }
    finally {
        [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($link)
        [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($shell)
    }
    $receipt.installed_cli_version = Invoke-Version (Join-Path $installDirectory "Inklow-cli.exe") "installed"

    # A real native settings window, paused in an isolated profile, with no
    # typing/model-download action. UI Automation invokes its existing Quit.
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    $gui = Start-Process -FilePath (Join-Path $installDirectory "Inklow.exe") `
        -ArgumentList @("--paused", "--settings", "--no-tray", "--pane", "about") -PassThru `
        -RedirectStandardOutput (Join-Path $reportDirectory "windows-gui.stdout.txt") `
        -RedirectStandardError (Join-Path $reportDirectory "windows-gui.stderr.txt")
    [void]$gui.Handle
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        Start-Sleep -Milliseconds 200
        $gui.Refresh()
        if ($gui.HasExited) { throw "Installed GUI exited before displaying settings (exit $($gui.ExitCode))." }
        $visible = $gui.MainWindowHandle -ne [IntPtr]::Zero -and $gui.MainWindowTitle -eq "Inklow Settings" -and $gui.Responding
    } until ($visible -or [DateTime]::UtcNow -ge $deadline)
    if (-not $visible) { throw "Installed GUI did not expose a responsive native Inklow Settings window within 30 seconds." }
    $receipt.native_settings_visible = $true
    $window = [System.Windows.Automation.AutomationElement]::FromHandle($gui.MainWindowHandle)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, "Quit Inklow"
    )
    $quit = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if ($null -eq $quit) { throw "Native settings window did not expose its Quit Inklow action." }
    $invoke = $quit.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
    $invoke.Invoke()
    $receipt.native_settings_quit_invoked = $true
    if (-not $gui.WaitForExit(15000)) { throw "GUI did not exit after invoking Quit Inklow." }
    $receipt.gui_exit_code = $gui.ExitCode
    Assert-Equal $gui.ExitCode 0 "GUI Quit exit code"
    foreach ($sentinel in $sentinels) { Assert-Hash $sentinel.path $sentinel.sha256 }
    $run = Read-Autostart
    Assert-Equal $run $oldRun "Installer/paused GUI must not change autostart"
}
catch { $errors.Add($_.Exception.Message) }
finally {
    try {
        if ($null -ne $gui -and -not $gui.HasExited) {
            # Failure cleanup only, never counted as a successful GUI smoke.
            $gui.Kill()
            $gui.WaitForExit()
        }
        if ($installAttempted -and (Test-Path -LiteralPath $uninstallKey)) {
            $receipt.uninstall_exit_code = Invoke-Msi @("/x", $productCode) (Join-Path $reportDirectory "windows-msi-uninstall.log")
            if ((Test-Path -LiteralPath $installDirectory) -or (Test-Path -LiteralPath $shortcut) -or (Test-Path -LiteralPath $uninstallKey)) {
                throw "MSI uninstall left installed application files, shortcut or uninstall registration behind."
            }
            $receipt.uninstall_verified = $true
        }
        elseif ($installAttempted -and $errors.Count -eq 0) { throw "Installed product has no uninstall registration." }
        foreach ($sentinel in $sentinels) { Assert-Hash $sentinel.path $sentinel.sha256 }
        $receipt.external_data_preserved = $true
    }
    catch { $errors.Add("Cleanup/uninstall: " + $_.Exception.Message) }
    [Environment]::SetEnvironmentVariable("INKLOW_HOME", $oldHome, "Process")
    [Environment]::SetEnvironmentVariable("QT_QPA_PLATFORM", $oldQt, "Process")
    $receipt.errors = @($errors.ToArray())
    $receipt.status = if ($errors.Count -eq 0) { "passed" } else { "failed" }
    $receipt | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $reportPath -Encoding utf8NoBOM
}
if ($errors.Count -gt 0) { throw ($errors -join "`n") }
Write-Output "Windows MSI install, native settings/CLI, portable CLI, uninstall and external-data preservation passed. Receipt: $reportPath"
