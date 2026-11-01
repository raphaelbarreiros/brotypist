# CI-only launcher shared by canonical xtask and authenticated artifact reuse.
# Never distribute this script or use it against a normal Windows profile.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$InputFile,
    [Parameter(Mandatory)][string]$Work
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$metadata = [ordered]@{
    schema = 1; tool = 'windows-restricted-installer-client'; passed = $false
    genuine_uac_filtered_logon = $false; policy_scope = 'HKLM64 DisableMSI absent-to-DWORD-zero only; disposable GitHub runner'
    policy_modified = $false; restoration_verified = $false
    before = $null; during = $null; restored = $null; policy_key_created = $false
    transactional_policy_contract = 'KTM RegOpen/Create/DeleteKeyTransacted; conflict/unsupported fails closed; no ordinary write/delete fallback'
    quiescence_verified = $false
    global_acl_changes = $false; alternate_credentials = $false
    diagnostics = [Collections.Generic.List[string]]::new()
}
$base = $null; $key = $null; $createdKey = $false; $createdValue = $false
$nativeInitialized = $false
$policyPath = 'Software\Policies\Microsoft\Windows\Installer'
function Require([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Assert-DisableMsiPolicy($Policy) {
    Require (-not $Policy.present -or ($Policy.kind -ceq 'DWord' -and $Policy.value -eq 0)) 'Explicit restrictive/type-invalid DisableMSI refused; never overridden'
}
function Assert-NativeQuiescence($Outcome) {
    Require ($Outcome.quiescence.installer_transactions_quiescent -is [bool] -and $Outcome.quiescence.installer_transactions_quiescent -and $Outcome.quiescence.owned_processes_quiescent -is [bool] -and $Outcome.quiescence.owned_processes_quiescent) 'Native MSI transactions and owned processes must independently attest quiescence'
}
function Policy-Snapshot($Key) {
    $result = [ordered]@{ key_present = $null -ne $Key; values = [ordered]@{} }
    foreach ($name in @('DisableMSI', 'DisableUserInstalls', 'AlwaysInstallElevated')) {
        $present = $null -ne $Key -and $Key.GetValueNames() -icontains $name
        $kind = if ($present) { $Key.GetValueKind($name).ToString() } else { $null }
        $valueHash = $null
        if ($present) {
            $encoded = [Text.UTF8Encoding]::new($false).GetBytes((ConvertTo-Json -InputObject $Key.GetValue($name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) -Compress))
            $valueHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($encoded)).ToLowerInvariant()
        }
        $result.values[$name] = [ordered]@{ present = $present; kind = $kind; value = if ($kind -ceq 'DWord') { $Key.GetValue($name) } else { $null }; value_sha256 = $valueHash; non_dword_value_retained = $false }
    }
    return $result
}
function Initialize-InstallerNative {
    # SAFER rights restriction is not a UAC-filtered logon. Native diagnostic
    # 41285568481 proved elevation metadata stays 1 after rights restriction.
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;
public static class BroTypistRestrictedInstallerClient {
    public static Dictionary<string,object> PreparedToken;
    public static bool ClientLaunched, ClientExited, DeadlineExceeded;
    public static uint LastWaitError;
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
    struct Startup {
        public int size;
        public string reserved,desktop,title;
        public uint x,y,width,height,xChars,yChars,fill,flags;
        public ushort show,reservedCount;
        public IntPtr reservedBytes,input,output,error;
    }
    [StructLayout(LayoutKind.Sequential)] struct ProcessInfo {
        public IntPtr process,thread;
        public uint processId,threadId;
    }
    [StructLayout(LayoutKind.Sequential)] struct Label { public IntPtr sid; public uint attributes; }
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool SaferCreateLevel(uint scope,uint level,uint flags,out IntPtr handle,IntPtr reserved);
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool SaferComputeTokenFromLevel(IntPtr level,IntPtr source,out IntPtr token,uint flags,IntPtr reserved);
    [DllImport("advapi32.dll")] static extern bool SaferCloseLevel(IntPtr handle);
    [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool ConvertStringSidToSidW(string text,out IntPtr sid);
    [DllImport("advapi32.dll")] static extern uint GetLengthSid(IntPtr sid);
    [DllImport("advapi32.dll")] static extern IntPtr GetSidSubAuthorityCount(IntPtr sid);
    [DllImport("advapi32.dll")] static extern IntPtr GetSidSubAuthority(IntPtr sid,uint index);
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool SetTokenInformation(IntPtr token,int kind,IntPtr value,int size);
    [DllImport("advapi32.dll", EntryPoint="GetTokenInformation", SetLastError=true)] static extern bool TokenUInt(IntPtr token,int kind,out uint value,int size,out int needed);
    [DllImport("advapi32.dll", EntryPoint="GetTokenInformation", SetLastError=true)] static extern bool TokenBuffer(IntPtr token,int kind,IntPtr value,int size,out int needed);
    [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern bool CreateProcessAsUserW(IntPtr token,string application,StringBuilder command,IntPtr processAttributes,IntPtr threadAttributes,bool inherit,uint flags,IntPtr environment,string directory,ref Startup startup,out ProcessInfo process);
    [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr memory);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(IntPtr handle,uint timeout);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetExitCodeProcess(IntPtr handle,out uint code);
    static void Check(bool value) { if(!value) throw new Win32Exception(Marshal.GetLastWin32Error()); }
    static uint UInt(IntPtr token,int kind) {
        uint value; int needed; Check(TokenUInt(token,kind,out value,4,out needed)); return value;
    }
    static uint Integrity(IntPtr token) {
        int needed; TokenBuffer(token,25,IntPtr.Zero,0,out needed);
        if(needed<IntPtr.Size || needed>1024) throw new InvalidOperationException("Invalid integrity label length");
        IntPtr buffer=Marshal.AllocHGlobal(needed);
        try {
            Check(TokenBuffer(token,25,buffer,needed,out needed));
            IntPtr sid=Marshal.ReadIntPtr(buffer);
            byte count=Marshal.ReadByte(GetSidSubAuthorityCount(sid));
            if(count==0) throw new InvalidOperationException("Invalid integrity SID");
            return unchecked((uint)Marshal.ReadInt32(GetSidSubAuthority(sid,(uint)count-1)));
        } finally { Marshal.FreeHGlobal(buffer); }
    }
    public static int Run(string executable,string command,string directory,string expectedSid) {
        IntPtr level=IntPtr.Zero,token=IntPtr.Zero,sid=IntPtr.Zero,label=IntPtr.Zero;
        ProcessInfo process=new ProcessInfo();
        try {
            Check(SaferCreateLevel(2,0x20000,1,out level,IntPtr.Zero));
            // NORMALUSER removes Administrator/Power User rights. Flags=0
            // preserves AppLocker/software-restriction enforcement.
            Check(SaferComputeTokenFromLevel(level,IntPtr.Zero,out token,0,IntPtr.Zero));
            if(token==IntPtr.Zero) throw new InvalidOperationException("SAFER returned no token");
            Check(ConvertStringSidToSidW("S-1-16-8192",out sid));
            int size=checked(Marshal.SizeOf<Label>()+(int)GetLengthSid(sid));
            label=Marshal.AllocHGlobal(size);
            Marshal.StructureToPtr(new Label { sid=sid,attributes=0x20 },label,false);
            Check(SetTokenInformation(token,25,label,size));
            uint integrity=Integrity(token),restricted=UInt(token,21),tokenType=UInt(token,8);
            using(var identity=new WindowsIdentity(token)) {
                bool same=identity.User.Value==expectedSid,system=identity.IsSystem;
                bool administrator=new WindowsPrincipal(identity).IsInRole(WindowsBuiltInRole.Administrator);
                PreparedToken=new Dictionary<string,object> {
                    {"same_user",same}, {"is_system",system}, {"administrator_role_enabled",administrator},
                    {"token_elevation",UInt(token,20)}, {"token_elevation_type",UInt(token,18)},
                    {"integrity_rid",integrity}, {"token_has_restrictions",restricted}, {"token_type",tokenType},
                    {"client_launched",false}
                };
                if(!same || system || administrator || integrity!=8192 || restricted!=1 || tokenType!=1)
                    throw new InvalidOperationException("SAFER token lacks required same-user restricted primary rights");
            }
            var startup=new Startup { size=Marshal.SizeOf<Startup>() };
            // Same SID/profile/environment/desktop; no inherited handles,
            // alternate credentials or desktop/window-station ACL changes.
            Check(CreateProcessAsUserW(token,executable,new StringBuilder(command),IntPtr.Zero,IntPtr.Zero,false,0,IntPtr.Zero,directory,ref startup,out process));
            PreparedToken["client_launched"]=true;
            ClientLaunched=true;
            uint wait=WaitForSingleObject(process.process,900000);
            DeadlineExceeded=wait==258;
            // A deadline is failure, never permission to abandon ownership.
            // Wait errors retain the handle and the policy lease until a later
            // signalled exit or disposal of this entire hosted VM.
            while(wait!=0) {
                if(wait==0xffffffff) {
                    LastWaitError=unchecked((uint)Marshal.GetLastWin32Error());
                    System.Threading.Thread.Sleep(1000);
                }
                wait=WaitForSingleObject(process.process,0xffffffff);
            }
            ClientExited=true;
            if(LastWaitError!=0) throw new Win32Exception(unchecked((int)LastWaitError),"Owned child wait failed before exit was subsequently confirmed");
            if(DeadlineExceeded) throw new TimeoutException("Restricted probe exceeded its deadline; owned child exit was subsequently confirmed");
            uint code; Check(GetExitCodeProcess(process.process,out code));
            return unchecked((int)code);
        } finally {
            if(process.thread!=IntPtr.Zero) CloseHandle(process.thread);
            if(process.process!=IntPtr.Zero) CloseHandle(process.process);
            if(label!=IntPtr.Zero) Marshal.FreeHGlobal(label);
            if(sid!=IntPtr.Zero) LocalFree(sid);
            if(token!=IntPtr.Zero) CloseHandle(token);
            if(level!=IntPtr.Zero) SaferCloseLevel(level);
        }
    }
}
// No ordinary RegistryKey write/delete fallback is permitted. Registry reads,
// value writes/deletion, and conditional empty-key deletion share a KTM snapshot;
// any intervening non-transacted mutation aborts the transaction at commit.
public sealed class BroTypistInstallerPolicyTransaction : IDisposable {
    const string Path=@"Software\Policies\Microsoft\Windows\Installer";
    const string Parent=@"Software\Policies\Microsoft\Windows";
    static readonly IntPtr Hklm=new IntPtr(unchecked((int)0x80000002));
    const uint Access=0x3011f, View64=0x100;
    IntPtr transaction=IntPtr.Zero;
    bool committed;
    public Microsoft.Win32.RegistryKey Key { get; private set; }
    public bool Created { get; private set; }
    public bool Committed { get { return committed; } }
    [DllImport("KtmW32.dll", CharSet=CharSet.Unicode, ExactSpelling=true, SetLastError=true)]
    static extern IntPtr CreateTransaction(IntPtr attributes,IntPtr guid,uint options,uint isolation,uint flags,uint timeout,string description);
    [DllImport("KtmW32.dll", SetLastError=true)] static extern bool CommitTransaction(IntPtr transaction);
    [DllImport("KtmW32.dll", SetLastError=true)] static extern bool RollbackTransaction(IntPtr transaction);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("advapi32.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
    static extern int RegOpenKeyTransactedW(IntPtr root,string path,uint options,uint access,out IntPtr key,IntPtr transaction,IntPtr reserved);
    [DllImport("advapi32.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
    static extern int RegCreateKeyTransactedW(IntPtr root,string path,uint reserved,string kind,uint options,uint access,IntPtr attributes,out IntPtr key,out uint disposition,IntPtr transaction,IntPtr extended);
    [DllImport("advapi32.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
    static extern int RegDeleteKeyTransactedW(IntPtr root,string path,uint access,uint reserved,IntPtr transaction,IntPtr extended);
    [DllImport("advapi32.dll")] static extern int RegCloseKey(IntPtr key);
    static void Check(int error) { if(error!=0) throw new Win32Exception(error); }
    static Microsoft.Win32.RegistryKey Wrap(IntPtr handle) {
        return Microsoft.Win32.RegistryKey.FromHandle(new Microsoft.Win32.SafeHandles.SafeRegistryHandle(handle,true),Microsoft.Win32.RegistryView.Registry64);
    }
    public BroTypistInstallerPolicyTransaction() {
        transaction=CreateTransaction(IntPtr.Zero,IntPtr.Zero,0,0,0,30000,"BroTypist disposable CI DisableMSI fixture");
        if(transaction==IntPtr.Zero || transaction==new IntPtr(-1)) {
            transaction=IntPtr.Zero; throw new Win32Exception(Marshal.GetLastWin32Error());
        }
        try {
            IntPtr key; int result=RegOpenKeyTransactedW(Hklm,Path,0,Access,out key,transaction,IntPtr.Zero);
            if(result==2) return;
            Check(result); Key=Wrap(key);
        } catch { Dispose(); throw; }
    }
    public void CreateMissingKey() {
        if(Key!=null) throw new InvalidOperationException("Policy key is already open");
        IntPtr parent=IntPtr.Zero,key=IntPtr.Zero;
        try {
            // Require existing ancestors; creation is limited to Installer.
            Check(RegOpenKeyTransactedW(Hklm,Parent,0,0x2001f|View64,out parent,transaction,IntPtr.Zero));
            uint disposition;
            Check(RegCreateKeyTransactedW(parent,"Installer",0,null,0,Access,IntPtr.Zero,out key,out disposition,transaction,IntPtr.Zero));
            Key=Wrap(key); key=IntPtr.Zero; Created=disposition==1;
            if(!Created) throw new InvalidOperationException("Policy key appeared concurrently; transaction refused");
        } finally { if(key!=IntPtr.Zero) RegCloseKey(key); if(parent!=IntPtr.Zero) RegCloseKey(parent); }
    }
    public bool DeleteIfStillEmpty() {
        if(Key==null || Key.ValueCount!=0 || Key.SubKeyCount!=0) return false;
        Key.Dispose(); Key=null;
        Check(RegDeleteKeyTransactedW(Hklm,Path,View64,0,transaction,IntPtr.Zero));
        return true;
    }
    public void Commit() {
        if(!CommitTransaction(transaction)) throw new Win32Exception(Marshal.GetLastWin32Error());
        committed=true;
    }
    public void Dispose() {
        if(Key!=null) { Key.Dispose(); Key=null; }
        if(transaction!=IntPtr.Zero) {
            if(!committed) RollbackTransaction(transaction);
            CloseHandle(transaction); transaction=IntPtr.Zero;
        }
    }
}
'@
}
function Invoke-RestrictedCore([string]$Executable, [string]$Core, [string]$InputPath, [string]$CoreWork) {
    foreach ($path in @($Executable, $Core, $InputPath, $CoreWork)) {
        Require ([IO.Path]::IsPathFullyQualified($path) -and $path -cnotmatch '["\r\n]') 'Unsafe restricted child path'
    }
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    try { $expectedSid = $identity.User.Value } finally { $identity.Dispose() }
    $command = '"{0}" -NoLogo -NoProfile -NonInteractive -File "{1}" -InputFile "{2}" -Work "{3}" -RequireRestrictedClient -ExpectedUserSid "{4}"' -f $Executable, $Core, $InputPath, $CoreWork, $expectedSid
    try { return [BroTypistRestrictedInstallerClient]::Run($Executable, $command, $CoreWork, $expectedSid) }
    finally {
        $metadata.prepared_restricted_token = [BroTypistRestrictedInstallerClient]::PreparedToken
        $metadata.child_deadline_exceeded = [BroTypistRestrictedInstallerClient]::DeadlineExceeded
        $metadata.child_wait_error = [BroTypistRestrictedInstallerClient]::LastWaitError
        $metadata.child_exit_confirmed = [BroTypistRestrictedInstallerClient]::ClientExited
    }
}

try {
    Require ($IsWindows -and $env:CI -ceq 'true' -and $env:GITHUB_ACTIONS -ceq 'true' -and $env:RUNNER_ENVIRONMENT -ceq 'github-hosted' -and $env:RUNNER_OS -ceq 'Windows' -and $env:BROTYPIST_INSTALLER_SMOKE_DISPOSABLE -ceq '1') 'Disposable hosted Windows CI guard required'
    Require ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq [Runtime.InteropServices.Architecture]::X64) 'Native Windows x86_64 required'
    foreach ($path in @($InputFile, $Work)) {
        Require ([IO.Path]::IsPathFullyQualified($path)) 'Absolute private probe paths required'
        $relative = [IO.Path]::GetRelativePath([IO.Path]::GetFullPath($env:RUNNER_TEMP), [IO.Path]::GetFullPath($path))
        Require ($relative -ne '.' -and $relative -ne '..' -and -not $relative.StartsWith('..\') -and -not [IO.Path]::IsPathRooted($relative)) 'Probe paths must remain beneath RUNNER_TEMP'
        $ancestor = if (Test-Path -LiteralPath $path -PathType Container) { [IO.DirectoryInfo]::new($path) } else { [IO.FileInfo]::new($path) }
        while ($null -ne $ancestor) {
            if ($ancestor.Exists) { Require (($ancestor.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Linked probe ancestor refused' }
            $ancestor = if ($ancestor -is [IO.FileInfo]) { $ancestor.Directory } else { $ancestor.Parent }
        }
    }
    Require (-not (Test-Path -LiteralPath (Join-Path $Work 'client-observations.json'))) 'Existing client evidence refused'
    $input = Get-Content -LiteralPath $InputFile -Raw | ConvertFrom-Json
    foreach ($package in @($input.package, $input.previous)) {
        if ($null -eq $package) { continue }
        $msi = Get-Item -LiteralPath $package.artifacts.msi.path
        Require (-not $msi.PSIsContainer -and ($msi.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0 -and $msi.Length -eq $package.artifacts.msi.size) 'Nonregular or mismatched MSI refused'
        Require ((Get-FileHash -LiteralPath $msi.FullName -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $package.artifacts.msi.sha256) 'Canonical MSI hash differs before leaf-only SYSTEM Read'
        $acl = Get-Acl -LiteralPath $msi.FullName
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-5-18'), 'Read', 'Allow'))
        Set-Acl -LiteralPath $msi.FullName -AclObject $acl
        Require ((Get-FileHash -LiteralPath $msi.FullName -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $package.artifacts.msi.sha256) 'Canonical MSI hash differs after leaf-only SYSTEM Read'
    }
    $metadata.msi_system_read = 'non-secret canonical MSI leaves only; hashes rechecked; no inherited directory grants'
    Initialize-InstallerNative
    $nativeInitialized = $true
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    $apply = [BroTypistInstallerPolicyTransaction]::new()
    try {
        $metadata.before = Policy-Snapshot $apply.Key
        $policy = $metadata.before.values.DisableMSI
        Assert-DisableMsiPolicy $policy
        $valueWasAbsent = -not $policy.present
        if ($valueWasAbsent) {
            if ($null -eq $apply.Key) { $apply.CreateMissingKey() }
            # Reads and this write share the same transacted key handle.
            # Concurrent ordinary or transacted mutations conflict at commit.
            $apply.Key.SetValue('DisableMSI', [int]0, [Microsoft.Win32.RegistryValueKind]::DWord)
        }
        $metadata.transaction_intended_during = Policy-Snapshot $apply.Key
        $apply.Commit()
    } finally {
        # Recover committed ownership even if PowerShell is interrupted between
        # the native commit and the next script statement.
        if ($apply.Committed) {
            $createdValue = $valueWasAbsent; $createdKey = $apply.Created
            $metadata.policy_modified = $createdValue; $metadata.policy_key_created = $createdKey
        }
        $apply.Dispose()
    }
    $key = $base.OpenSubKey($policyPath, $false)
    $metadata.during = Policy-Snapshot $key
    Require ($metadata.during.values.DisableMSI.kind -ceq 'DWord' -and $metadata.during.values.DisableMSI.value -eq 0) 'Temporary DisableMSI fixture was not applied'
    $core = Join-Path $PSScriptRoot 'windows-installer-smoke.ps1'
    $metadata.core_sha256 = (Get-FileHash -LiteralPath $core -Algorithm SHA256).Hash.ToLowerInvariant()
    $nativeExit = Invoke-RestrictedCore (Get-Process -Id $PID).Path $core $InputFile $Work
    $metadata.native_exit_code = $nativeExit
    Require ($nativeExit -eq 0) 'Restricted native lifecycle failed; retain its independent observations'
    $metadata.passed = $true
} catch {
    $metadata.diagnostics.Add('Restricted client or bounded policy fixture failed; native observations and typed policy snapshots are authoritative')
    try { if ($null -ne $base) { $metadata.during = Policy-Snapshot $key } }
    catch { $metadata.diagnostics.Add('During-policy snapshot unavailable; never counted as passed') }
    $message = $_.Exception.Message
    foreach ($path in @($Work, $InputFile, $env:RUNNER_TEMP, $env:USERPROFILE)) {
        if (-not [string]::IsNullOrWhiteSpace($path)) { $message = $message.Replace($path, '<private-path>') }
    }
    $metadata.diagnostics.Add($message.Substring(0, [Math]::Min(2048, $message.Length)))
    $metadata.passed = $false
} finally {
    # Never race host-policy restoration with a live child, active/unknown MSI
    # service transaction, or owned UI/worker cleanup. A deadline does not free
    # the lease. Unprovable quiescence deliberately holds this runner until VM
    # teardown rather than returning/collecting an unfinished native outcome.
    $quiescent = -not $nativeInitialized -or -not [BroTypistRestrictedInstallerClient]::ClientLaunched
    if ($nativeInitialized -and [BroTypistRestrictedInstallerClient]::ClientLaunched -and [BroTypistRestrictedInstallerClient]::ClientExited) {
        try {
            $outcomePath = Join-Path $Work 'observations.json'
            Require ((Get-Item -LiteralPath $outcomePath).Length -le 262144) 'Native quiescence receipt missing/oversized'
            $outcome = Get-Content -LiteralPath $outcomePath -Raw | ConvertFrom-Json
            Assert-NativeQuiescence $outcome
            $quiescent = $true
        } catch { $quiescent = $false }
    }
    if (-not $quiescent) {
        [Console]::Error.WriteLine('Native quiescence is unknown: retaining child/VM ownership and policy lease until hosted VM teardown; no restoration, racing uninstall or final receipt collection')
        while ($true) { [Threading.Thread]::Sleep(30000) }
    }
    $metadata.quiescence_verified = $true
    try {
        if ($createdValue) {
            if ($null -ne $key) { $key.Dispose(); $key = $null }
            $restore = [BroTypistInstallerPolicyTransaction]::new()
            try {
                $current = Policy-Snapshot $restore.Key
                Require (-not $current.values.DisableMSI.present -or ($current.values.DisableMSI.kind -ceq 'DWord' -and $current.values.DisableMSI.value -eq 0)) 'Concurrent DisableMSI change preserved; restoration unresolved'
                if ($current.values.DisableMSI.present) { $restore.Key.DeleteValue('DisableMSI', $false) }
                # Empty-key read and nonrecursive deletion are part of this same
                # conflict-detecting transaction; concurrent values survive.
                if ($createdKey) { $metadata.empty_created_key_deleted = $restore.DeleteIfStillEmpty() }
                $restore.Commit()
            } finally { $restore.Dispose() }
        }
        if ($null -ne $key) { $key.Dispose(); $key = $null }
        if ($null -ne $base) {
            $key = $base.OpenSubKey($policyPath, $false)
            $metadata.restored = Policy-Snapshot $key
            foreach ($name in @('DisableMSI', 'DisableUserInstalls', 'AlwaysInstallElevated')) {
                foreach ($field in @('present', 'kind', 'value', 'value_sha256')) {
                    Require ($metadata.restored.values[$name][$field] -ceq $metadata.before.values[$name][$field]) 'Policy restoration or unchanged companion-policy verification failed'
                }
            }
            if (-not $createdKey) { Require ($metadata.restored.key_present -eq $metadata.before.key_present) 'Original policy key presence changed' }
            $metadata.restoration_verified = $true
        }
    } catch {
        $metadata.passed = $false
        $metadata.diagnostics.Add('Policy restoration could not be verified; inspect before/during/restored snapshots; fixture does not count as passed')
        try {
            if ($null -ne $base) {
                if ($null -ne $key) { $key.Dispose(); $key = $null }
                $key = $base.OpenSubKey($policyPath, $false)
                $metadata.restored = Policy-Snapshot $key
            }
        } catch { $metadata.diagnostics.Add('Restored policy snapshot itself was unavailable; restoration remains unverified') }
    } finally {
        if ($null -ne $key) { $key.Dispose() }
        if ($null -ne $base) { $base.Dispose() }
    }
    $metadata.passed = $metadata.passed -and $metadata.restoration_verified
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($metadata | ConvertTo-Json -Depth 16) + "`n")
    $stream = [IO.File]::Open((Join-Path $Work 'client-observations.json'), [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $stream.Write($bytes, 0, $bytes.Length); $stream.Flush($true) } finally { $stream.Dispose() }
}
if (-not $metadata.passed) { [Console]::Error.WriteLine('Restricted installer client failed; inspect client-observations.json and observations.json'); exit 1 }
