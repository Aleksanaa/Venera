$desktopWorkload = 'notepad-owned-document-v1'
$script:clock = [Diagnostics.Stopwatch]::StartNew()
$script:rows = @()
$desktopText = '2026111802'

function Until([scriptblock]$probe, [string]$label, [int]$ms=4000) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        $value = & $probe
        if ($value) { return $value }
        Start-Sleep -Milliseconds 30
    } while ($timer.ElapsedMilliseconds -lt $ms)
    throw "Desktop observation timed out: $label"
}

function Event($action, $detail=$null) {
    $script:rows += [ordered]@{action=$action;ms=$script:clock.Elapsed.TotalMilliseconds;utc=[datetime]::UtcNow.ToString('o');detail=$detail}
}

function RoundPlan {
    foreach ($n in 1..3) { [ordered]@{round=$n;started=$false;completed=$false;action_ms=$null;file=$null} }
}

function Assert-NotepadSessionReady {
    $active = @(Get-Process -Name Notepad -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -eq $script:desktopSession })
    if ($active.Count) { throw 'Use a dedicated interactive Windows session with Notepad idle (no running Notepad process).' }
}

function Initialize-Desktop {
    $script:desktopSession = (Get-Process -Id $PID).SessionId
    $pkg = @(Get-AppxPackage -Name Microsoft.WindowsNotepad | Where-Object { $_.Version -eq '11.2609.11.0' })
    if ($pkg.Count -ne 1) { throw 'Expected one installed Windows Notepad 11.2609.11.0 package in the test account.' }
    $script:desktopImage = (Resolve-Path -LiteralPath (Join-Path $pkg[0].InstallLocation 'Notepad\Notepad.exe')).Path
    Assert-NotepadSessionReady
}

function Assert-OwnedTab($tabs, [string]$leaf, [string]$tabId) {
    if ($tabs.Count -ne 1) { throw "Expected one dedicated document tab; found $($tabs.Count)." }
    $tab = $tabs[0]
    if (!$tab.Selected -or !$tab.Id -or ($tabId -and $tab.Id -cne $tabId) -or
        $tab.Name -cnotin @($leaf,[IO.Path]::GetFileNameWithoutExtension($leaf))) {
        throw 'Selected document tab differs from the round fixture.'
    }
    return $tab.Id
}

function Test-FixtureBytes([string]$file, [string]$expected) {
    $bytes = [IO.File]::ReadAllBytes($file)
    return [BitConverter]::ToString($bytes) -ceq [BitConverter]::ToString([Text.Encoding]::UTF8.GetBytes($expected))
}

function DesktopControls($root, $type) {
    $condition = New-Object Windows.Automation.PropertyCondition ([Windows.Automation.AutomationElement]::ControlTypeProperty,$type)
    $root.FindAll([Windows.Automation.TreeScope]::Descendants,$condition)
}

function Assert-DesktopRoot($trial) {
    $trial.Process.Refresh()
    if ($trial.Process.HasExited -or $trial.Process.StartTime.ToUniversalTime() -ne $trial.StartUtc -or
        $trial.Process.Path -ine $script:desktopImage -or $trial.Process.SessionId -ne $script:desktopSession) {
        throw 'The dedicated Notepad process changed.'
    }
    $h = $trial.Hwnd
    $visible = @([DesktopQA]::Windows() | Where-Object { [DesktopQA]::Owner($_) -eq $trial.Process.Id -and [DesktopQA]::IsWindowVisible($_) })
    if ($visible.Count -ne 1 -or $visible[0] -ne $h -or [DesktopQA]::Class($h) -cne 'Notepad' -or
        ![DesktopQA]::IsWindowEnabled($h) -or [DesktopQA]::IsIconic($h) -or [DesktopQA]::GetWindow($h,4) -ne [IntPtr]::Zero -or
        [DesktopQA]::GetForegroundWindow() -ne $h -or
        $trial.Root.Current.ProcessId -ne $trial.Process.Id -or $trial.Root.Current.NativeWindowHandle -ne $h.ToInt64()) {
        throw 'The dedicated Notepad window is not the sole active window of its process.'
    }
    $window = $trial.Root.GetCurrentPattern([Windows.Automation.WindowPattern]::Pattern).Current
    $dialogClass = New-Object Windows.Automation.PropertyCondition ([Windows.Automation.AutomationElement]::ClassNameProperty,'ContentDialog')
    $dialogs = $trial.Root.FindAll([Windows.Automation.TreeScope]::Descendants,$dialogClass)
    if ($window.IsModal -or $window.WindowInteractionState -ne [Windows.Automation.WindowInteractionState]::ReadyForUserInteraction -or
        @(DesktopControls $trial.Root ([Windows.Automation.ControlType]::Window) | Where-Object { !$_.Current.IsOffscreen }).Count -or
        @($dialogs | Where-Object { !$_.Current.IsOffscreen }).Count) {
        throw 'The dedicated Notepad window is not ready for document interaction.'
    }
}

