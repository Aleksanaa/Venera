$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'desktop-owned-document.ps1')

function Expect-Rejection([scriptblock]$action, [string]$reason) {
    try { & $action | Out-Null }
    catch {
        if ($_.Exception.Message -cne $reason) { throw }
        return
    }
    throw "Expected rejection: $reason"
}

$leaf = 'Venera-QA-123.txt'
$owned = @{Id='7:20';Name='Venera-QA-123.txt';Selected=$true}
$restored = @{Id='7:19';Name='Meeting notes';Selected=$false}
if ((Assert-OwnedTab @($owned) $leaf '') -cne '7:20') { throw 'The single selected fixture should qualify on its own.' }
Expect-Rejection { Assert-OwnedTab @() $leaf '' } 'Expected one dedicated document tab; found 0.'
Expect-Rejection { Assert-OwnedTab @($owned,$restored) $leaf '' } 'Expected one dedicated document tab; found 2.'
Expect-Rejection { Assert-OwnedTab @(@{Id='7:19';Name='Meeting notes';Selected=$true}) $leaf '' } 'Selected document tab differs from the round fixture.'
Expect-Rejection { Assert-OwnedTab @(@{Id='7:20';Name='Copy of Venera-QA-123.txt';Selected=$true}) $leaf '' } 'Selected document tab differs from the round fixture.'
Expect-Rejection { Assert-OwnedTab @(@{Id='7:20';Name=$leaf;Selected=$false}) $leaf '' } 'Selected document tab differs from the round fixture.'
Expect-Rejection { Assert-OwnedTab @($owned) $leaf '7:99' } 'Selected document tab differs from the round fixture.'
Write-Output 'PASS: 7 tab identity cases'

$file = [IO.Path]::GetTempFileName()
try {
    [IO.File]::WriteAllBytes($file, [Text.Encoding]::ASCII.GetBytes('2026111802'))
    if ((Test-FixtureBytes $file '2026111802') -ne $true) { throw 'Expected exact text bytes to match.' }
    foreach ($bytes in @(
        ,([Text.Encoding]::ASCII.GetBytes("2026111802`n"))
        ,([byte[]](@(239,187,191) + [Text.Encoding]::ASCII.GetBytes('2026111802')))
        ,([Text.Encoding]::ASCII.GetBytes('2026111803'))
    )) {
        [IO.File]::WriteAllBytes($file, $bytes)
        if ((Test-FixtureBytes $file '2026111802') -ne $false) { throw 'Expected differing bytes to be rejected.' }
    }
} finally { [IO.File]::Delete($file) }
Write-Output 'PASS: 4 byte-readback cases'
