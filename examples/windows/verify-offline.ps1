[CmdletBinding()]
param(
    [string]$Inputs = 'C:\dmg-windows-inputs',
    [string]$Evidence = 'C:\dmg-windows-evidence'
)

$ErrorActionPreference = 'Stop'
$source = (Resolve-Path -LiteralPath $Inputs).Path
$record = (Resolve-Path -LiteralPath $Evidence).Path
if (Get-ChildItem -LiteralPath $record -Force | Select-Object -First 1) {
    throw 'Use an empty evidence directory for each check.'
}

$metadata = Join-Path $record 'guest.txt'
$system = Get-CimInstance -ClassName Win32_OperatingSystem
$revision = Get-ItemPropertyValue -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' -Name UBR
$processor = Get-CimInstance -ClassName Win32_Processor | Select-Object -First 1
@(
    "UTC: $([DateTimeOffset]::UtcNow.ToString('o'))"
    "Windows: $($system.Caption) $($system.Version).$revision"
    "Processor architecture: $($processor.Architecture)"
) | Set-Content -LiteralPath $metadata -Encoding utf8
if ($processor.Architecture -ne 9 -or -not [Environment]::Is64BitProcess) {
    throw 'Run this check with native x64 PowerShell in the Windows guest.'
}

$adapters = @(Get-NetAdapter | Select-Object Name, InterfaceDescription, Status)
if ($adapters.Count) {
    $adapters | Format-Table -AutoSize | Out-String | Add-Content -LiteralPath $metadata
} else {
    'No visible network adapters.' | Add-Content -LiteralPath $metadata
}
if ($adapters.Status -contains 'Up') {
    throw 'Disconnect the guest virtual NIC before running the check.'
}

foreach ($library in @('ucrtbase.dll', 'vcruntime140.dll')) {
    $path = Join-Path ([Environment]::SystemDirectory) $library
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Missing $library. Check the guest runtime prerequisites in the runbook."
    }
    "$library version: $((Get-Item -LiteralPath $path).VersionInfo.FileVersion)" |
        Add-Content -LiteralPath $metadata
}

function Expand-PinnedArchive($Name, $Digest, $Destination) {
    $path = Join-Path $source $Name
    $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -cne $Digest) {
        throw "$Name differs from its expected SHA-256."
    }
    "$Name SHA-256: $actual" | Add-Content -LiteralPath $metadata
    Expand-Archive -LiteralPath $path -DestinationPath $Destination
}

$work = New-Item -ItemType Directory -Path (Join-Path $env:TEMP "dmg-$([Guid]::NewGuid())")
$bundle = Join-Path $work.FullName 'bundle'
$pythonDirectory = Join-Path $work.FullName 'python'
$gitDirectory = Join-Path $work.FullName 'git'
Expand-PinnedArchive 'dmg-windows-offline-acceptance.zip' `
    'a7c5d22bbf14cfd3d839379a52846881ea6e1aba675376c4c92bdf82716bfeed' $bundle
Expand-PinnedArchive 'python-3.14.10-embed-amd64.zip' `
    '90905e64a840557d8c1bc4f5b74db3d7ee85e3ffb217611cac1c7813acea4159' $pythonDirectory
Expand-PinnedArchive 'MinGit-2.56.1-64-bit.zip' `
    '4a92850b6e917da4fd010669b614b55dccf5b0ec333a534cc4be4700dfefdafd' $gitDirectory

Get-Content -LiteralPath (Join-Path $bundle 'SHA256SUMS') | ForEach-Object {
    if ($_ -cnotmatch '^([0-9a-f]{64})  ([A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*)$') {
        throw 'Invalid bundle checksum line.'
    }
    $expected = $Matches[1]
    $relative = $Matches[2]
    if ($relative.Split('/') -contains '..') { throw 'Invalid bundle checksum path.' }
    $actual = (Get-FileHash -LiteralPath (Join-Path $bundle $relative) -Algorithm SHA256).Hash
    if ($actual.ToLowerInvariant() -cne $expected) { throw "Bundle checksum mismatch for $relative." }
}

# Embedded Python needs the verified harness directory on its isolated module path.
[IO.File]::WriteAllLines((Join-Path $pythonDirectory 'python314._pth'),
    [string[]]@('python314.zip', '.', (Join-Path $bundle 'scripts')),
    [Text.UTF8Encoding]::new($false))
$python = Join-Path $pythonDirectory 'python.exe'
$git = Join-Path $gitDirectory 'cmd\git.exe'
$pythonVersion = & $python --version
if ($LASTEXITCODE -ne 0) { throw 'The staged Python could not run.' }
$gitVersion = & $git --version
if ($LASTEXITCODE -ne 0) { throw 'The staged Git could not run.' }
$pythonVersion, $gitVersion | Add-Content -LiteralPath $metadata

$arguments = @(
    '-B'
    ('"{0}"' -f (Join-Path $bundle 'scripts\verify-windows-upgrade.py'))
    '--package'
    ('"{0}"' -f (Join-Path $bundle 'candidate'))
    '--manifest-sha256'
    'eb875e3be11e1504d683849ff19def662c6658c7a377c7b2937db82c95f6c619'
    '--previous'
    ('"{0}"' -f (Join-Path $bundle 'pilot\dmg-0.1.0-x86_64-pc-windows-msvc.exe'))
    '--previous-sha256'
    'e1c374c1a0c2b08c2fdd34f04f3ebe38dc811fbd5077000ff8f5853449be3c97'
    '--git'
    ('"{0}"' -f $git)
    '--scratch'
    ('"{0}"' -f $work.FullName)
)
$replay = Start-Process -FilePath $python -ArgumentList $arguments -NoNewWindow -Wait -PassThru `
    -RedirectStandardOutput (Join-Path $record 'guard.stdout.log') `
    -RedirectStandardError (Join-Path $record 'guard.stderr.log')
$exitCode = $replay.ExitCode
if ($null -eq $exitCode) { throw 'The replay did not provide an exit code.' }
$exitCode | Set-Content -LiteralPath (Join-Path $record 'guard.exit.txt') -Encoding ascii
if ($exitCode -ne 0) { throw "Windows verification exited $exitCode. See the evidence logs." }
Write-Output "Windows verification passed. Evidence: $record"
