# Owned by xtask smoke-installer. Never invoke against a workstation or normal profile.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$InputFile, [Parameter(Mandatory)][string]$Work)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$receipt = [ordered]@{
    schema = 1; tool = 'windows-core-installer-smoke'; passed = $false
    signing = 'unsigned'; publication = 'held; no Windows OTA/publication proof'
    full_installer_matrix_passed = $false; physical_typing_verified = $false
    observations = [System.Collections.Generic.List[object]]::new()
    diagnostics = [System.Collections.Generic.List[string]]::new()
    required_gates = [ordered]@{
        upgrade = @{ status = 'not_exercised'; reason = 'no genuine prior canonical package supplied' }
        rollback = @{ status = 'not_exercised'; reason = 'no genuine rollback-failure fixture supplied' }
    }
}
$owned = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$products = [System.Collections.Generic.List[string]]::new()
$logs = [System.Collections.Generic.List[string]]::new()
$transactionUnknown = $false
$installAttempted = $false
$profile = $null
$sentinels = @()
$originalHome = [Environment]::GetEnvironmentVariable('INKLOW_HOME', 'Process')
$originalPath = [Environment]::GetEnvironmentVariable('PATH', 'Process')
$originalRun = $null
$runObserved = $false
$installDirectory = $null
$shortcut = $null

