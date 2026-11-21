# Windows 双架构图形日志验收

从同一构建的安装程序和图形产物组装验收包。以下示例在 Linux 仓库根目录运行：

```sh
qa=$(mktemp -d)
nix develop --offline -c env CARGO_TARGET_DIR="$qa/target" \
  python3 tests/win/build-native-graphics-matrix.py \
  --build-dir build/win --icd-dir build/icd --out "$qa/matrix"
```

在 Windows 上安装该版本，将 `$qa/matrix/windows/` 完整复制到本地磁盘。在交互桌面打开管理员 PowerShell：

```powershell
Get-Service VeneraDebugGuest
Set-Location C:\QA\graphics-matrix
$env:VENERA_GRAPHICS_MATRIX = (Get-Location).Path
.\native-graphics-matrix-history.exe --ignored --nocapture --exact windows::capture::tests::native_graphics_package_matrix::native_graphics_package_matrix
```

控制台显示 `VeneraGraphicsMatrix-...` 会话和 `%TEMP%` 下的产物目录。`live-pair.json` 保存从安装服务读取的窗口：起点是启动首个程序前的历史高水位，终点是读到异文栅栏后约 3 秒取得的新高水位。读取预算约 90 秒。原文和栅栏同属一个 PID 的 `venera_d3d11` 事件 5；`ods_returned_filetime` 位于两个事件的源时间之间，可结合游标复核处理顺序。`*-pair.images.txt` 记录发射步骤和当前 Windows 会话内 DBWIN 对象的打开结果，供现场诊断。

`installed-graphics-records.json` 保存安装服务返回的两个架构的记录。每个架构覆盖 UMD 事件 5/6、DXVK 和 vkd3d 的 88,000 字节主体，以及包内 Vulkan、WGL 各两个 Mesa 日志出口。`graphics.etl` 和 `private-history-page.json` 用于复核独立 ETW 文件回放。`*.images.txt` 记录触发进程实际映射的 DLL 路径、PID 和位数；`package-provenance.json` 提供安装包映像、大小和日志入口偏移。

在产物目录中复核窗口及长正文：

```powershell
$pair = Get-Content -Raw live-pair.json | ConvertFrom-Json
$pair | Select-Object status, pid, before, scanned_through, ods_returned_filetime, etw_matches, fence_matches, dbwin_matches
$pair.candidates + $pair.fences | ForEach-Object {
    [pscustomobject]@{ Position = $_.cursor.position; SourceTime = $_.record.structured.source_time_filetime; Text = $_.record.text }
}
(Get-Content -Raw installed-graphics-records.json | ConvertFrom-Json) | ForEach-Object {
    if ($_.record.structured.event_id -in 7, 8) {
        [pscustomobject]@{ Pid = $_.record.process.pid; Event = $_.record.structured.event_id; Bytes = [Text.Encoding]::UTF8.GetByteCount($_.record.text) }
    }
}
```

活动归档使用 [Venera 主机采集器](../../docs/virtio-socket-host.md)。在运行 Windows 验收前连接安装服务，在新目录保存数据库，待主机历史游标达到 `scanned_through` 后导出：

```sh
archive=$(mktemp -d)
printf '%s\n' "$archive"
venera-debug-host serve --cid "$cid" --database "$archive/history.db" \
  --control "$archive/control.sock" --no-web
# 在另一个终端，用上一步显示的归档目录：
venera-debug-host export --database "$archive/history.db" > "$archive/history.jsonl"
```

在 Windows 验收产物目录中，按游标核对同文对照、栅栏和两个架构的完整 ETW 正文：

```sh
jq -e -n --slurpfile live live-pair.json --slurpfile matrix installed-graphics-records.json \
  --slurpfile archive "$archive/history.jsonl" '
  $live[0].status == "observed" and ($matrix[0] | length > 0) and (
  ($live[0] | .candidates + .fences) + ($matrix[0] | map(select(.record.source == "etw")))
  | all(.[]; . as $row | any($archive[];
      .cursor == $row.cursor and .message.source == $row.record.source
      and .message.component == $row.record.component and .message.text == $row.record.text)))'
```

现场回传上述 JSON、ETL 和 `*.images.txt`；遇到发射错误时附上 `*.stderr.txt`。
