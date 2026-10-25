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

function Format-SmokeError([System.Management.Automation.ErrorRecord]$Record) {
    $diagnostic = @(
        "ErrorRecord: $($Record.Exception.ToString())"
        "FullyQualifiedErrorId: $($Record.FullyQualifiedErrorId)"
        "CategoryInfo: $($Record.CategoryInfo)"
        "Position: $($Record.InvocationInfo.PositionMessage)"
        "ScriptStackTrace: $($Record.ScriptStackTrace)"
    ) -join "`n"
    # Keep CI output and receipt errors useful without unbounded exception dumps.
    if ($diagnostic.Length -gt 8192) { return $diagnostic.Substring(0, 8192) + "`n[diagnostic truncated]" }
    return $diagnostic
}

# Preflight failures happen before the install/uninstall receipt exists. Retain
# their original ErrorRecord location and stack instead of only the message.
trap {
    [Console]::Error.WriteLine((Format-SmokeError $_))
    break
}
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
    Assert-Equal $actual "BroTypist $Version" "CLI executable version"
    return $actual
}

function Read-MsiProperty($Database, [string]$Name, [switch]$AllowMissing) {
    $query = "SELECT ``Value`` FROM ``Property`` WHERE ``Property`` = '$Name'"
    $view = $Database.GetType().InvokeMember("OpenView", [System.Reflection.BindingFlags]::InvokeMethod, $null, $Database, @($query))
    if ($null -eq $view) { throw "MSI OpenView returned no view for property '$Name'." }
    try {
        [void]$view.GetType().InvokeMember("Execute", [System.Reflection.BindingFlags]::InvokeMethod, $null, $view, $null)
        $row = $view.GetType().InvokeMember("Fetch", [System.Reflection.BindingFlags]::InvokeMethod, $null, $view, $null)
        if ($null -eq $row) {
            if ($AllowMissing) { return "" }
            throw "MSI Property table is missing required property '$Name'."
        }
        try {
            # StringData is a 1-based indexed COM property, not a method. Use
            # explicit IDispatch property-get rather than PS7 method dispatch.
            # https://learn.microsoft.com/en-us/windows/win32/msi/record-stringdata
            $value = $row.GetType().InvokeMember("StringData", [System.Reflection.BindingFlags]::GetProperty, $null, $row, @([int]1))
            if ($null -eq $value) { throw "MSI StringData returned null for property '$Name'." }
            return [string]$value
        }
        finally { [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($row) }
    }
    finally {
        [void]$view.GetType().InvokeMember("Close", [System.Reflection.BindingFlags]::InvokeMethod, $null, $view, $null)
        [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($view)
    }
}

function Read-Autostart {
    $key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
    if (-not (Test-Path -LiteralPath $key)) { return $null }
    return (Get-Item -LiteralPath $key).GetValue("Inklow", $null)
}

# Query MSI's own product/context contract, not an assumed ARP registry hive.
# https://learn.microsoft.com/en-us/windows/win32/api/msi/nf-msi-msigetproductinfoexw
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class InklowMsi {
    [DllImport("msi.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    public static extern uint MsiGetProductInfoExW(
        string product, string sid, uint context, string property,
        StringBuilder value, ref uint length);
}
'@

function Read-ProductInfo([uint32]$Context, [string]$Property, [switch]$AllowAbsentProduct) {
    [uint32]$length = 0
    $code = [InklowMsi]::MsiGetProductInfoExW($productCode, $null, $Context, $Property, $null, [ref]$length)
    if ($code -eq 1605 -and $AllowAbsentProduct) { return $null }
    if ($code -ne 0) { throw "MsiGetProductInfoExW($productCode, context=$Context, $Property) failed: $code" }
    $buffer = [System.Text.StringBuilder]::new([int]$length + 1)
    $length = [uint32]$buffer.Capacity
    $code = [InklowMsi]::MsiGetProductInfoExW($productCode, $null, $Context, $Property, $buffer, [ref]$length)
    if ($code -ne 0) { throw "MsiGetProductInfoExW($productCode, context=$Context, $Property) failed: $code" }
    return $buffer.ToString()
}

function Read-ProductContexts {
    # Current user only (NULL SID); context 4 is the machine instance.
    foreach ($context in @(1, 2, 4)) {
        $state = Read-ProductInfo $context "State" -AllowAbsentProduct
        if ($null -ne $state) { [pscustomobject]@{ context = $context; state = $state } }
    }
}

function Get-PackedProductCode([string]$Code) {
    $parts = $Code.Trim('{}').Split('-')
    $packed = ""
    foreach ($part in $parts[0..2]) {
        $chars = $part.ToCharArray()
        [array]::Reverse($chars)
        $packed += -join $chars
    }
    $tail = $parts[3] + $parts[4]
    for ($index = 0; $index -lt $tail.Length; $index += 2) {
        $packed += [string]$tail[$index + 1] + [string]$tail[$index]
    }
    return $packed
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
$database = $null
try {
    $database = $installer.GetType().InvokeMember("OpenDatabase", [System.Reflection.BindingFlags]::InvokeMethod, $null, $installer, @($msiPath, [int]0))
    if ($null -eq $database) { throw "MSI OpenDatabase returned no database for '$msiPath'." }
    $productCode = Read-MsiProperty $database "ProductCode"
    Assert-Equal ($productCode.ToUpperInvariant()) ("{" + $package.product_code.ToUpperInvariant() + "}") "MSI ProductCode"
    Assert-Equal (Read-MsiProperty $database "ProductName") "BroTypist" "MSI ProductName"
    Assert-Equal (Read-MsiProperty $database "ProductVersion") $Version "MSI ProductVersion"
    Assert-Equal ((Read-MsiProperty $database "UpgradeCode").ToUpperInvariant()) ("{" + $package.upgrade_code.ToUpperInvariant() + "}") "MSI UpgradeCode"
    Assert-Equal (Read-MsiProperty $database "INKLOW_SOURCE_COMMIT") $SourceCommit "MSI source metadata"
    Assert-Equal (Read-MsiProperty $database "INKLOW_ARCHITECTURE") "x86_64" "MSI architecture metadata"
    Assert-Equal (Read-MsiProperty $database "ALLUSERS" -AllowMissing) "" "MSI must not request machine installation"
}
finally {
    if ($null -ne $database) { [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($database) }
    [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($installer)
}

$installDirectory = Join-Path ([Environment]::GetFolderPath("LocalApplicationData")) "Programs\BroTypist"
$shortcut = Join-Path ([Environment]::GetFolderPath("Programs")) "BroTypist.lnk"
$userSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$packedProductCode = Get-PackedProductCode $productCode
# MSI's per-user InstallProperties live under HKLM, scoped by the installing
# user's SID. HKLM storage does not imply MSIINSTALLCONTEXT_MACHINE.
# Check only this exact product's registration locations, never other products.
$uninstallKeys = @(
    "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$productCode"
    "HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$productCode"
    "HKLM:\Software\Microsoft\Windows\CurrentVersion\Installer\UserData\$userSid\Products\$packedProductCode\InstallProperties"
)
$existingContexts = @(Read-ProductContexts)
if ($existingContexts.Count -gt 0 -or (Test-Path -LiteralPath $installDirectory) -or
    @($uninstallKeys | Where-Object { Test-Path -LiteralPath $_ }).Count -gt 0 -or (Test-Path -LiteralPath $shortcut)) {
    throw "Refusing to modify an existing BroTypist installation. Use a disposable CI runner."
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
    installer_user_sid = $userSid; installed_contexts = @(); uninstall_contexts = @()
    arp_entries = @()
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
    $portableBundle = Join-Path $portable "BroTypist"
    Assert-Payload $portableBundle $package.files
    $receipt.portable_cli_version = Invoke-Version (Join-Path $portableBundle "BroTypist-cli.exe") "portable"

    $installAttempted = $true
    $receipt.install_exit_code = Invoke-Msi @("/i", "`"$msiPath`"") (Join-Path $reportDirectory "windows-msi-install.log")
    Assert-Payload $installDirectory $package.files
    $receipt.payload_verified = $true
    $installedMarker = Get-Content -LiteralPath (Join-Path $installDirectory ".inklow-build.json") -Raw | ConvertFrom-Json
    Assert-Equal $installedMarker.version $Version "Installed bundle version"
    Assert-Equal $installedMarker.source_commit $SourceCommit "Installed bundle source"
    Assert-Equal $installedMarker.architecture "x86_64" "Installed bundle architecture"
    $receipt.installed_contexts = @(Read-ProductContexts)
    Assert-Equal $receipt.installed_contexts.Count 1 "Exactly one installed MSI context"
    $installedContext = $receipt.installed_contexts[0]
    Assert-Equal $installedContext.context 2 "MSI per-user unmanaged context"
    Assert-Equal $installedContext.state "5" "MSI installed product state"
    Assert-Equal (Read-ProductInfo 2 "AssignmentType") "0" "MSI per-user assignment"
    Assert-Equal (Read-ProductInfo 2 "InstalledProductName") "BroTypist" "Registered MSI name"
    Assert-Equal (Read-ProductInfo 2 "VersionString") $Version "Registered MSI version"
    Assert-Equal ((Read-ProductInfo 2 "InstallLocation").TrimEnd('\')) ($installDirectory.TrimEnd('\')) "Registered MSI location"
    $receipt.arp_entries = @(
        foreach ($key in $uninstallKeys) {
            if (Test-Path -LiteralPath $key) {
                $entry = Get-ItemProperty -LiteralPath $key
                [pscustomobject]@{ path = $key; values = $entry }
            }
        }
    )
    if ($receipt.arp_entries.Count -eq 0) { throw "Installed MSI has no Add/Remove Programs registration for this product/user." }
    foreach ($entry in $receipt.arp_entries) {
        $arp = $entry.values
        Assert-Equal $arp.DisplayName "BroTypist" "Uninstall entry name"
        Assert-Equal $arp.DisplayVersion $Version "Uninstall entry version"
        Assert-Equal ($arp.InstallLocation.TrimEnd('\')) ($installDirectory.TrimEnd('\')) "Uninstall entry install directory"
        Assert-Equal $arp.WindowsInstaller 1 "Uninstall entry Windows Installer marker"
        if ($arp.PSObject.Properties.Name -contains "SystemComponent" -and $arp.SystemComponent -ne 0) {
            throw "MSI registration hides BroTypist from Installed Apps."
        }
        if ([string]::IsNullOrWhiteSpace($arp.UninstallString) -or
            $arp.UninstallString.IndexOf($productCode, [StringComparison]::OrdinalIgnoreCase) -lt 0) {
            throw "Uninstall entry does not reference this MSI product."
        }
        if ([string]::IsNullOrWhiteSpace($arp.DisplayIcon)) { throw "Uninstall entry has no icon." }
        $displayIcon = ($arp.DisplayIcon -replace ',\s*-?\d+$', '').Trim('"')
        if (-not (Test-Path -LiteralPath $displayIcon -PathType Leaf)) { throw "Uninstall entry icon is missing: $displayIcon" }
    }
    if (-not (Test-Path -LiteralPath $shortcut -PathType Leaf)) { throw "Start Menu shortcut was not installed." }
    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($shortcut)
    try {
        Assert-Equal $link.TargetPath (Join-Path $installDirectory "BroTypist.exe") "Start Menu executable target"
        Assert-Equal ($link.WorkingDirectory.TrimEnd('\')) ($installDirectory.TrimEnd('\')) "Start Menu working directory"
    }
    finally {
        [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($link)
        [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($shell)
    }
    $receipt.installed_cli_version = Invoke-Version (Join-Path $installDirectory "BroTypist-cli.exe") "installed"

    # A real native settings window, paused in an isolated profile, with no
    # typing/model-download action. UI Automation invokes its existing Quit.
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    $gui = Start-Process -FilePath (Join-Path $installDirectory "BroTypist.exe") `
        -ArgumentList @("--paused", "--settings", "--no-tray", "--pane", "about") -PassThru `
        -RedirectStandardOutput (Join-Path $reportDirectory "windows-gui.stdout.txt") `
        -RedirectStandardError (Join-Path $reportDirectory "windows-gui.stderr.txt")
    [void]$gui.Handle
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        Start-Sleep -Milliseconds 200
        $gui.Refresh()
        if ($gui.HasExited) { throw "Installed GUI exited before displaying settings (exit $($gui.ExitCode))." }
        $visible = $gui.MainWindowHandle -ne [IntPtr]::Zero -and $gui.Responding
    } until ($visible -or [DateTime]::UtcNow -ge $deadline)
    if (-not $visible) { throw "Installed GUI did not expose a responsive native BroTypist Settings window within 30 seconds." }
    $receipt.native_settings_visible = $true
    $window = [System.Windows.Automation.AutomationElement]::FromHandle($gui.MainWindowHandle)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, "Quit BroTypist"
    )
    $quit = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if ($null -eq $quit) { throw "Native settings window did not expose its Quit BroTypist action." }
    $invoke = $quit.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
    $invoke.Invoke()
    $receipt.native_settings_quit_invoked = $true
    if (-not $gui.WaitForExit(15000)) { throw "GUI did not exit after invoking Quit BroTypist." }
    $receipt.gui_exit_code = $gui.ExitCode
    Assert-Equal $gui.ExitCode 0 "GUI Quit exit code"
    foreach ($sentinel in $sentinels) { Assert-Hash $sentinel.path $sentinel.sha256 }
    $run = Read-Autostart
    Assert-Equal $run $oldRun "Installer/paused GUI must not change autostart"
}
catch { $errors.Add((Format-SmokeError $_)) }
finally {
    try {
        if ($null -ne $gui -and -not $gui.HasExited) {
            # Failure cleanup only, never counted as a successful GUI smoke.
            $gui.Kill()
            $gui.WaitForExit()
        }
    }
    catch { $errors.Add("GUI failure cleanup: " + (Format-SmokeError $_)) }
    try {
        # ARP/metadata/GUI assertions can fail after a successful installation.
        # Always use MSI's installed/advertised state to decide cleanup.
        if ($installAttempted) {
            $receipt.uninstall_contexts = @(Read-ProductContexts)
            if ($receipt.uninstall_contexts.Count -gt 0) {
                $receipt.uninstall_exit_code = Invoke-Msi @("/x", $productCode) (Join-Path $reportDirectory "windows-msi-uninstall.log")
            }
            $remainingContexts = @(Read-ProductContexts)
            $remainingKeys = @($uninstallKeys | Where-Object { Test-Path -LiteralPath $_ })
            if ($remainingContexts.Count -gt 0 -or $remainingKeys.Count -gt 0 -or
                (Test-Path -LiteralPath $installDirectory) -or (Test-Path -LiteralPath $shortcut)) {
                throw "MSI uninstall left installed application files, shortcut or product registration behind."
            }
            if ($null -ne $receipt.uninstall_exit_code) { $receipt.uninstall_verified = $true }
            elseif ($errors.Count -eq 0) { throw "Installed product has no MSI registration to uninstall." }
        }
        foreach ($sentinel in $sentinels) { Assert-Hash $sentinel.path $sentinel.sha256 }
        $receipt.external_data_preserved = $true
    }
    catch { $errors.Add("Cleanup/uninstall: " + (Format-SmokeError $_)) }
    [Environment]::SetEnvironmentVariable("INKLOW_HOME", $oldHome, "Process")
    [Environment]::SetEnvironmentVariable("QT_QPA_PLATFORM", $oldQt, "Process")
    $receipt.errors = @($errors.ToArray())
    $receipt.status = if ($errors.Count -eq 0) { "passed" } else { "failed" }
    $receipt | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $reportPath -Encoding utf8NoBOM
}
if ($errors.Count -gt 0) { throw ($errors -join "`n") }
Write-Output "Windows MSI install, native settings/CLI, portable CLI, uninstall and external-data preservation passed. Receipt: $reportPath"
