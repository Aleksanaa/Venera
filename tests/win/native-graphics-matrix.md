# Windows 双架构图形日志验收

在 Linux 仓库根目录，用匹配的 `build/win` 和 `build/icd` 组装验收包：

```sh
nix develop --offline -c python3 tests/win/build-native-graphics-matrix.py \
  --out build/native-graphics-qa6
```

将 `build/native-graphics-qa6/windows/` 完整复制到 Windows 本地磁盘，在监听普通调试输出的交互会话中打开管理员 PowerShell：

```powershell
Set-Location C:\QA\graphics-qa6
$env:VENERA_GRAPHICS_MATRIX = (Get-Location).Path
.\native-graphics-matrix-history.exe --ignored --nocapture --exact windows::capture::tests::native_graphics_package_matrix::native_graphics_package_matrix
```

控制台显示临时 `VeneraGraphicsMatrix-...` 会话及产物目录。采集现有服务的同文结果最多等待约 90 秒；时间范围从启动同文进程前的 `history.range` 高水位开始。以 `live-pair.json` 为准：同一 PID、同一完整正文对应 `etw` / `venera_d3d11` / 事件 5 与 `dbwin` / 空组件 / 普通记录各一条。报告保留游标、匹配记录和读错误，方便核对部分结果。

`private-history-page.json` 是新版私有历史经 `history.read` 的结果，同文 PID 应有一条完整事件 5。两个架构各覆盖 UMD 事件 5/6、DXVK 和 vkd3d 的 88,000 字节主体，以及包内 Vulkan、WGL 各两个 Mesa 日志出口。`*.images.txt` 记录每个触发进程实际映射的 DLL 绝对路径、PID 和位数，`package-provenance.json` 提供对应的安装包映像、字节大小和日志入口偏移。

现场回传 `live-pair.json`、`private-history-page.json`、`graphics.etl`、`*.images.txt` 和出错时的 `*.stderr.txt`。日志入口验收调用包内 DLL 的日志路径；图形 API 渲染另由图形工作负载覆盖。
