# 记事本独立测试文档配对

`notepad-owned-document-v1` 每臂观察 60 秒，在约 0.5、20.5、40.5 秒各开始一轮。每轮打开一份新建的临时文档，输入 `2026111802`，执行保存并逐字节读回。历史桌面负载还包含资源管理器和往返焦点切换；两组结果按负载标识分别汇报。

由现场操作员在 `win10-field-restore` 内使用独立的交互式测试账户。账户需安装 Windows 记事本 `11.2609.11.0`，记事本处于完全退出状态，第一次打开测试文件即可进入独立单文档窗口。使用提升权限的 Windows PowerShell，桌面保持解锁并把键盘、鼠标操作留给本次测量。

将桌面脚本包解到新的目录。从该目录指定原现场包的 `venera-debug-guest-tests.exe` 和一个已存在的本地证据目录：

```powershell
$exe = 'C:\path\to\field-kit\venera-debug-guest-tests.exe'
$evidence = 'C:\path\to\desktop-evidence'
powershell.exe -NoProfile -STA -File .\process-field-desktop.ps1 -Scenario desktop-probe -TestExe $exe -EvidenceRoot $evidence
```

资格检查通过后再跑配对。奇数对先跑基线，偶数对先跑采集。

```powershell
powershell.exe -NoProfile -STA -File .\process-field-desktop.ps1 -Scenario desktop -PairNumber 1 -TestExe $exe -EvidenceRoot $evidence
powershell.exe -NoProfile -STA -File .\process-field-desktop.ps1 -Scenario desktop -PairNumber 2 -TestExe $exe -EvidenceRoot $evidence
```

每次运行将打印独立的证据目录。资格检查核对 `desktop.json`、`actions.json`、`rounds.json` 和 `complete.txt`；配对核对各臂的动作记录与最终 `pair.json`。遇到中止时，保留 `error.txt` 和当时的桌面状态，供现场操作员核对。
