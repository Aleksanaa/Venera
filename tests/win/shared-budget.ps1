param([string]$ScratchRoot = $env:TEMP, [switch]$Migration)

$ErrorActionPreference = 'Stop'
$exe = Join-Path $PSScriptRoot 'shared-budget-tests.exe'
$etl = Get-Item -LiteralPath (Join-Path $PSScriptRoot 'capacity.etl')
if ($etl.Length -ne 32MB) { throw 'capacity.etl must be 32 MiB.' }
$testName = 'capture_and_journal_replacement_peak'
$mode = 'shared-budget'
$minimumSpace = 1.2GB
if ($Migration) {
    $testName = 'early_history_migration_extent_peak'
    $mode = 'migration-extent'
    $minimumSpace = 160MB
}
$scratchParent = Get-Item -LiteralPath $ScratchRoot
if (!$scratchParent.PSIsContainer) { throw 'Select an NTFS scratch directory.' }
$drive = New-Object System.IO.DriveInfo($scratchParent.FullName.Substring(0, 3))
if ($drive.DriveFormat -ne 'NTFS' -or $drive.AvailableFreeSpace -lt $minimumSpace) {
    throw "Select an NTFS scratch directory with at least $minimumSpace bytes free."
}
$run = $mode + '-' + [guid]::NewGuid().ToString('N')
$evidence = (New-Item -ItemType Directory -Path (Join-Path $PSScriptRoot "$run-evidence")).FullName
$scratch = (New-Item -ItemType Directory -Path (Join-Path $scratchParent.FullName "$run-scratch")).FullName
$saved = @{}
$testExit = 1
try {
    $volume = Get-CimInstance Win32_Volume -Filter "DriveLetter='$($drive.Name.Substring(0,2))'"
    [ordered]@{
        Scratch = $scratch
        Attributes = (Get-Item -LiteralPath $scratch).Attributes.ToString()
        FileSystem = $drive.DriveFormat
        ClusterBytes = $volume.BlockSize
        FreeBytes = $drive.AvailableFreeSpace
    } | ConvertTo-Json | Set-Content (Join-Path $evidence 'volume.json')

    $envForTest = @{
        TEMP = $scratch; TMP = $scratch; VENERA_CAPACITY_ETL = $etl.FullName
        VENERA_SHARED_BUDGET_INVENTORY = (Join-Path $evidence 'files.tsv')
    }
    if ($Migration) { $envForTest['VENERA_MIGRATION_FIXTURE'] = Join-Path $PSScriptRoot 'DebugHistory' }
    foreach ($key in $envForTest.Keys) {
        $saved[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
        [Environment]::SetEnvironmentVariable($key, $envForTest[$key], 'Process')
    }
    $out = Join-Path $evidence 'windows.log'
    $err = Join-Path $evidence 'windows.err.log'
    $test = Start-Process -FilePath $exe -Wait -PassThru -NoNewWindow `
        -ArgumentList "journal::tests::shared_budget::$testName", '--exact', '--ignored', '--nocapture', '--test-threads=1' `
        -RedirectStandardOutput $out -RedirectStandardError $err
    $testExit = $test.ExitCode
    Get-Content -LiteralPath $out -Tail 15
    if ((Get-Item -LiteralPath $err).Length -gt 0) { Get-Content -LiteralPath $err -Tail 20 }
    # libtest exits successfully even if a stale executable matches no tests.
    if ($testExit -eq 0 -and !(Select-String -LiteralPath $out -Pattern 'test result: ok\. 1 passed;' -Quiet)) {
        throw "The executable did not report a passing $testName test."
    }
} finally {
    foreach ($key in $saved.Keys) { [Environment]::SetEnvironmentVariable($key, $saved[$key], 'Process') }
    Remove-Item -LiteralPath $scratch -Recurse -Force
    Write-Host "Evidence: $evidence"
}
exit $testExit
