# 日志归属现场验收

每轮使用新的 Windows 验收目录和 Linux 数据库。示例端口 `49253` 见 `endpoint.json`，`CID` 为目标 Windows 虚拟机的 CID。

1. 在 Windows `cmd.exe` 中进入套件的 `windows` 目录：

   ```bat
   set "VENERA_FIELD_DIR=%CD%\qa-live"
   set "VENERA_FIELD_MODE=live"
   start "" venera-panel.exe
   ```

   在新开的 Panel 中进入 **日志**，点 **开始**。运行 `type "%VENERA_FIELD_DIR%\transport.jsonl"`，等到 `panel-ready` 和 `port: present`。

   在 Linux 套件目录启动接收器；记下输出的 `run`，在核对终端设置同一个值：

   ```sh
   run=$(mktemp -d); printf 'run=%s\n' "$run"
   ./venera-debug-host serve --cid "$CID" --port 49253 --database "$run/history.db" --control "$run/control.sock" --no-web
   ```

2. Windows 执行 `type nul > "%VENERA_FIELD_DIR%\publish-logs"`。Panel 显示 `A: before PID reuse`（`etw[42]`）和 `B: after PID reuse`（`B.exe[42]`）。保存 `before.png`；等 `pipe-2.jsonl` 出现和 `transport.jsonl` 记录 vsock ACK `2`。将两份 JSONL 复制到 Linux 的 `$run` 并执行：

   ```sh
   python3 check-field.py 2 "$run/history.db" "$run/pipe-2.jsonl" "$run/transport.jsonl"
   ```

3. 保持当前 Panel 日志页和接收器运行，Windows 执行 `type nul > "%VENERA_FIELD_DIR%\publish-revision"`。A 的原行变为 `A.exe[42]`；两行正文、时间、行序和 B 的进程栏应保持相同。保存 `after.png`。等 `pipe-3.jsonl` 出现和 vsock ACK `3`；将新记录及最新 `transport.jsonl` 复制到同一个 `$run`，执行：

   ```sh
   python3 check-field.py 3 "$run/history.db" "$run/pipe-3.jsonl" "$run/transport.jsonl"
   ```

   文本证据记录同一归档的 `1`、`2`、修订 `3 → 1`，并分列进程内命名管道与 Linux vsock 的 ACK。两张截图用于核对实际行更新。

4. Windows 执行 `type nul > "%VENERA_FIELD_DIR%\finish"`，看到 `phase: finished` 后关闭验收 Panel；Linux 接收器按 Ctrl+C。用 `qa-sealed` 和 `VENERA_FIELD_MODE=sealed` 重跑一轮，分别保存两轮证据。
