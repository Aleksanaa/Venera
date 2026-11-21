[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][ValidateSet('short-lived','desktop-probe','desktop','rotation','process-cases','user-logs')][string]$Scenario,
    [string]$TestExe = (Join-Path $PSScriptRoot 'venera-debug-guest-tests.exe'),
    [string]$EvidenceRoot = $env:TEMP,
    [ValidateRange(1,100)][int]$PairNumber = 1,
    [string]$UmdX64,
    [string]$UmdX86
)
$ErrorActionPreference = 'Stop'
$admin = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (!$admin.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Open an elevated Windows PowerShell.' }
$isDesktop = $Scenario.StartsWith('desktop')
if ($isDesktop -and (![Environment]::UserInteractive -or [Threading.Thread]::CurrentThread.GetApartmentState() -ne 'STA')) {
    throw 'Open an interactive elevated Windows PowerShell with -STA.'
}
$exe = (Resolve-Path -LiteralPath $TestExe).Path
$base = Join-Path (Resolve-Path -LiteralPath $EvidenceRoot).Path ('Venera-process-field-' + [guid]::NewGuid().ToString('N'))
$dir = Join-Path $base 'fixture'
$null = New-Item -ItemType Directory -Path $base,$dir
Write-Host "Evidence: $base"
$prefix = 'windows::capture::tests::'
$keys = 'TEMP','TMP','VENERA_HISTORY_LOAD','VENERA_HISTORY_COUNT','VENERA_HISTORY_SECONDS','VENERA_HISTORY_FIXTURE','VENERA_HISTORY_BARRIER','VENERA_UMD_EMITTER','VENERA_UMD_X64','VENERA_UMD_X86'
$original = @{}
foreach ($key in $keys) { $original[$key] = [Environment]::GetEnvironmentVariable($key,'Process') }

function Json($value,$path) { ConvertTo-Json -InputObject $value -Depth 20 | Set-Content -LiteralPath $path -Encoding UTF8 }
function Sessions($path) {
    & "$env:SystemRoot\System32\logman.exe" query -ets | Out-File -LiteralPath $path
    if ($LASTEXITCODE) { throw 'ETW session enumeration failed.' }
    Get-Content -LiteralPath $path -Raw
}
function Run-Native($testName,$folder,[bool]$includeIgnored=$true) {
    $native = Join-Path $folder 'native'
    $null = New-Item -ItemType Directory -Path $folder,$native
    $arguments = "$prefix$testName --exact --nocapture --test-threads=1"
    if ($includeIgnored) { $arguments += ' --ignored' }
    $oldTemp = $env:TEMP; $oldTmp = $env:TMP
    try {
        $env:TEMP = $native; $env:TMP = $native
        $child = Start-Process -FilePath $exe -ArgumentList $arguments -WorkingDirectory (Split-Path -Parent $exe) -WindowStyle Hidden -PassThru `
            -RedirectStandardOutput (Join-Path $folder 'test.out') -RedirectStandardError (Join-Path $folder 'test.err')
        $null = $child.Handle
        $child
    } finally { $env:TEMP = $oldTemp; $env:TMP = $oldTmp }
}
function Confirm-Test($child,$folder,$started) {
    if (!$child.WaitForExit(150000)) { throw "Native test is still running (PID $($child.Id)); inspect $folder." }
    Json @{pid=$child.Id;exit=$child.ExitCode;startUtc=$started;endUtc=[datetime]::UtcNow.ToString('o')} (Join-Path $folder 'result.json')
    if ($child.ExitCode -ne 0 -or (Get-Content -LiteralPath (Join-Path $folder 'test.out') -Raw) -notmatch '1 passed; 0 failed;') {
        throw "Native test failed: $folder"
    }
}

try {
    Remove-Item Env:VENERA_HISTORY_FIXTURE,Env:VENERA_HISTORY_BARRIER,Env:VENERA_UMD_EMITTER,Env:VENERA_UMD_X64,Env:VENERA_UMD_X86 -ErrorAction SilentlyContinue
    if ($isDesktop) {
        Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes
        Add-Type -Path (Join-Path $PSScriptRoot 'window.cs')
        . (Join-Path $PSScriptRoot 'desktop-owned-document.ps1')
        Initialize-Desktop
        Json @{desktop_workload=$desktopWorkload;image=$script:desktopImage} (Join-Path $base 'desktop.json')
    }
    if ($Scenario -eq 'desktop-probe') {
        $rounds = @(RoundPlan)
        try { foreach ($i in 1..3) { Cycle $i } }
        finally { Json $rounds (Join-Path $base 'rounds.json'); Json $script:rows (Join-Path $base 'actions.json') }
    } elseif ($Scenario -in 'short-lived','desktop') {
        $env:VENERA_HISTORY_LOAD = $Scenario
        $env:VENERA_HISTORY_COUNT = '200'
        $env:VENERA_HISTORY_SECONDS = '60'
        $order = if ($PairNumber % 2) { 'baseline','capture' } else { 'capture','baseline' }
        $results = @{}
        foreach ($arm in $order) {
            $folder = Join-Path $base $arm
            Sessions (Join-Path $base "$arm-sessions-before.txt") | Out-Null
            $started = [datetime]::UtcNow.ToString('o')
            $child = Run-Native "process_history_overhead_$arm" $folder
            $sessionPattern = '(?m)^\s*Venera process cost ' + $child.Id + '\s'
            try {
                if ($isDesktop) {
                    $rounds = @(RoundPlan); $script:rows = @()
                    $out = Join-Path $folder 'test.out'
                    Until {
                        if ($child.HasExited) { throw 'Native test exited before its observation window.' }
                        if ((Get-Content -LiteralPath $out -Raw) -match 'Desktop observation window: 60 seconds') { $true }
                    } 'native observation window' 9000 | Out-Null
                    $script:clock = [Diagnostics.Stopwatch]::StartNew()
                    Event 'window_observed' $child.Id
                    $sessions = Sessions (Join-Path $folder 'sessions-window.txt')
                    if (($sessions -match $sessionPattern) -ne ($arm -eq 'capture')) { throw 'Supplemental ETW session differs from the selected arm.' }
                    foreach ($i in 1..3) {
                        $target = 500 + ($i-1)*20000
                        while ($script:clock.ElapsedMilliseconds -lt $target) { if ($child.WaitForExit(30)) { throw 'Observation window ended before the desktop action.' } }
                        if ($script:clock.ElapsedMilliseconds -gt $target+1500) { throw "Round $i missed its scheduled start." }
                        Cycle $i
                        if ($script:clock.ElapsedMilliseconds -ge 59500 -or $child.HasExited) { throw "Round $i crossed the observation window." }
                    }
                }
            } finally {
                if ($isDesktop) { Json $rounds (Join-Path $folder 'rounds.json'); Json $script:rows (Join-Path $folder 'actions.json') }
                Confirm-Test $child $folder $started
                if ((Sessions (Join-Path $folder 'sessions-after.txt')) -match $sessionPattern) { throw "Supplemental ETW session remains: PID $($child.Id)." }
            }
            $files = @(Get-ChildItem -LiteralPath (Join-Path $folder 'native') -Filter measurement.json -Recurse -File)
            if ($files.Count -ne 1) { throw "Expected one measurement in $folder." }
            $m = Get-Content -LiteralPath $files[0].FullName -Raw | ConvertFrom-Json
            if ($m.scenario -ne $Scenario -or $m.capture -ne ($arm -eq 'capture') -or $m.requested_processes -ne 200 -or $m.workload_ms -le 0) { throw "Measurement identity differs: $folder" }
            if ($isDesktop -and $m.workload_ms -lt 60000) { throw "Desktop window is short: $folder" }
            if ($arm -eq 'capture' -and (!$m.trace -or $m.trace.events_lost -ne 0 -or $m.trace.buffers_lost -ne 0 -or $null -eq $m.import_ms -or $m.journal_bytes -le 0 -or ($Scenario -eq 'short-lived' -and $m.lifecycle_events -lt 400) -or !(Test-Path -LiteralPath (Join-Path $files[0].DirectoryName 'original.etl')))) {
                throw "Capture or import evidence differs: $folder"
            }
            $results[$arm] = @{native=$m;actions_ms=if($isDesktop){($rounds | ForEach-Object { $_.action_ms } | Measure-Object -Sum).Sum}else{$null}}
        }
        $delta = [ordered]@{}
        foreach ($metric in 'workload_ms','workload_process_cpu_100ns','workload_system_busy_100ns','workload_process_written_bytes') {
            $delta[$metric] = [int64]$results.capture.native.$metric - [int64]$results.baseline.native.$metric
        }
        if ($isDesktop) { $delta['actions_ms'] = $results.capture.actions_ms - $results.baseline.actions_ms }
        $pair = @{scenario=$Scenario;pair=$PairNumber;order=$order;arms=$results;capture_minus_baseline=$delta}
        if ($isDesktop) { $pair.desktop_workload = $desktopWorkload }
        Json $pair (Join-Path $base 'pair.json')
    } else {
        $tests = switch ($Scenario) {
            'rotation' { 'process_history_survives_etl_rotation_and_reopen' }
            'process-cases' {
                'copied_source_gets_a_managed_handoff_and_distinct_file_identity'
                'deferred_inventory_accounts_for_the_exclusively_open_source'
                'offline_process_tree_import_preserves_instance_and_ancestors'
                'zero_loss_primary_stop_preserves_process_interruption_on_replay'
                'deferred_process_file_merges_after_later_file_import_and_reopen'
            }
            'user-logs' {
                if (!$UmdX64 -or !$UmdX86) { throw 'Supply the x64 and x86 log-capture.exe fixture paths.' }
                $env:VENERA_UMD_X64 = (Resolve-Path -LiteralPath $UmdX64).Path
                $env:VENERA_UMD_X86 = (Resolve-Path -LiteralPath $UmdX86).Path
                'offline_user_log_modules_preserve_process_ancestry_and_sequences'
            }
        }
        foreach ($name in $tests) {
            $folder = Join-Path $base $name
            $started = [datetime]::UtcNow.ToString('o')
            $ignored = $name -notin 'copied_source_gets_a_managed_handoff_and_distinct_file_identity','deferred_inventory_accounts_for_the_exclusively_open_source'
            $child = Run-Native $name $folder $ignored
            Confirm-Test $child $folder $started
        }
    }
    'ok' | Set-Content -LiteralPath (Join-Path $base 'complete.txt')
} catch {
    $_ | Out-String | Set-Content -LiteralPath (Join-Path $base 'error.txt') -Encoding UTF8
    throw
} finally {
    foreach ($key in $keys) { [Environment]::SetEnvironmentVariable($key,$original[$key],'Process') }
}
