# Probe-only adapter for authenticated canonical artifacts; never rebuilds or rewrites a package receipt.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ArtifactRun,
    [Parameter(Mandatory)][string]$Work,
    [Parameter(Mandatory)][string]$Reports
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$stage = 'disposable_hosted_preflight'
$metadata = [ordered]@{
    schema = 1; tool = 'windows-installer-artifact-reuse'; passed = $false
    publication = 'held; no publication, signing or OTA'; rebuilt = $false
    full_installer_matrix_passed = $false; physical_typing_verified = $false
    requested_artifact_run = $ArtifactRun; probe_checkout_commit = $env:GITHUB_SHA
    scope = 'existing canonical MSI/ZIP under current native probe; package source is not probe source'
    installed_tiny_worker = 'not_exercised; canonical package artifact does not contain worker_smoke.exe'
    diagnostics = [System.Collections.Generic.List[string]]::new()
}
$originalPath = $env:PATH
$originalHome = $env:INKLOW_HOME
$originalDisposable = $env:BROTYPIST_INSTALLER_SMOKE_DISPOSABLE
$originalToken = $env:GH_TOKEN
$reportsOwned = $false

function Require([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function New-PrivateDirectory([string]$Path) {
    Require (-not (Test-Path -LiteralPath $Path)) 'Refusing an existing probe directory'
    [void][IO.Directory]::CreateDirectory($Path)
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $acl.SetOwner($sid); $acl.SetAccessRuleProtection($true, $false)
    $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
    Set-Acl -LiteralPath $Path -AclObject $acl
}
function Assert-TemporaryPath([string]$Path) {
    Require ([IO.Path]::IsPathFullyQualified($Path)) 'Probe paths must be absolute'
    $relative = [IO.Path]::GetRelativePath([IO.Path]::GetFullPath($env:RUNNER_TEMP), [IO.Path]::GetFullPath($Path))
    Require ($relative -ne '.' -and $relative -ne '..' -and -not $relative.StartsWith('..\') -and -not $relative.StartsWith('../') -and -not [IO.Path]::IsPathRooted($relative)) 'Probe paths must be strictly beneath RUNNER_TEMP'
    $parent = [IO.DirectoryInfo]::new([IO.Path]::GetFullPath($Path))
    while ($null -ne $parent) {
        if ($parent.Exists) { Require (($parent.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Linked probe ancestor refused' }
        $parent = $parent.Parent
    }
}
function Write-NewBytes([string]$Path, [byte[]]$Bytes) {
    $file = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $file.Write($Bytes, 0, $Bytes.Length); $file.Flush($true) } finally { $file.Dispose() }
}
function Write-NewJson([string]$Path, $Value) {
    Write-NewBytes $Path ([Text.UTF8Encoding]::new($false).GetBytes(($Value | ConvertTo-Json -Depth 32) + "`n"))
}
function Assert-Hash([string]$Path, [string]$Hash, [long]$Size) {
    Require ($Hash -cmatch '^[0-9a-f]{64}$' -and $Size -gt 0) 'Invalid canonical file digest/length'
    $file = Get-Item -LiteralPath $Path
    Require (-not $file.PSIsContainer -and ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Linked/nonregular package file refused'
    Require ($file.Length -eq $Size -and (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $Hash) 'Canonical file SHA256/length differs'
}
function Safe-Relative([string]$Path) {
    Require (-not [string]::IsNullOrWhiteSpace($Path) -and $Path.Length -le 1024 -and $Path -cnotmatch '[\\:\x00-\x1f]' -and -not $Path.StartsWith('/')) 'Unsafe portable member path'
    foreach ($part in $Path.Split('/')) { Require ($part -ne '' -and $part -ne '.' -and $part -ne '..' -and $part -notmatch '[. ]$') 'Unsafe portable path component' }
}
function Extract-Entry($Entry, [string]$Path, [long]$Limit) {
    Require ($Entry.Length -gt 0 -and $Entry.Length -le $Limit) 'Archive member exceeds declared bound'
    $mode = ($Entry.ExternalAttributes -shr 16) -band 0xF000
    Require ($mode -eq 0 -or $mode -eq 0x8000) 'Linked/special archive member refused'
    Require (($Entry.ExternalAttributes -band 0x410) -eq 0) 'Directory/reparse archive member refused'
    $source = $Entry.Open()
    $destination = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $buffer = [byte[]]::new(65536); [long]$total = 0
        while (($count = $source.Read($buffer, 0, $buffer.Length)) -gt 0) {
            $total += $count; Require ($total -le $Entry.Length) 'Archive member expands beyond declared bound'
            $destination.Write($buffer, 0, $count)
        }
        Require ($total -eq $Entry.Length) 'Archive member is truncated'
        $destination.Flush($true)
    } finally { $destination.Dispose(); $source.Dispose() }
}
function Get-Api([string]$Suffix) {
    return Invoke-RestMethod -Uri "https://api.github.com/repos/$env:GITHUB_REPOSITORY/$Suffix" -Headers $script:headers -TimeoutSec 60
}
function New-V5Guid([Guid]$Namespace, [string]$Name) {
    $bytes = $Namespace.ToByteArray()
    [Array]::Reverse($bytes, 0, 4); [Array]::Reverse($bytes, 4, 2); [Array]::Reverse($bytes, 6, 2)
    $digest = [Security.Cryptography.SHA1]::HashData([byte[]]($bytes + [Text.Encoding]::UTF8.GetBytes($Name.ToLowerInvariant())))
    $digest[6] = ($digest[6] -band 0x0F) -bor 0x50; $digest[8] = ($digest[8] -band 0x3F) -bor 0x80
    $hex = [Convert]::ToHexString($digest[0..15])
    return "$($hex.Substring(0,8))-$($hex.Substring(8,4))-$($hex.Substring(12,4))-$($hex.Substring(16,4))-$($hex.Substring(20,12))"
}
function Redact([string]$Text) {
    foreach ($path in @($Work, $Reports, $env:RUNNER_TEMP, $env:USERPROFILE, $originalToken)) {
        if (-not [string]::IsNullOrWhiteSpace($path)) { $Text = $Text.Replace($path, '<private>') }
    }
    $Text = [regex]::Replace($Text, '(?im)(token|password|secret|authorization|api[_-]?key)\s*[:=]\s*[^\r\n]+', '$1=<redacted>')
    if ($Text.Length -gt 4096) { $Text = $Text.Substring(0, 4096) + '[truncated]' }
    return $Text
}

try {
    Require ($IsWindows -and [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq [Runtime.InteropServices.Architecture]::X64) 'Native Windows x86_64 required'
    Require ($env:CI -ceq 'true' -and $env:GITHUB_ACTIONS -ceq 'true' -and $env:RUNNER_ENVIRONMENT -ceq 'github-hosted' -and $env:RUNNER_OS -ceq 'Windows' -and $env:GITHUB_EVENT_NAME -ceq 'workflow_dispatch') 'Disposable manual GitHub-hosted probe required'
    Assert-TemporaryPath $Work; Assert-TemporaryPath $Reports
    $Work = [IO.Path]::GetFullPath($Work); $Reports = [IO.Path]::GetFullPath($Reports)
    Require ($Work -ine $Reports -and -not $Work.StartsWith($Reports + '\', [StringComparison]::OrdinalIgnoreCase) -and -not $Reports.StartsWith($Work + '\', [StringComparison]::OrdinalIgnoreCase)) 'Evidence and private working paths must be separate'
    New-PrivateDirectory $Reports; $reportsOwned = $true
    New-PrivateDirectory $Work
    Require ($ArtifactRun -cmatch '^[1-9][0-9]{0,19}$') 'installer_artifact_run must be a positive decimal GitHub run ID'
    Require ($env:GITHUB_REPOSITORY -ceq 'renan-brasilio/brotypist' -and $env:GITHUB_API_URL -ceq 'https://api.github.com') 'Only the canonical same-repository GitHub artifact transport is allowed'
    Require (-not [string]::IsNullOrWhiteSpace($originalToken)) 'Authenticated Actions artifact access required'
    Require ($env:GITHUB_SHA -cmatch '^[0-9a-f]{40}$') 'Probe checkout commit is missing'
    $script:headers = @{ Authorization = "Bearer $originalToken"; Accept = 'application/vnd.github+json'; 'X-GitHub-Api-Version' = '2022-11-28' }

    $stage = 'authenticated_artifact_identity'
    $run = Get-Api "actions/runs/$ArtifactRun"
    Require ([string]$run.id -ceq $ArtifactRun -and $run.repository.full_name -ceq $env:GITHUB_REPOSITORY -and $run.head_repository.full_name -ceq $env:GITHUB_REPOSITORY) 'Foreign workflow run refused'
    Require ($run.path -ceq '.github/workflows/ci.yml' -and $run.status -ceq 'completed' -and $run.head_sha -cmatch '^[0-9a-f]{40}$') 'A completed canonical native workflow run is required (its installer gate may have failed)'
    $artifactName = 'installer-packages-windows-x86_64-not-published'
    $listing = Get-Api "actions/runs/$ArtifactRun/artifacts?name=$artifactName&per_page=100"
    Require ($listing.total_count -eq 1 -and @($listing.artifacts).Count -eq 1) 'Expected exactly one canonical Windows package artifact'
    $artifact = $listing.artifacts[0]
    Require ($artifact.name -ceq $artifactName -and -not $artifact.expired -and $artifact.digest -cmatch '^sha256:[0-9a-f]{64}$' -and $artifact.size_in_bytes -gt 0 -and $artifact.size_in_bytes -le 536870912) 'Expired, oversized or digest-less artifact refused'
    Require ([string]$artifact.workflow_run.id -ceq $ArtifactRun -and $artifact.workflow_run.repository_id -eq $run.repository.id -and $artifact.workflow_run.head_repository_id -eq $run.repository.id -and $artifact.workflow_run.head_sha -ceq $run.head_sha) 'Artifact/run repository or source binding differs'
    $metadata.artifact = @{ run_id = $ArtifactRun; id = $artifact.id; name = $artifact.name; digest = $artifact.digest; size = $artifact.size_in_bytes; run_head_commit = $run.head_sha; repository = $env:GITHUB_REPOSITORY }
    $archivePath = Join-Path $Work 'authenticated-actions-artifact.zip'
    Invoke-WebRequest -Uri "https://api.github.com/repos/$env:GITHUB_REPOSITORY/actions/artifacts/$($artifact.id)/zip" -Headers $script:headers -OutFile $archivePath -TimeoutSec 180 -MaximumRedirection 5
    Assert-Hash $archivePath $artifact.digest.Substring(7) $artifact.size_in_bytes
    $metadata.artifact.archive_sha256_verified = $true

    $stage = 'canonical_receipt_and_source_binding'
    $assets = Join-Path $Work 'packages'; New-PrivateDirectory $assets
    $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        Require ($archive.Entries.Count -eq 3) 'Canonical package artifact must contain only MSI, ZIP and original receipt'
        $receiptName = 'package-receipt-win32-x86_64.json'
        $receiptEntry = @($archive.Entries | Where-Object { $_.FullName -ceq $receiptName })
        Require ($receiptEntry.Count -eq 1) 'Canonical raw package receipt missing/duplicated'
        $rawReceipt = Join-Path $assets $receiptName
        Extract-Entry $receiptEntry[0] $rawReceipt 8388608
        # This retained copy is byte-for-byte original, not the rebased InputFile adapter.
        Copy-Item -LiteralPath $rawReceipt -Destination (Join-Path $Reports $receiptName)
        $metadata.raw_package_receipt_sha256 = (Get-FileHash -LiteralPath $rawReceipt -Algorithm SHA256).Hash.ToLowerInvariant()
        $package = Get-Content -LiteralPath $rawReceipt -Raw | ConvertFrom-Json
        Require ($package.application -ceq 'com.inklow.Inklow' -and $package.platform -ceq 'win32' -and $package.runtime -ceq 'rust' -and $package.architecture -ceq 'x86_64' -and $package.scope -ceq 'perUser' -and $package.signing -ceq 'unsigned' -and $package.signature -ceq 'unsigned' -and $package.release_eligible -eq $true) 'Expected canonical clean unsigned per-user Rust package'
        Require ($package.version -cmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' -and $package.source_commit -cmatch '^[0-9a-f]{40}$') 'Invalid package version/source identity'
        $provenance = $package.native_provenance
        Require ($provenance.schema -eq 1 -and $provenance.runtime -ceq 'rust' -and $provenance.source_kind -ceq 'pinned-source' -and $provenance.target -ceq 'x86_64-pc-windows-msvc' -and $provenance.llama_revision -ceq '0c1e57098bba43ac29e6e3b677cdceebdd22334f' -and $provenance.release_eligible -eq $true -and $provenance.source.dirty -eq $false -and $provenance.source.base_commit -ceq $package.source_commit -and $provenance.source.source_digest -cmatch '^[0-9a-f]{64}$') 'Canonical clean pinned native provenance differs'
        $namespace = [Guid](New-V5Guid ([Guid]'6ba7b811-9dad-11d1-80b4-00c04fd430c8') 'https://inklow.com/installer/windows/x86_64')
        Require ($package.product_code -ceq (New-V5Guid $namespace "product/$($package.version)") -and $package.upgrade_code -ceq (New-V5Guid $namespace 'upgrade')) 'Foreign MSI product/family identity refused'
        # PR artifacts compile a synthetic merge, not necessarily run.head_sha.
        $source = Get-Api "commits/$($package.source_commit)"
        Require ($source.sha -ceq $package.source_commit) 'Package source commit cannot be authenticated'
        $sourceBound = $package.source_commit -ceq $run.head_sha
        if (-not $sourceBound -and $run.event -ceq 'pull_request' -and @($source.parents).Count -eq 2) {
            $matches = @($run.pull_requests | Where-Object { $_.head.sha -ceq $run.head_sha -and $_.head.repo.id -eq $run.repository.id -and $_.base.repo.id -eq $run.repository.id -and $_.base.sha -ceq $source.parents[0].sha -and $_.head.sha -ceq $source.parents[1].sha })
            $sourceBound = $matches.Count -eq 1
        }
        Require $sourceBound 'Package source is neither the authenticated run head nor its exact same-repo PR merge'
        $metadata.package_source_commit = $package.source_commit
        $metadata.package_source_digest = $provenance.source.source_digest
        $metadata.package_version = $package.version
        $metadata.package_source_binding = if ($package.source_commit -ceq $run.head_sha) { 'exact run head' } else { 'authenticated same-repo PR base/head merge parents' }
        $metadata.package_artifacts = [ordered]@{}
        Require (@($package.artifacts.PSObject.Properties).Count -eq 2) 'Unexpected package artifact kinds'
        foreach ($kind in @('msi', 'zip')) {
            $record = $package.artifacts.$kind
            $name = "BroTypist-$($package.version)-windows-x86_64.$kind"
            Require ([IO.Path]::GetFileName([string]$record.path) -ceq $name) 'Recorded package basename differs from canonical version/platform'
            $entry = @($archive.Entries | Where-Object { $_.FullName -ceq $name })
            Require ($entry.Count -eq 1 -and $record.size -le 268435456) 'Missing/duplicate/oversized canonical package'
            $path = Join-Path $assets $name
            Extract-Entry $entry[0] $path $record.size
            Assert-Hash $path $record.sha256 $record.size
            $metadata.package_artifacts[$kind] = @{ original_path = $record.path; sha256 = $record.sha256; size = $record.size; rebased_basename = $name }
            # The only adapted values are the two artifact paths in this in-memory InputFile copy.
            $record.path = $path
        }
    } finally { $archive.Dispose() }

    $stage = 'portable_exact_payload_and_identity'
    $portableRoot = Join-Path $Work 'verified-portable'; New-PrivateDirectory $portableRoot
    $portable = Join-Path $portableRoot 'BroTypist'; New-PrivateDirectory $portable
    $expected = [Collections.Generic.Dictionary[string,object]]::new([StringComparer]::OrdinalIgnoreCase)
    Require (@($package.files).Count -gt 0 -and @($package.files).Count -le 4096) 'Invalid canonical payload inventory size'
    foreach ($row in $package.files) {
        Safe-Relative $row.path
        Require (-not $expected.ContainsKey("BroTypist/$($row.path)")) 'Duplicate/case-colliding payload path refused'
        Require ($row.size -gt 0 -and $row.size -le 67108864 -and $row.sha256 -cmatch '^[0-9a-f]{64}$') 'Invalid payload digest/size'
        $expected.Add("BroTypist/$($row.path)", $row)
    }
    $zip = [IO.Compression.ZipFile]::OpenRead($package.artifacts.zip.path)
    try {
        Require ($zip.Entries.Count -eq $expected.Count) 'Portable ZIP file count differs'
        foreach ($entry in $zip.Entries) {
            Safe-Relative $entry.FullName
            Require ($expected.ContainsKey($entry.FullName)) 'Unexpected/duplicate portable member'
            $row = $expected[$entry.FullName]
            Require ($entry.FullName -ceq "BroTypist/$($row.path)") 'Portable member case differs'
            $path = Join-Path $portable $row.path
            [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path))
            Extract-Entry $entry $path $row.size
            Assert-Hash $path $row.sha256 $row.size
            [void]$expected.Remove($entry.FullName)
        }
        Require ($expected.Count -eq 0) 'Portable ZIP omits canonical payload'
    } finally { $zip.Dispose() }
    $identity = Get-Content -LiteralPath (Join-Path $portable '.inklow-build.json') -Raw | ConvertFrom-Json
    foreach ($field in @('application', 'version', 'source_commit', 'architecture', 'platform')) { Require ($identity.$field -ceq $package.$field) 'Portable build marker differs from canonical receipt' }
    $embedded = Get-Content -LiteralPath (Join-Path $portable 'assets/native-provenance.json') -Raw | ConvertFrom-Json
    Require (($embedded | ConvertTo-Json -Depth 32 -Compress) -ceq ($provenance | ConvertTo-Json -Depth 32 -Compress)) 'Embedded portable native provenance differs from receipt'
    foreach ($name in @('BroTypist-cli.exe', 'BroTypist.exe')) {
        $file = [IO.File]::OpenRead((Join-Path $portable $name))
        $reader = [IO.BinaryReader]::new($file)
        try {
            Require ($reader.ReadUInt16() -eq 0x5A4D) 'Missing native PE header'
            $file.Position = 60; $offset = $reader.ReadUInt32(); Require ($offset -le $file.Length - 6) 'Invalid PE offset'
            $file.Position = $offset
            Require ($reader.ReadUInt32() -eq 0x00004550 -and $reader.ReadUInt16() -eq 0x8664) 'Expected native x86_64 PE'
        } finally { $reader.Dispose() }
    }
    $metadata.portable_exact_payload_verified = $true

    $stage = 'isolated_checksum_verified_tiny_acquisition'
    # Same immutable URL construction as models.rs, but no default known-folder cache mutation.
    $modelDirectory = Join-Path $Work 'models'; New-PrivateDirectory $modelDirectory
    $model = Join-Path $modelDirectory 'SmolLM2-135M-Q4_K_M.gguf'
    $modelHash = 'e3131339bf4e8065265593d4fd8f7bb7ff2d3abff1edb5618aa1197b89cad9f5'
    Invoke-WebRequest -Uri 'https://huggingface.co/ggml-org/SmolLM2-135M-GGUF/resolve/1076b85d135017971845b19b8d07df1479155988/SmolLM2-135M-Q4_K_M.gguf' -OutFile $model -TimeoutSec 300 -MaximumRedirection 5
    Assert-Hash $model $modelHash 101016128
    $metadata.tiny_model = @{ preset = 'tiny'; sha256 = $modelHash; size = 101016128; verified = $true; acquisition = 'canonical immutable catalogue URL into private work; default cache untouched'; installed_worker_exercised = $false }

    $stage = 'probe_only_input_adapter'
    $coreWork = Join-Path $Work 'native'; New-PrivateDirectory $coreWork
    $inputPath = Join-Path $coreWork 'probe-only-input.json'
    Write-NewJson $inputPath @{ package = $package; previous = $null; worker = $null; model = $model }
    $metadata.input_adapter = @{ probe_only = $true; schema = 'package, previous, worker, model (canonical xtask contract)'; changed_fields = @('package.artifacts.msi.path', 'package.artifacts.zip.path'); sha256 = (Get-FileHash -LiteralPath $inputPath -Algorithm SHA256).Hash.ToLowerInvariant(); original_receipt_unchanged = $true }
    Require ((Get-FileHash -LiteralPath $rawReceipt -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $metadata.raw_package_receipt_sha256) 'Original package receipt changed'
    # Match package.rs: only the non-secret MSI leaf gets SYSTEM read. Profile/cache ACLs stay owner-only.
    $acl = Get-Acl -LiteralPath $package.artifacts.msi.path
    $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-5-18'), 'Read', 'Allow'))
    Set-Acl -LiteralPath $package.artifacts.msi.path -AclObject $acl
    Assert-Hash $package.artifacts.msi.path $package.artifacts.msi.sha256 $package.artifacts.msi.size
    $metadata.msi_system_read = 'non-secret MSI leaf only; no inherited profile/cache grants'

    $stage = 'existing_guarded_native_core'
    $pwsh = (Get-Process -Id $PID).Path
    $core = Join-Path $PSScriptRoot 'windows-installer-smoke.ps1'
    $metadata.probe_core_sha256 = (Get-FileHash -LiteralPath $core -Algorithm SHA256).Hash.ToLowerInvariant()
    $metadata.probe_adapter_sha256 = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
    $env:BROTYPIST_INSTALLER_SMOKE_DISPOSABLE = '1'
    $env:GH_TOKEN = $null; $script:headers = $null
    $capture = [Text.StringBuilder]::new()
    & $pwsh -NoLogo -NoProfile -NonInteractive -File $core -InputFile $inputPath -Work $coreWork 2>&1 | ForEach-Object {
        if ($capture.Length -lt 32768) { [void]$capture.AppendLine((Redact ([string]$_))) }
    }
    $nativeExit = $LASTEXITCODE
    $metadata.native_exit_code = $nativeExit
    $metadata.native_output = $capture.ToString()
    $observationsPath = Join-Path $coreWork 'observations.json'
    Require ((Get-Item -LiteralPath $observationsPath).Length -le 262144) 'Native core receipt missing/oversized'
    Copy-Item -LiteralPath $observationsPath -Destination (Join-Path $Reports 'windows-core-installer-smoke.json')
    $observations = Get-Content -LiteralPath $observationsPath -Raw | ConvertFrom-Json
    Require ($nativeExit -eq 0 -and $observations.passed -eq $true) 'Native installer core assertions failed; inspect retained native receipt'
    $metadata.passed = $true
    $stage = 'completed_core_scope_only'
} catch {
    $metadata.diagnostics.Add((Redact $_.Exception.Message))
} finally {
    $env:PATH = $originalPath; $env:INKLOW_HOME = $originalHome
    $env:BROTYPIST_INSTALLER_SMOKE_DISPOSABLE = $originalDisposable
    $env:GH_TOKEN = $originalToken; $script:headers = $null
    $metadata.stage = $stage
    if ($reportsOwned) {
        # Only explicit bounded JSON evidence is uploaded, never payloads, models or arbitrary logs.
        Write-NewJson (Join-Path $Reports 'windows-installer-reuse.json') $metadata
    }
}
if (-not $metadata.passed) { throw 'Windows installer artifact reuse failed; see bounded retained JSON evidence' }
Write-Output 'Existing Windows installer probe passed within its explicitly limited scope; no rebuild/publication'
