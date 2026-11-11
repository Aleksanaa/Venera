# SPDX-License-Identifier: MIT
[CmdletBinding()]
param([string]$OutputDirectory = (Join-Path $env:TEMP 'venera-adapter-runtime'))

$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path $OutputDirectory).Path
$settings = @{
    DXVK_LOG_LEVEL = 'info'
    DXVK_LOG_PATH = $OutputDirectory
    VKD3D_DEBUG = 'info'
    VKD3D_SHADER_DEBUG = 'info'
    VKD3D_LOG_FILE = (Join-Path $OutputDirectory 'vkd3d-runtime.log')
    VKD3D_LOG_BUFFERED = $null
}
$previous = @{}
$results = @()
$failed = $false
try {
    foreach ($name in $settings.Keys) {
        $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        [Environment]::SetEnvironmentVariable($name, $settings[$name], 'Process')
    }
    foreach ($engine in @('dxvk', 'vkd3d')) {
        $exe = Join-Path $PSScriptRoot ("adapter-runtime-{0}.exe" -f $engine)
        $started = [DateTime]::UtcNow.ToString('o')
        $watch = [Diagnostics.Stopwatch]::StartNew()
        $process = Start-Process -FilePath $exe -PassThru -Wait -NoNewWindow `
            -RedirectStandardOutput (Join-Path $OutputDirectory ("{0}-stdout.txt" -f $engine)) `
            -RedirectStandardError (Join-Path $OutputDirectory ("{0}-stderr.txt" -f $engine))
        $watch.Stop()
        $exitCode = $process.ExitCode
        if ($null -eq $exitCode) { throw "Process result unavailable: $engine" }
        $results += [pscustomobject]@{
            engine = $engine
            executable = $exe
            pid = $process.Id
            started_utc = $started
            exit_code = $exitCode
            elapsed_ms = $watch.Elapsed.TotalMilliseconds
            expected_logical_messages = 67
        }
        if ($exitCode -ne 0) { $failed = $true }
    }
    Copy-Item (Join-Path $PSScriptRoot 'literal-expectations.json') $OutputDirectory -Force
} finally {
    foreach ($name in $previous.Keys) {
        [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process')
    }
    ConvertTo-Json -InputObject @($results) -Depth 6 |
        Set-Content -Encoding UTF8 (Join-Path $OutputDirectory 'run-results.json')
}
$results | Format-Table -AutoSize
Write-Output ("Results: {0}" -f $OutputDirectory)
if ($failed) { exit 1 }
