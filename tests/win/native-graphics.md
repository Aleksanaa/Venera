# 图形日志 Windows 原生验收

在 Linux 仓库根目录，用与当前安装包匹配的 `build/win` 和 `build/icd/meson` 组装验收包：

```sh
nix develop --offline -c python3 tests/win/build-native-graphics.py \
  --out build/native-graphics-qa
```

`windows/` 中有两组发射程序：`source-runtime-*.exe` 来自 `build-json-probes.py` 的真实 EventWrite 构建路径，使用对应构建的日志代码；`native-graphics-package.exe` 按绝对路径加载从安装包资源提取的三个 DLL，直接调用它们的日志入口。`package-provenance.txt` 记载对应安装包和 DLL 的符号位置。测试覆盖 DLL 日志入口和传输，适用场景是日志迁移验收。

现场操作员将整个 `windows/` 目录复制到 Windows 本地磁盘，在管理员 PowerShell 中执行：

```powershell
Set-Location C:\QA\windows
$env:VENERA_GRAPHICS_KIT = (Get-Location).Path
.\native-graphics-history.exe --ignored --nocapture --exact windows::capture::tests::native_graphics_history::native_graphics_history
```

运行输出首行包含 `VeneraGraphicsQA-...` 会话名和 `%TEMP%\venera-native-graphics-...` 产物目录。目录中的 `graphics.etl` 是原始跟踪；同目录保存各发射程序的输出和私有历史库。工具通过私有管道调用生产 `history.read`，`history-result.json` 保存返回值；核对通过后写出 `history-page.json`，可直接归档。

核对点：

- 两组 DXVK 各有一条 88,000 字节正文；两组 vkd3d-proton 各有一条 `compile: ` 加同样 88,000 字节正文。按生产者 PID、事件号、级别、channel 和逐字节正文核对。
- Mesa 的 `os_log_message` 和 `mesa_log` 对应事件 9/10；后者使用 `MESA-VIRTIO` channel。UMD 警告对应事件 5。包内 DLL 组的进程输出同时提供实际加载路径和 PID。
- 同一包内 DLL 发射进程的普通 ODS 对照由调试器观察；`history.read` 中对应的是该 PID 的结构化 ETW 正例。ETW 文件会话的丢失计数和历史页 gap 同时检查。

将 Windows 的 `history-page.json` 带回 Linux，归档到新目录：

```sh
build/native-graphics-qa/native-graphics-archive \
  /path/from-windows/history-page.json build/native-graphics-archive-run
```

归档工具写入 `archive.sqlite`，验证重复导入、重开游标，以及 `archive.ndjson` 导出与 Windows 历史页逐条相等。
