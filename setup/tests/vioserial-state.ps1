param([string]$InstallerScript = "$PSScriptRoot/../../build/debug-integration-media/Install-DebugTest.ps1")
$ErrorActionPreference = 'Stop'
$tokens = $null; $errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Resolve-Path $InstallerScript).Path, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw ($errors -join "`n") }
foreach ($name in @('Get-DebugDriverReadiness', 'Test-DebugDriverResult')) {
    $function = $ast.Find({param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
    }, $true)
    . ([scriptblock]::Create($function.Extent.Text))
}
function Assert-Decision($Actual, [bool]$Expected, [string]$Name) {
    if ($Actual -isnot [bool] -or $Actual -ne $Expected) { throw "Decision mismatch: $Name" }
    Write-Output "PASS $Name"
}
function Controller($Service = 'VirtioSerial', $Problem = 0, $Status = 'OK', $Present = $true,
                    $Hardware = @('PCI\VEN_1AF4&DEV_1043&SUBSYS_11001AF4&REV_01')) {
    [pscustomobject]@{
        PNPDeviceID = 'PCI\VEN_1AF4&DEV_1043\1'
        HardwareID = $Hardware
        Present = $Present
        Service = $Service
        ConfigManagerErrorCode = $Problem
        Status = $Status
    }
}
$healthy = Get-DebugDriverReadiness @((Controller))
Assert-Decision (Test-DebugDriverResult 259 $healthy.ready) $true '259 healthy controller'
if ($healthy.devices.Count -ne 1 -or $healthy.devices[0].service -cne 'VirtioSerial' -or
    $healthy.devices[0].hardwareIds[0] -cne 'PCI\VEN_1AF4&DEV_1043&SUBSYS_11001AF4&REV_01') {
    throw 'Controller evidence mismatch'
}
Assert-Decision (Test-DebugDriverResult 259 (Get-DebugDriverReadiness @()).ready) $false '259 device discovery'
Assert-Decision (Test-DebugDriverResult 259 (Get-DebugDriverReadiness @((Controller -Problem 28))).ready) $false '259 controller problem'
Assert-Decision (Test-DebugDriverResult 259 (Get-DebugDriverReadiness @((Controller -Service 'other'))).ready) $false '259 service binding'
Assert-Decision (Test-DebugDriverResult 259 (Get-DebugDriverReadiness @((Controller -Status 'Unknown'))).ready) $false '259 device status'
Assert-Decision (Test-DebugDriverResult 259 (Get-DebugDriverReadiness @((Controller -Problem $null))).ready) $false '259 problem query'
Assert-Decision (Test-DebugDriverResult 259 (Get-DebugDriverReadiness @((Controller -Present $null))).ready) $false '259 presence query'
Assert-Decision (Test-DebugDriverResult 259 (Get-DebugDriverReadiness @((Controller -Hardware @()))).ready) $false '259 hardware query'
Assert-Decision (Test-DebugDriverResult 259 (Get-DebugDriverReadiness @((Controller -Present $false))).ready) $false '259 present devices'
Assert-Decision (Test-DebugDriverResult 259 (Get-DebugDriverReadiness @((Controller), (Controller -Service 'other'))).ready) $false '259 multiple controllers'
Assert-Decision (Test-DebugDriverResult 259 (Get-DebugDriverReadiness @((Controller -Hardware @('PCI\VEN_1AF4&DEV_10430')))).ready) $false '259 device ID token'
Assert-Decision (Test-DebugDriverResult 259 (Get-DebugDriverReadiness @((Controller -Hardware @('PCI\VEN_1AF4&DEV_1003')))).ready) $true '259 legacy compatible ID'
Assert-Decision (Test-DebugDriverResult 0 $false) $true 'successful installation'
Assert-Decision (Test-DebugDriverResult 3010 $false) $true 'restart requested'
Assert-Decision (Test-DebugDriverResult 1641 $false) $true 'restart initiated'
Assert-Decision (Test-DebugDriverResult $null $true) $false 'process completion'
Assert-Decision (Test-DebugDriverResult 5 $true) $false 'installation error'
