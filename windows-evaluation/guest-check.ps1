$ErrorActionPreference = 'Stop'
$serial = [IO.Ports.SerialPort]::new('COM1', 115200)
$serial.Open()
$serial.NewLine = [string][char]10
try {
    $os = Get-CimInstance -ClassName Win32_OperatingSystem
    $version = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
    $physical = @(Get-CimInstance -ClassName Win32_NetworkAdapter -Filter 'PhysicalAdapter = True')
    $routed = @(Get-NetRoute -ErrorAction Stop | Where-Object DestinationPrefix -in @('0.0.0.0/0','::/0'))
    if ($version.EditionID -notlike 'ServerStandardEval*' -or $os.BuildNumber -ne '26100' -or $os.OSArchitecture -ne '64-bit') {
        throw 'The installed guest is outside the pinned evaluation scope.'
    }
    if ($physical.Count -ne 0 -or $routed.Count -ne 0) {
        throw 'The guest has a physical network adapter or a default route.'
    }
    $sample = Join-Path $env:TEMP 'windows-evaluation-probe.txt'
    $utf8 = [Text.UTF8Encoding]::new($false)
    [IO.File]::WriteAllText($sample, 'synthetic disconnected Windows evaluation', $utf8)
    $result = [ordered]@{
        status = 'passed'
        phase = 'SetupComplete'
        caption = [string]$os.Caption
        edition = [string]$version.EditionID
        installation_type = [string]$version.InstallationType
        version = [string]$os.Version
        update_build_revision = [int]$version.UBR
        architecture = [string]$os.OSArchitecture
        physical_network_adapters = $physical.Count
        default_routes = $routed.Count
        sample_sha256 = (Get-FileHash -LiteralPath $sample -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    $serial.WriteLine('WINDOWS_EVALUATION_RESULT ' + ($result | ConvertTo-Json -Compress))
} catch {
    $result = @{status = 'failed'; phase = 'SetupComplete'; error = $_.Exception.Message}
    $serial.WriteLine('WINDOWS_EVALUATION_RESULT ' + ($result | ConvertTo-Json -Compress))
    throw
} finally {
    $serial.Close()
}