function Require([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Equal($Actual, $Expected, [string]$Message) { Require ($Actual -ceq $Expected) $Message }
function Observe([string]$Check, $Evidence) { $receipt.observations.Add(@{ check = $Check; passed = $true; evidence = $Evidence }) }
function Redact([string]$Text) {
    foreach ($path in @($Work, $env:RUNNER_TEMP, $env:USERPROFILE)) {
        if (-not [string]::IsNullOrWhiteSpace($path)) { $Text = $Text.Replace($path, '<private-path>') }
    }
    $Text = [regex]::Replace($Text, '(?im)(token|password|secret|authorization|api[_-]?key)\s*[:=]\s*[^\r\n]+', '$1=<redacted>')
    if ($Text.Length -gt 8192) { $Text = $Text.Substring(0, 8192) + '[truncated]' }
    return $Text
}
function Bounded-Text([string]$Path, [int]$Limit = 32768) {
    $stream = [IO.File]::OpenRead($Path)
    try {
        $prefix = [byte[]]::new(3)
        $prefixCount = $stream.Read($prefix, 0, $prefix.Length)
        $encoding = [Text.Encoding]::UTF8
        $wide = $false
        if ($prefixCount -ge 2 -and $prefix[0] -eq 255 -and $prefix[1] -eq 254) { $encoding = [Text.Encoding]::Unicode; $wide = $true }
        if ($prefixCount -ge 2 -and $prefix[0] -eq 254 -and $prefix[1] -eq 255) { $encoding = [Text.Encoding]::BigEndianUnicode; $wide = $true }
        [long]$offset = [Math]::Max(0, $stream.Length - $Limit)
        if ($wide -and $offset % 2 -ne 0) { $offset++ }
        [void]$stream.Seek($offset, [IO.SeekOrigin]::Begin)
        $buffer = [byte[]]::new([int][Math]::Min($Limit, $stream.Length - $offset))
        $count = $stream.Read($buffer, 0, $buffer.Length)
        return $encoding.GetString($buffer, 0, $count)
    } finally { $stream.Dispose() }
}
function New-PrivateDirectory([string]$Path) {
    Require (-not (Test-Path -LiteralPath $Path)) 'Private smoke directory already exists'
    [void][IO.Directory]::CreateDirectory($Path)
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $acl.SetOwner($sid)
    $acl.SetAccessRuleProtection($true, $false)
    $rule = [Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    $acl.AddAccessRule($rule)
    Set-Acl -LiteralPath $Path -AclObject $acl
}
function Hash-Equal([string]$Path, [string]$Hash) {
    Require (Test-Path -LiteralPath $Path -PathType Leaf) 'Required synthetic/payload file is missing'
    Require (((Get-Item -LiteralPath $Path).Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Linked file refused'
    Equal ((Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()) $Hash.ToLowerInvariant() 'Payload/sentinel SHA256 differs'
}
function Assert-Payload([string]$Directory, $Package) {
    foreach ($row in $Package.files) {
        $path = Join-Path $Directory $row.path
        Hash-Equal $path $row.sha256
        Equal ((Get-Item -LiteralPath $path).Length) ([long]$row.size) 'Payload byte length differs'
    }
    $entries = @(Get-ChildItem -LiteralPath $Directory -Recurse -Force)
    Require (@($entries | Where-Object { ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 }).Count -eq 0) 'Linked payload refused'
    Equal (@($entries | Where-Object { -not $_.PSIsContainer }).Count) (@($Package.files).Count) 'Exact payload inventory differs'
}
function Assert-Sentinels {
    foreach ($sentinel in $sentinels) { Hash-Equal $sentinel.path $sentinel.sha256 }
}
function Read-Run {
    $key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
    if (-not (Test-Path -LiteralPath $key)) { return @{} }
    $item = Get-Item -LiteralPath $key
    $values = [ordered]@{}
    foreach ($name in @('Inklow', 'BroTypist')) { $values[$name] = $item.GetValue($name, $null, 'DoNotExpandEnvironmentNames') }
    return $values
}
function Assert-Run { Equal ((Read-Run | ConvertTo-Json -Compress)) ($originalRun | ConvertTo-Json -Compress) 'Autostart registration changed' }
function Guid-Code($Package) { return '{' + ([string]$Package.product_code).ToUpperInvariant() + '}' }
function Msi-Call($Object, [string]$Method, [object[]]$Arguments = @()) {
    # Explicit IDispatch invocation matches the existing Windows oracle and
    # avoids PowerShell 7 COM adapter differences.
    return $Object.GetType().InvokeMember($Method, [Reflection.BindingFlags]::InvokeMethod, $null, $Object, $Arguments)
}
function Property($Database, [string]$Name, [switch]$Optional) {
    $view = Msi-Call $Database 'OpenView' @("SELECT ``Value`` FROM ``Property`` WHERE ``Property`` = '$Name'")
    try {
        [void](Msi-Call $view 'Execute')
        $row = Msi-Call $view 'Fetch'
        if ($null -eq $row) { if ($Optional) { return '' }; throw 'Required MSI property is missing' }
        try { return [string]$row.GetType().InvokeMember('StringData', [Reflection.BindingFlags]::GetProperty, $null, $row, @([int]1)) }
        finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($row) }
    } finally { [void](Msi-Call $view 'Close'); [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($view) }
}
function Validate-Msi($Package) {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $database = $null
    try {
        $database = Msi-Call $installer 'OpenDatabase' @([string]$Package.artifacts.msi.path, [int]0)
        Equal (Property $database 'ProductCode') (Guid-Code $Package) 'MSI product identity differs'
        Equal (Property $database 'UpgradeCode') ('{' + ([string]$Package.upgrade_code).ToUpperInvariant() + '}') 'MSI family identity differs'
        Equal (Property $database 'ProductName') 'BroTypist' 'MSI product name differs'
        Equal (Property $database 'ProductVersion') $Package.version 'MSI version differs'
        Equal (Property $database 'INKLOW_SOURCE_COMMIT') $Package.source_commit 'MSI source differs'
        Equal (Property $database 'INKLOW_ARCHITECTURE') 'x86_64' 'MSI architecture differs'
        Equal (Property $database 'ALLUSERS' -Optional) '' 'Machine-scope MSI refused'
        # Canonical WiX package has no executable custom actions and authors
        # only HKCU registration. Never run an arbitrary fixture as trusted MSI.
        $tableView = Msi-Call $database 'OpenView' @('SELECT `Name` FROM `_Tables`')
        $tables = [System.Collections.Generic.List[string]]::new()
        try {
            [void](Msi-Call $tableView 'Execute')
            while ($null -ne ($row = Msi-Call $tableView 'Fetch')) {
                try { $tables.Add([string]$row.GetType().InvokeMember('StringData', [Reflection.BindingFlags]::GetProperty, $null, $row, @([int]1))) }
                finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($row) }
            }
        } finally { [void](Msi-Call $tableView 'Close'); [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($tableView) }
        if ($tables.Contains('CustomAction')) {
            $view = Msi-Call $database 'OpenView' @('SELECT `Type`, `Source`, `Target` FROM `CustomAction`')
            try {
                [void](Msi-Call $view 'Execute')
                while ($null -ne ($row = Msi-Call $view 'Fetch')) {
                    try {
                        $type = [int]$row.GetType().InvokeMember('IntegerData', [Reflection.BindingFlags]::GetProperty, $null, $row, @([int]1))
                        $source = [string]$row.GetType().InvokeMember('StringData', [Reflection.BindingFlags]::GetProperty, $null, $row, @([int]2))
                        $target = [string]$row.GetType().InvokeMember('StringData', [Reflection.BindingFlags]::GetProperty, $null, $row, @([int]3))
                        # WiX SetProperty emits a non-executable type-51 action.
                        Require ($type -eq 51 -and $source -ceq 'ARPINSTALLLOCATION' -and $target -ceq '[INSTALLFOLDER]') 'Executable/noncanonical MSI custom action refused'
                    } finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($row) }
                }
            } finally { [void](Msi-Call $view 'Close'); [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($view) }
        }
        $view = Msi-Call $database 'OpenView' @('SELECT `Root` FROM `Registry`')
        try {
            [void](Msi-Call $view 'Execute')
            while ($null -ne ($row = Msi-Call $view 'Fetch')) {
                try { Equal ([int]$row.GetType().InvokeMember('IntegerData', [Reflection.BindingFlags]::GetProperty, $null, $row, @([int]1))) 1 'MSI authors non-HKCU registry changes' }
                finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($row) }
            }
        } finally { [void](Msi-Call $view 'Close'); [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($view) }
    } finally {
        if ($null -ne $database) { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($database) }
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($installer)
    }
}
function Product-Info([string]$Product, [uint32]$Context, [string]$Name) {
    [uint32]$length = 0
    # A PowerShell $null string binds as empty; MSI requires a native NULL SID.
    $code = [BroTypistInstallerNative]::MsiGetProductInfoExW($Product, [System.Management.Automation.Language.NullString]::Value, $Context, $Name, $null, [ref]$length)
    if ($code -eq 1605) { return $null }
    Require ($code -in @(0, 234)) "MSI product query failed (code=$code; context=$Context; property=$Name; stage=size)"
    $value = [Text.StringBuilder]::new([int]$length + 1)
    $length = [uint32]$value.Capacity
    $code = [BroTypistInstallerNative]::MsiGetProductInfoExW($Product, [System.Management.Automation.Language.NullString]::Value, $Context, $Name, $value, [ref]$length)
    Equal $code ([uint32]0) "MSI product query failed (code=$code; context=$Context; property=$Name; stage=value)"
    return $value.ToString()
}
function Contexts([string]$Product) {
    foreach ($context in @(1, 2, 4)) {
        $state = Product-Info $Product $context 'State'
        if ($null -ne $state) { @{ context = $context; state = $state } }
    }
}
function Arp-Keys([string]$Product) {
    $parts = $Product.Trim('{}').Split('-')
    $packed = ''
    foreach ($part in $parts[0..2]) { $chars = $part.ToCharArray(); [array]::Reverse($chars); $packed += -join $chars }
    $tail = $parts[3] + $parts[4]
    for ($index = 0; $index -lt $tail.Length; $index += 2) { $packed += [string]$tail[$index + 1] + [string]$tail[$index] }
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    # This exact SID-scoped MSI database entry is not machine installation.
    # It is read-only here; no probe code writes HKLM.
    return @("HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$Product", "HKLM:\Software\Microsoft\Windows\CurrentVersion\Installer\UserData\$sid\Products\$packed\InstallProperties")
}
function Assert-Installed($Package) {
    $product = Guid-Code $Package
    $contexts = @(Contexts $product)
    Require ($contexts.Count -eq 1 -and $contexts[0].context -eq 2 -and $contexts[0].state -eq '5') 'Product is not exclusively per-user unmanaged'
    $assignment = Product-Info $product 2 'AssignmentType'
    Equal $assignment '0' 'MSI assignment is not per-user'
    Equal (Product-Info $product 2 'InstalledProductName') 'BroTypist' 'Registered name differs'
    Equal (Product-Info $product 2 'VersionString') $Package.version 'Registered version differs'
    Equal ((Product-Info $product 2 'InstallLocation').TrimEnd('\')) $installDirectory.TrimEnd('\') 'Default per-user install directory differs'
    $machineArp = Test-Path -LiteralPath "HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$product"
    $machineArp32 = Test-Path -LiteralPath "HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\$product"
    Observe 'native_per_user_context' @{ contexts = $contexts; assignment_type = $assignment; machine_arp_present = $machineArp; machine_arp_32_present = $machineArp32 }
    Require (-not $machineArp) 'Machine ARP registration exists'
    Require (-not $machineArp32) '32-bit machine ARP registration exists'
    $arpKeys = @(Arp-Keys $product | Where-Object { Test-Path -LiteralPath $_ })
    Require ($arpKeys.Count -gt 0) 'Per-user Installed Apps registration is missing'
    foreach ($key in $arpKeys) {
        $arp = Get-ItemProperty -LiteralPath $key
        Equal $arp.DisplayName 'BroTypist' 'ARP name differs'
        Equal $arp.DisplayVersion $Package.version 'ARP version differs'
        Equal $arp.InstallLocation.TrimEnd('\') $installDirectory.TrimEnd('\') 'ARP location differs'
        Equal $arp.WindowsInstaller 1 'ARP is not a Windows Installer product'
        Require ($arp.PSObject.Properties.Name -notcontains 'SystemComponent' -or $arp.SystemComponent -eq 0) 'ARP product is hidden'
        Require (-not [string]::IsNullOrWhiteSpace($arp.UninstallString) -and $arp.UninstallString.IndexOf($product, [StringComparison]::OrdinalIgnoreCase) -ge 0) 'ARP uninstall command does not reference owned product'
        Require (-not [string]::IsNullOrWhiteSpace($arp.DisplayIcon)) 'ARP icon is missing'
        $icon = ($arp.DisplayIcon -replace ',\s*-?\d+$', '').Trim('"')
        Require (Test-Path -LiteralPath $icon -PathType Leaf) 'ARP icon file is missing'
    }
    $registration = Get-ItemProperty -LiteralPath 'HKCU:\Software\Inklow\Installer'
    Equal $registration.Version $Package.version 'HKCU version differs'
    Equal $registration.SourceCommit $Package.source_commit 'HKCU source differs'
    Equal $registration.InstallDirectory.TrimEnd('\') $installDirectory.TrimEnd('\') 'HKCU location differs'
    Assert-Payload $installDirectory $Package
    Require (Test-Path -LiteralPath $shortcut -PathType Leaf) 'Start Menu shortcut missing'
    $shell = New-Object -ComObject WScript.Shell
    $link = $null
    try {
        $link = $shell.CreateShortcut($shortcut)
        Equal $link.TargetPath (Join-Path $installDirectory 'BroTypist.exe') 'Start Menu target differs'
        Equal $link.WorkingDirectory.TrimEnd('\') $installDirectory.TrimEnd('\') 'Start Menu working directory differs'
    } finally {
        if ($null -ne $link) { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($link) }
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($shell)
    }
    Assert-Sentinels
    Assert-Run
}
function Msi-Transaction([string]$Stage, [string[]]$Arguments, [switch]$ExpectedFailure) {
    $log = Join-Path $Work ($Stage + '.msi.log')
    $logs.Add($log)
    $process = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32\msiexec.exe') -ArgumentList ($Arguments + @('/qn', '/norestart', '/L*v', ('"' + $log + '"'))) -PassThru
    $owned.Add($process)
    [void]$process.Handle
    if (-not $process.WaitForExit(180000)) {
        $script:transactionUnknown = $true
        try {
            $process.Kill()
            [void]$process.WaitForExit(10000)
        } catch { $receipt.diagnostics.Add('Owned timed-out msiexec client could not be stopped; MSI service is never killed') }
        throw 'Owned msiexec timed out; Windows Installer transaction state is unknown; no racing retry/uninstall attempted'
    }
    $code = $process.ExitCode
    $rejected = $false
    if ($ExpectedFailure -and $code -eq 1603) {
        $text = Bounded-Text $log 65536
        $rejected = $text.Contains('A newer version of BroTypist is already installed.')
    }
    $receipt.observations.Add(@{ check = $Stage; passed = if ($ExpectedFailure) { $rejected } else { $code -in @(0, 3010) }; exit_code = $code; reboot_required = $code -eq 3010; authored_downgrade_message_observed = $rejected })
    Require ($code -ne 1641) 'Installer initiated an unexpected reboot'
    if ($ExpectedFailure) { Require $rejected 'Expected authored MSI downgrade rejection was not observed' }
    else { Require ($code -in @(0, 3010)) 'MSI transaction failed' }
    return $code
}
function Invoke-NativeCli([string]$Executable, [string[]]$Arguments, [string]$Stage, [int]$Timeout = 30) {
    $stdout = Join-Path $Work ($Stage + '.stdout.log')
    $stderr = Join-Path $Work ($Stage + '.stderr.log')
    $logs.Add($stdout); $logs.Add($stderr)
    # ProcessStartInfo.ArgumentList preserves spaces/Unicode and avoids shell quoting.
    $start = [Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    $start.WorkingDirectory = $profile
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new(); $process.StartInfo = $start
    Require ($process.Start()) 'Could not start owned CLI'
    $owned.Add($process)
    # Copy asynchronously to owned files, never wait on full redirected pipes.
    $outFile = [IO.File]::Create($stdout); $errFile = [IO.File]::Create($stderr)
    $outCopy = [BroTypistInstallerNative]::Capture($process.StandardOutput.BaseStream, $outFile, 65536)
    $errCopy = [BroTypistInstallerNative]::Capture($process.StandardError.BaseStream, $errFile, 65536)
    try {
        if (-not $process.WaitForExit($Timeout * 1000)) {
            $process.Kill($true)
            [void]$process.WaitForExit(10000)
            throw 'Owned CLI timed out; only its owned process tree was stopped'
        }
        Require ($outCopy.Wait(10000) -and $errCopy.Wait(10000)) 'Owned CLI output pipes did not close'
        Require ($outCopy.Result -le 65536 -and $errCopy.Result -le 65536) 'CLI output exceeded bounded capture limit'
        Equal $process.ExitCode 0 'Owned CLI exit code differs'
    } finally { $outFile.Dispose(); $errFile.Dispose() }
    Require ((Get-Item -LiteralPath $stdout).Length -le 65536) 'CLI stdout exceeds limit'
    return Bounded-Text $stdout 65536
}
function Probe-Cli([string]$Directory, $Package, [string]$Stage) {
    $app = Join-Path $Directory 'BroTypist-cli.exe'
    Equal ((Invoke-NativeCli $app @('--version') ($Stage + '-version')).Trim()) ('BroTypist ' + $Package.version) 'Actual CLI version differs'
    $doctor = Invoke-NativeCli $app @('--paused', 'doctor') ($Stage + '-doctor') | ConvertFrom-Json
    Equal $doctor.version $Package.version 'Doctor version differs'
    Equal $doctor.platform 'windows' 'Doctor platform differs'
    Equal $doctor.runtime 'Rust' 'Doctor runtime differs'
    Equal $doctor.shell 'Win32' 'Doctor shell differs'
    Equal ([IO.Path]::GetFullPath($doctor.settings)) (Join-Path $profile 'settings.json') 'Doctor did not use private profile'
    Equal $doctor.global_enabled $false 'Doctor did not remain paused'
    Equal $doctor.native.hook_running $false 'Doctor unexpectedly installed an input hook'
    Observe $Stage @{ version = $doctor.version; shell = $doctor.shell; private_profile = $true; paused = $true; input_hook_running = $false }
    Assert-Sentinels
}
function Capture-Children([Diagnostics.Process]$Root, $Children) {
    $parents = [System.Collections.Generic.Queue[int]]::new(); $parents.Enqueue($Root.Id)
    $seen = [System.Collections.Generic.HashSet[int]]::new()
    while ($parents.Count -gt 0) {
        $parentId = $parents.Dequeue()
        if (-not $seen.Add($parentId)) { continue }
        foreach ($child in @(Get-CimInstance -ClassName Win32_Process -Filter "ParentProcessId=$parentId")) {
            if ($child.CreationDate -lt $Root.StartTime) { continue }
            $parents.Enqueue([int]$child.ProcessId)
            if (-not $Children.ContainsKey([int]$child.ProcessId)) {
                try {
                    $process = [Diagnostics.Process]::GetProcessById([int]$child.ProcessId)
                    [void]$process.Handle
                    $Children.Add($process.Id, $process); $owned.Add($process)
                } catch [ArgumentException] { } # Child exited between enumeration/open.
            }
        }
    }
}
function Probe-Gui {
    $start = [Diagnostics.ProcessStartInfo]::new((Join-Path $installDirectory 'BroTypist.exe'))
    $start.UseShellExecute = $false; $start.WorkingDirectory = $profile
    foreach ($arg in @('--paused', '--settings', '--no-tray', '--pane', 'about')) { $start.ArgumentList.Add($arg) }
    $gui = [Diagnostics.Process]::new(); $gui.StartInfo = $start
    Require ($gui.Start()) 'Could not start owned installed GUI'
    $owned.Add($gui)
    [void]$gui.Handle
    $children = [System.Collections.Generic.Dictionary[int,System.Diagnostics.Process]]::new()
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    $window = [IntPtr]::Zero
    do {
        Require (-not $gui.HasExited) 'GUI exited before visible Settings'
        Capture-Children $gui $children
        $window = [BroTypistInstallerNative]::FindSettings([uint32]$gui.Id)
        if ($window -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    Require ($window -ne [IntPtr]::Zero) 'Owned visible Settings HWND not observed before deadline'
    $button = [BroTypistInstallerNative]::GetDlgItem($window, 13)
    Require ($button -ne [IntPtr]::Zero) 'Canonical Quit control ID 13 missing'
    Require ([BroTypistInstallerNative]::OwnedVisibleQuit($window, $button, [uint32]$gui.Id)) 'Canonical Quit is not a visible enabled native BUTTON owned by GUI'
    Capture-Children $gui $children
    [UIntPtr]$result = [UIntPtr]::Zero
    # Invoke the actual visible button; never send WM_QUIT/WM_CLOSE or terminate
    # a process as a success shortcut. Native Controller owns worker shutdown.
    $sent = [BroTypistInstallerNative]::SendMessageTimeoutW($button, 0x00F5, [UIntPtr]::Zero, [IntPtr]::Zero, 0x0002, 5000, [ref]$result)
    Require ($sent -ne [IntPtr]::Zero) 'Visible Quit button invocation timed out/failed'
    Require ($gui.WaitForExit(30000)) 'GUI failed to exit after real Quit action'
    Equal $gui.ExitCode 0 'GUI Quit did not exit cleanly'
    foreach ($child in $children.Values) { Require ($child.WaitForExit(10000)) 'GUI-owned worker/child outlived Quit'; Equal $child.ExitCode 0 'GUI-owned child failed during Quit' }
    # Query once more for children created during the shutdown interval, not just
    # those present when Settings first appeared.
    Capture-Children $gui $children
    foreach ($child in $children.Values) { Require ($child.HasExited) 'New GUI-owned child remained after Quit' }
    Observe 'installed_native_settings_quit' @{ process_id = $gui.Id; window_class = 'BroTypist.Settings'; visible = $true; control_id = 13; action = 'BM_CLICK on visible owned native Quit BUTTON'; exit_code = $gui.ExitCode; child_process_ids = @($children.Keys); owned_children_exited = $true; worker_observation = if ($children.Count -eq 0) { 'paused GUI created no observable worker process; GUI controller exited cleanly' } else { 'observed GUI children exited after canonical Quit' } }
    Assert-Sentinels
    Assert-Run
}

try {
    Require ($IsWindows -and [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq [Runtime.InteropServices.Architecture]::X64) 'Native Windows x86_64 required'
    Require ($env:BROTYPIST_INSTALLER_SMOKE_DISPOSABLE -ceq '1' -and $env:CI -ceq 'true' -and $env:GITHUB_ACTIONS -ceq 'true' -and $env:RUNNER_ENVIRONMENT -ceq 'github-hosted' -and $env:RUNNER_OS -ceq 'Windows') 'Explicit disposable GitHub-hosted Windows guard required'
    $Work = [IO.Path]::GetFullPath($Work)
    Require (Test-Path -LiteralPath $Work -PathType Container) 'Owned evidence work directory is missing'
    Require (-not (Test-Path -LiteralPath (Join-Path $Work 'observations.json'))) 'Refusing existing evidence output'
    $input = Get-Content -LiteralPath $InputFile -Raw | ConvertFrom-Json
    $package = $input.package
    Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class BroTypistInstallerNative {
    public static async System.Threading.Tasks.Task<long> Capture(System.IO.Stream source,System.IO.Stream destination,int limit) {
        byte[] buffer=new byte[8192]; long total=0; int retained=0;
        for (;;) {
            int count=await source.ReadAsync(buffer,0,buffer.Length).ConfigureAwait(false);
            if(count==0) break;
            int write=Math.Min(count,limit-retained);
            if(write>0) { await destination.WriteAsync(buffer,0,write).ConfigureAwait(false); retained+=write; }
            total=Math.Min(long.MaxValue-count,total)+count;
        }
        await destination.FlushAsync().ConfigureAwait(false);
        return total;
    }
    [DllImport("msi.dll", CharSet=CharSet.Unicode, ExactSpelling=true)] public static extern uint MsiGetProductInfoExW(string product,string sid,uint context,string property,StringBuilder value,ref uint length);
    [DllImport("msi.dll", CharSet=CharSet.Unicode, ExactSpelling=true)] public static extern uint MsiEnumRelatedProductsW(string upgrade,uint reserved,uint index,StringBuilder product);
    delegate bool EnumProc(IntPtr hwnd,IntPtr data);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback,IntPtr data);
    [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr hwnd,int id);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd,out uint process);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool IsWindowEnabled(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool IsHungAppWindow(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool IsIconic(IntPtr hwnd);
    [StructLayout(LayoutKind.Sequential)] struct Rect { public int left,top,right,bottom; }
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd,out Rect rect);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassNameW(IntPtr hwnd,StringBuilder text,int length);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowTextW(IntPtr hwnd,StringBuilder text,int length);
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr hwnd,uint flags);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, ExactSpelling=true)] public static extern IntPtr SendMessageTimeoutW(IntPtr hwnd,uint message,UIntPtr wp,IntPtr lp,uint flags,uint timeout,out UIntPtr result);
    static string Class(IntPtr hwnd) { var text=new StringBuilder(256); GetClassNameW(hwnd,text,text.Capacity); return text.ToString(); }
    public static IntPtr FindSettings(uint pid) {
        IntPtr found=IntPtr.Zero;
        EnumWindows((hwnd,data)=>{ uint owner; GetWindowThreadProcessId(hwnd,out owner); if(owner==pid && IsWindowVisible(hwnd) && !IsIconic(hwnd) && !IsHungAppWindow(hwnd) && Class(hwnd)=="BroTypist.Settings") { found=hwnd; return false; } return true; },IntPtr.Zero);
        return found;
    }
    public static bool OwnedVisibleQuit(IntPtr root,IntPtr button,uint pid) {
        uint owner,rootOwner; GetWindowThreadProcessId(button,out owner); GetWindowThreadProcessId(root,out rootOwner);
        var text=new StringBuilder(256); GetWindowTextW(button,text,text.Capacity);
        Rect bounds,control;
        if(!GetWindowRect(root,out bounds) || !GetWindowRect(button,out control)) return false;
        bool inside=control.right>control.left && control.bottom>control.top && control.left>=bounds.left && control.top>=bounds.top && control.right<=bounds.right && control.bottom<=bounds.bottom;
        return owner==pid && rootOwner==pid && inside && Class(root)=="BroTypist.Settings" && !IsIconic(root) && GetAncestor(button,2)==root && IsWindowVisible(button) && IsWindowEnabled(button) && Class(button)=="Button" && text.ToString().Replace("&","")=="Quit BroTypist";
    }
}
'@
    $installDirectory = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Programs\BroTypist'
    $shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'BroTypist.lnk'
    Require (-not (Test-Path -LiteralPath $installDirectory) -and -not (Test-Path -LiteralPath $shortcut) -and -not (Test-Path -LiteralPath 'HKCU:\Software\Inklow\Installer')) 'Existing BroTypist installation/registration refused'
    foreach ($base in @($env:ProgramFiles, ${env:ProgramFiles(x86)})) {
        if (-not [string]::IsNullOrWhiteSpace($base)) { Require (-not (Test-Path -LiteralPath (Join-Path $base 'BroTypist'))) 'Existing machine BroTypist installation refused' }
    }
    Require (@(Get-Process -Name 'BroTypist','BroTypist-cli' -ErrorAction SilentlyContinue).Count -eq 0) 'Existing BroTypist process refused'
    $family = '{' + ([string]$package.upgrade_code).ToUpperInvariant() + '}'
    $related = [Text.StringBuilder]::new(39)
    Equal ([BroTypistInstallerNative]::MsiEnumRelatedProductsW($family, 0, 0, $related)) ([uint32]259) 'Existing installer family refused'
    foreach ($candidate in @($input.previous, $package)) {
        if ($null -eq $candidate) { continue }
        Hash-Equal $candidate.artifacts.msi.path $candidate.artifacts.msi.sha256
        Hash-Equal $candidate.artifacts.zip.path $candidate.artifacts.zip.sha256
        Validate-Msi $candidate
        $code = Guid-Code $candidate
        Require (@(Contexts $code).Count -eq 0) 'Existing product context refused'
        foreach ($key in @("HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$code", "HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$code", "HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\$code")) { Require (-not (Test-Path -LiteralPath $key)) 'Existing ARP product refused' }
        $products.Add($code)
    }
    $originalRun = Read-Run; $runObserved = $true
    $root = Join-Path $env:RUNNER_TEMP ('brotypist-installer-' + [Guid]::NewGuid().ToString('N'))
    New-PrivateDirectory $root
    $profile = Join-Path $root 'profile'
    New-PrivateDirectory $profile
    [Environment]::SetEnvironmentVariable('INKLOW_HOME', $profile, 'Process')
    # Strip build-tree DLL directories: only the executable directory, native OS
    # DLL search and Windows system PATH remain available to installed/ZIP apps.
    [Environment]::SetEnvironmentVariable('PATH', "$env:SystemRoot\System32;$env:SystemRoot", 'Process')
    $settings = Join-Path $profile 'settings.json'
    @{
        global_enabled = $false; setup_completed = $true; launch_at_login = $false
        check_updates_automatically = $false; record_history = $false; record_unaccepted = $false
        statistics_enabled = $false; clipboard_context = $false; screen_context = $false
        screenshot_context = $false; personalization_strength = 0; model_path = ''; acceleration = 'cpu'
    } | ConvertTo-Json | Set-Content -LiteralPath $settings -Encoding utf8NoBOM
    [void][IO.Directory]::CreateDirectory((Join-Path $profile 'models'))
    [IO.File]::WriteAllText((Join-Path $profile 'models\synthetic.gguf'), 'synthetic continuity sentinel, not model weights')
    [IO.File]::WriteAllText((Join-Path $profile 'history.enc'), 'synthetic encrypted history continuity sentinel')
    [IO.File]::WriteAllText((Join-Path $profile 'preserve-user-data.txt'), 'synthetic data only; no user writing')
    $sentinels = @($settings, (Join-Path $profile 'models\synthetic.gguf'), (Join-Path $profile 'history.enc'), (Join-Path $profile 'preserve-user-data.txt')) | ForEach-Object { @{ path = $_; sha256 = (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant() } }
    Observe 'isolation_and_family_preflight' @{ disposable_hosted_ci = $true; preexisting_install = $false; install_directory = '%LOCALAPPDATA%\Programs\BroTypist'; installer_family = $family; signing = 'unsigned'; manually_authored_hklm_changes = $false }
    $extract = Join-Path $root 'portable-original'
    Expand-Archive -LiteralPath $package.artifacts.zip.path -DestinationPath $extract
    $portable = Join-Path $extract 'BroTypist'
    Assert-Payload $portable $package
    # Moving the complete extracted tree proves it is not tied to its extraction
    # location. CWD is a private profile, not the bundle or build output.
    $relocated = Join-Path $root 'relocated portable Unicode é'
    [IO.Directory]::Move($portable, $relocated)
    Require (-not (Test-Path -LiteralPath $portable)) 'Portable relocation did not remove original location'
    Assert-Payload $relocated $package
    Probe-Cli $relocated $package 'relocated_portable_cli_doctor'
    Observe 'portable_zip_exact_relocation' @{ exact_manifest = $true; source_location_absent = $true; original_build_dll_path_removed = $true }
    $installAttempted = $true
    [void](Msi-Transaction 'fresh_install' @('/i', ('"' + $package.artifacts.msi.path + '"')))
    Assert-Installed $package
    if ($null -ne $input.previous) {
        [void](Msi-Transaction 'uninstall_before_prior_fixture' @('/x', (Guid-Code $package)))
        Require (@(Contexts (Guid-Code $package)).Count -eq 0 -and -not (Test-Path -LiteralPath $installDirectory) -and -not (Test-Path -LiteralPath $shortcut)) 'Fresh install cleanup before upgrade fixture failed'
        [void](Msi-Transaction 'prior_install' @('/i', ('"' + $input.previous.artifacts.msi.path + '"')))
        Assert-Installed $input.previous
        Probe-Cli $installDirectory $input.previous 'prior_installed_cli_doctor'
        [void](Msi-Transaction 'major_upgrade' @('/i', ('"' + $package.artifacts.msi.path + '"')))
        Assert-Installed $package
        Require (@(Contexts (Guid-Code $input.previous)).Count -eq 0) 'Prior product remained registered after major upgrade'
        $receipt.required_gates.upgrade = @{ status = 'passed'; from_version = $input.previous.version; to_version = $package.version; prior_product_absent = $true }
        # A genuine old package exercises authored downgrade rejection, not an
        # invented test-only failure flag. This is not an in-transaction rollback.
        [void](Msi-Transaction 'real_downgrade_rejected' @('/i', ('"' + $input.previous.artifacts.msi.path + '"')) -ExpectedFailure)
        Assert-Installed $package
        Require (@(Contexts (Guid-Code $input.previous)).Count -eq 0) 'Rejected downgrade registered prior product'
    }
    # Deliberately damage one installer-owned file; force repair of all package
    # files so HKCU component keypaths cannot mask a missing native executable.
    Remove-Item -LiteralPath (Join-Path $installDirectory 'BroTypist-cli.exe') -Force
    Require (-not (Test-Path -LiteralPath (Join-Path $installDirectory 'BroTypist-cli.exe'))) 'Repair damage was not applied'
    [void](Msi-Transaction 'repair' @('/fa', ('"' + $package.artifacts.msi.path + '"')))
    Assert-Installed $package
    Observe 'repair_restored_deleted_cli' @{ exact_payload_restored = $true; sentinels_unchanged = $true }
    Probe-Cli $installDirectory $package 'installed_cli_doctor'
    if ($null -ne $input.worker) {
        # worker_smoke verifies the pinned tiny catalogue bytes itself, creates
        # its own child profile, and exercises real canonical worker shutdown.
        $workerHome = Join-Path $root 'worker-root'; New-PrivateDirectory $workerHome
        $workerOutput = Join-Path $Work 'tiny-worker'
        [Environment]::SetEnvironmentVariable('INKLOW_HOME', $workerHome, 'Process')
        try {
            [void](Invoke-NativeCli $input.worker @('--app', (Join-Path $installDirectory 'BroTypist-cli.exe'), '--output', $workerOutput, '--preset', 'tiny', '--model-path', $input.model, '--timeout-seconds', '180') 'installed-tiny-worker' 1200)
            $workerReceipt = Get-Content -LiteralPath (Join-Path $workerOutput 'worker-smoke.json') -Raw | ConvertFrom-Json
            Equal $workerReceipt.passed $true 'Canonical installed Tiny worker smoke failed'
            Observe 'installed_tiny_worker' @{ passed = $true; preset = 'tiny'; canonical_worker_receipt = 'tiny-worker/worker-smoke.json'; model_verified_by_canonical_smoke = $true }
        } finally { [Environment]::SetEnvironmentVariable('INKLOW_HOME', $profile, 'Process') }
    } else { $receipt.observations.Add(@{ check = 'installed_tiny_worker'; status = 'not_exercised'; reason = 'explicit canonical worker executable and cached model QA environment variables not provided'; passed = $null }) }
    Probe-Gui
} catch {
    $receipt.diagnostics.Add((Redact $_.Exception.Message))
} finally {
    # Forced cleanup is only a failure path; it can never make passed=true.
    foreach ($process in $owned) {
        try {
            if (-not $process.HasExited) {
                $receipt.diagnostics.Add('Owned process required forced failure cleanup')
                $process.Kill($true)
                Require ($process.WaitForExit(10000)) 'Owned process did not exit during bounded failure cleanup'
            }
        } catch { $receipt.diagnostics.Add((Redact ('Owned process cleanup: ' + $_.Exception.Message))) }
    }
    if ($installAttempted) {
        if ($transactionUnknown) { $receipt.diagnostics.Add('Installer transaction state unknown; uninstall safely withheld instead of racing MSI service') }
        else {
            try {
                foreach ($product in $products) {
                    if (@(Contexts $product).Count -gt 0) { [void](Msi-Transaction 'uninstall_transaction' @('/x', $product)) }
                }
                foreach ($product in $products) {
                    Require (@(Contexts $product).Count -eq 0) 'Uninstall left MSI registration'
                    foreach ($key in @(Arp-Keys $product)) { Require (-not (Test-Path -LiteralPath $key)) 'Uninstall left per-user ARP registration' }
                }
                Require (-not (Test-Path -LiteralPath $installDirectory) -and -not (Test-Path -LiteralPath $shortcut) -and -not (Test-Path -LiteralPath 'HKCU:\Software\Inklow\Installer')) 'Uninstall left payload, shortcut or HKCU registration'
                $remaining = [Text.StringBuilder]::new(39)
                Equal ([BroTypistInstallerNative]::MsiEnumRelatedProductsW($family, 0, 0, $remaining)) ([uint32]259) 'Uninstall left installer family registration'
                Observe 'uninstall' @{ payload_absent = $true; shortcut_absent = $true; all_owned_product_contexts_absent = $true; installer_family_absent = $true }
            } catch { $receipt.diagnostics.Add((Redact ('Uninstall cleanup: ' + $_.Exception.Message))) }
        }
    }
    try {
        if ($sentinels.Count -gt 0) {
            Assert-Sentinels
            Observe 'synthetic_profile_model_history_settings_unchanged' @{ sha256 = @($sentinels | ForEach-Object { @{ file = [IO.Path]::GetFileName($_.path); sha256 = $_.sha256 } }); unchanged_at_final_check = $true; uninstall_observed = @($receipt.observations | Where-Object { $_.check -eq 'uninstall' -and $_.passed }).Count -eq 1 }
        }
        if ($runObserved) { Assert-Run }
    } catch { $receipt.diagnostics.Add((Redact ('Continuity cleanup: ' + $_.Exception.Message))) }
    [Environment]::SetEnvironmentVariable('INKLOW_HOME', $originalHome, 'Process')
    [Environment]::SetEnvironmentVariable('PATH', $originalPath, 'Process')
    # Keep only bounded/redacted diagnostics, not arbitrary MSI/CLI environment
    # dumps. Raw logs were confined to the ACL-private work directory.
    foreach ($log in $logs) {
        try {
            if (Test-Path -LiteralPath $log -PathType Leaf) {
                if ($receipt.diagnostics.Count -gt 0 -and $receipt.diagnostics.Count -lt 8) { $receipt.diagnostics.Add((Redact (Bounded-Text $log 4096))) }
                Remove-Item -LiteralPath $log -Force
            }
        } catch { $receipt.diagnostics.Add('Could not remove owned raw diagnostic log') }
    }
    $receipt.passed = $receipt.diagnostics.Count -eq 0 -and $installAttempted -and @($receipt.observations | Where-Object { $_.check -eq 'installed_native_settings_quit' }).Count -eq 1 -and @($receipt.observations | Where-Object { $_.check -eq 'uninstall' -and $_.passed }).Count -eq 1
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($receipt | ConvertTo-Json -Depth 16) + "`n")
    $stream = [IO.File]::Open((Join-Path $Work 'observations.json'), [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $stream.Write($bytes, 0, $bytes.Length); $stream.Flush($true) } finally { $stream.Dispose() }
}
if (-not $receipt.passed) { [Console]::Error.WriteLine('Windows core installer smoke failed; see redacted observations'); exit 1 }
Write-Output 'Windows core installer smoke passed; upgrade/rollback gates remain separately scoped'