function Assert-OwnedEditor($trial, [string]$text, [switch]$Focused, [switch]$Opening) {
    Assert-DesktopRoot $trial
    $tabs = @(foreach ($tab in (DesktopControls $trial.Root ([Windows.Automation.ControlType]::TabItem))) {
        if ($tab.Current.ProcessId -ne $trial.Process.Id) { throw 'Document tab process differs.' }
        [pscustomobject]@{Id=($tab.GetRuntimeId() -join ':');Name=$tab.Current.Name;Selected=$tab.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Current.IsSelected}
    })
    if ($Opening -and !$tabs.Count) { return }
    $tabId = Assert-OwnedTab $tabs $trial.Leaf $trial.TabId
    $docs = @(DesktopControls $trial.Root ([Windows.Automation.ControlType]::Document) | Where-Object { !$_.Current.IsOffscreen })
    if ($Opening -and !$docs.Count) { return }
    if ($docs.Count -ne 1) { throw "Expected one visible document editor; found $($docs.Count)." }
    $doc = $docs[0]
    $edit = [IntPtr]$doc.Current.NativeWindowHandle
    if (!$doc.Current.IsEnabled -or $doc.Current.ProcessId -ne $trial.Process.Id -or $doc.Current.ClassName -cne 'RichEditD2DPT' -or
        [DesktopQA]::Class($edit) -cne 'RichEditD2DPT' -or [DesktopQA]::Owner($edit) -ne $trial.Process.Id -or
        [DesktopQA]::GetAncestor($edit,2) -ne $trial.Hwnd -or
        ($trial.Editor -and ($edit -ne $trial.EditHwnd -or ![Windows.Automation.Automation]::Compare($doc,$trial.Editor)))) {
        throw 'Editor identity differs from the round fixture.'
    }
    $observed = $doc.GetCurrentPattern([Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(-1) -creplace '\r\z',''
    if ($Opening -and $observed -ceq '') { return }
    if ($observed -cne $text) { throw 'Editor text differs from the expected round content.' }
    if ($Focused -and (!$doc.Current.HasKeyboardFocus -or [DesktopQA]::Focused($trial.Hwnd) -ne $edit)) {
        throw 'The document editor lost keyboard focus.'
    }
    Assert-DesktopRoot $trial
    $trial.TabId = $tabId; $trial.Editor = $doc; $trial.EditHwnd = $edit
    if ($Opening) { return $true }
}

function Save-OwnedDocument($trial, $seed) {
    # File / Save labels used by the English and Simplified Chinese Notepad builds.
    $fileLabel = ('^(File|{0}{1})(\s*\(&.\))?$' -f [char]0x6587,[char]0x4ef6)
    $saveLabel = ('^(Save|{0}{1})(\s*\(&.\))?$' -f [char]0x4fdd,[char]0x5b58)
    Assert-OwnedEditor $trial $desktopText -Focused
    $fileMenu = @(DesktopControls $trial.Root ([Windows.Automation.ControlType]::MenuItem) | Where-Object { $_.Current.Name -match $fileLabel -and !$_.Current.IsOffscreen -and $_.Current.IsEnabled })
    if ($fileMenu.Count -ne 1 -or $fileMenu[0].Current.ProcessId -ne $trial.Process.Id) { throw 'Expected one available File menu in the dedicated Notepad window.' }
    $expand = $null
    Assert-OwnedEditor $trial $desktopText -Focused
    if ($fileMenu[0].TryGetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern,[ref]$expand)) { $expand.Expand() }
    else { $fileMenu[0].GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke() }
    $save = Until {
        Assert-OwnedEditor $trial $desktopText
        $items = @(DesktopControls $trial.Root ([Windows.Automation.ControlType]::MenuItem) | Where-Object { $_.Current.Name -match $saveLabel -and !$_.Current.IsOffscreen -and $_.Current.IsEnabled })
        if ($items.Count -gt 1) { throw 'Save command is ambiguous.' }
        if ($items.Count) { $items[0] }
    } 'File > Save'
    if (!(Test-FixtureBytes $trial.File $seed)) { throw 'Fixture bytes changed before Save.' }
    Assert-OwnedEditor $trial $desktopText
    if ($save.Current.ProcessId -ne $trial.Process.Id -or !$save.Current.IsEnabled -or $save.Current.IsOffscreen) { throw 'Save command identity changed.' }
    $save.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
}

function Cycle($i) {
    $slot = $rounds[$i-1]; $slot.started = $true
    $timer = [Diagnostics.Stopwatch]::StartNew()
    try {
        Assert-NotepadSessionReady
        $id = [guid]::NewGuid().ToString('N')
        $leaf = "Venera-QA-$id.txt"; $file = Join-Path $dir $leaf
        $seed = "Venera desktop fixture $id"
        $stream = [IO.File]::Open($file,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
        try { $bytes = [Text.Encoding]::UTF8.GetBytes($seed); $stream.Write($bytes,0,$bytes.Length) } finally { $stream.Dispose() }
        $slot.file = $file
        $prior = [DesktopQA]::Windows()
        $process = Start-Process -FilePath $script:desktopImage -ArgumentList ('"' + $file + '"') -PassThru
        $null = $process.Handle
        $start = $process.StartTime.ToUniversalTime()
        Event 'document_launched' @{round=$i;pid=$process.Id;startUtc=$start.ToString('o');file=$file}
        $h = Until {
            if ($process.HasExited) { throw 'The Notepad launch exited before presenting a dedicated document.' }
            $windows = @([DesktopQA]::Windows() | Where-Object { [DesktopQA]::Owner($_) -eq $process.Id -and [DesktopQA]::IsWindowVisible($_) })
            if ($windows.Count -gt 1 -or ($windows.Count -eq 1 -and ($prior -contains $windows[0] -or [DesktopQA]::Class($windows[0]) -cne 'Notepad'))) {
                throw 'The Notepad launch presented an unexpected window.'
            }
            if ($windows.Count -eq 1 -and [DesktopQA]::GetForegroundWindow() -eq $windows[0]) { $windows[0] }
        } 'dedicated foreground Notepad window' 8000
        $trial = @{Process=$process;StartUtc=$start;Hwnd=$h;Root=[Windows.Automation.AutomationElement]::FromHandle($h);Leaf=$leaf;File=$file;TabId=$null;Editor=$null;EditHwnd=[IntPtr]::Zero}
        Until { Assert-OwnedEditor $trial $seed -Opening } 'owned document content' 2500 | Out-Null
        if (!(Test-FixtureBytes $file $seed)) { throw 'Fixture bytes differ at qualification.' }
        Event 'document_qualified' @{round=$i;pid=$process.Id;window=$h.ToInt64();editor=$trial.EditHwnd.ToInt64();tab=$trial.TabId;file=$file}
        Assert-OwnedEditor $trial $seed
        $trial.Editor.SetFocus()
        Assert-OwnedEditor $trial $seed -Focused
        [DesktopQA]::Edit($process.Id,$h,$trial.EditHwnd,$desktopText)
        Assert-OwnedEditor $trial $desktopText -Focused
        Event 'text_observed' $desktopText
        Save-OwnedDocument $trial $seed
        Until {
            Assert-OwnedEditor $trial $desktopText
            Test-FixtureBytes $file $desktopText
        } 'byte-exact saved document' | Out-Null
        Event 'fixture_saved' @{round=$i;file=$file;text=$desktopText;bytes=10}
        Assert-OwnedEditor $trial $desktopText
        $trial.Root.GetCurrentPattern([Windows.Automation.WindowPattern]::Pattern).Close()
        if (!$process.WaitForExit(4000)) { throw 'The dedicated Notepad process did not exit after document close.' }
        if (!(Test-FixtureBytes $file $desktopText)) { throw 'Saved bytes changed after close.' }
        Event 'cycle_success' $i
        $slot.completed = $true
    } catch {
        Event 'cycle_stopped' @{round=$i;reason=$_.Exception.Message}
        throw
    } finally { $slot.action_ms = $timer.Elapsed.TotalMilliseconds }
}
