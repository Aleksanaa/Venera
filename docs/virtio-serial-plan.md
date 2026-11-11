# VirtIO Serial 调试通道实施计划

状态：2026-11-11，核心交付完成，发行物为 `venera-debug-0.2.2.147.fd9eadb-r10.iso` / `.exe`，集成验证版本为 integration v13。四项 Windows 文件复制测试、完整生产安装、正常重启后的服务与会话自动采集、生产 Panel/宿主状态一致性及新增 67 条日志完整入库均有实证。重启比对见 `build/channel-stress-media/r10-reboot-host-comparison.json`。资源、WIM、14 项组件、CAT 与 SYS 内容比对检查通过，v13 来宾程序与 v12 逐字节一致，r9 压力对账证据继续有效。扩展验证与后续跟进见下方台账。

## 已取得的运行证据

- Windows 11 教育版 10.0.22621；官方 vioserial 100.103.104.30200，发布名 `oem61.inf`，签名检查 True。
- 手动 `install-service` 注册成功，`Start-Service VeneraDebugGuest` 启动成功。
- 首次 guest 与 host 状态均为 driver installed、controller running、port present、host connected；forwarding 为 true、revision 为 0，各筛选维度为全部。
- panel Debug channel 页已实际显示四项设备状态和默认 On。
- 宿主 SQLite 已保存真实 kernel ETW 记录。该次快照中 DBWIN、ETW、process_events 与会话 1 均为 collecting，loss 为 0。
- panel 无期限 DXVK / warning 筛选提交返回 revision 6，手动 Reset 返回 revision 7、All；关闭 panel 后服务保持 Running。
- 宿主接收端重启后有效筛选和到期时间保持一致。v6 核心矩阵脚本结果为 true，命令行检查包含关闭转发、重启服务后保持关闭、重新开启转发。
- 两个日志器在全部接收、后代进程筛选下分别完成 134/134 条逐条完整比对；错误进程实例接收为 0。此前独立的 134 条完整比对也已完成。
- 宿主历史按 `--from` / `--until` 接收时间范围导出已实现，9 项相关测试通过，真实导出获得 1206 个事件。
- LZX 安装载荷通过 Windows WIMGAPI 10.0.22621.4745 全部 API 检查。r4 生产安装器 `/vioserial /S` 实测 ProcessExit 为 0；安装日志记录 WIM 17 个文件、pnputil 259、控制器 problem 0 / ready 1，随后 Done、退出 0。

## 最新验证状态

| 项目 | 状态与证据 |
|---|---|
| 串口驱动安装 | r5、r6 安装器 `/vioserial /S` 均实测 ProcessExit 0；r6 日志记录 WIM 完成、pnputil 259、控制器 problem 0 / ready 1、Done。 |
| r8 串口驱动安装 | `D:\venera-setup.exe /vioserial /S` 经 Diagnostics.Process 实测 Exit 0，WIM 17 个文件、pnputil 259、problem 0 / ready 1、Done；服务 Running、PID 620，沿用测试目录服务路径。 |
| 路径筛选 | RepeatPath success true；五轮 PID 为 10216、14020、14460、1280、5684，各日志器运行成功，每轮损失为 0。宿主逐条比对每轮 67 条，共 335 条，正文、组件、级别和原始级别完全相同，missing、extra、missing_paths 均为 0；结束状态为 Running / On / All。 |
| Windows 本机通信 | r8 实测 4 项全部通过、Exit 0、1.04 秒，覆盖延迟读取、停止读取时的刷新取消，以及监听端释放后的首次读取与空闲订阅取消。 |
| 来源矩阵 | Windows 29 项均 success、Exit 0，环境恢复检查为 true；按 PID 与本地矩阵核对 229 条字面预期的输出/门控、组件、级别、原始级别，宿主收到 124 条，29 项比对均通过。 |
| 后台持续采集 | 关闭 panel PID 11240 后，日志器 PID 14300 的 67 条记录逐条完整入库。 |
| 会话 1 helper 恢复 | 终止 helper PID 12740 后，5 秒观察到新 helper PID 12400，同属服务 PID 6652；服务 Running、采集 collecting、损失 0，恢复后日志器 PID 14816 再次完整入库 67 条。 |
| 原生来源 | 恢复日志器 PID 4564 的 67 条正文全部匹配。真实 vkprobe 加载 Venus DLL，PID 14264 共 17 条，含精确多行 `vn_env`、venus/debug 与 renderer 连接日志；程序 Exit 0，报告 feature_gaps=2。原始 `vgpusrv.exe help` PID 10028 两条 `CService\n` / `~CService\n` 完整入库为 service/unknown，same_directory=true。 |
| 32/64 位来源 | PID 10780、1772、6936、15292 的实际位宽为 32、32、64、64，分别收到 4、5、4、5 条，级别正确。 |
| r6 压力观测 | PID 10852 调用 20001 次，用时 931.991 ms，单次最大 2.330 ms；宿主收到 2260 条唯一记录，服务已分配序号的 9076 条等于 2260 接收加 6816 gap。 |
| r7 压力结果 | PID 11200 调用 20001 次，用时 921.450 ms，单次最大 1.096 ms；2234 接收加 5994 序号缺口等于 8228，helper gap 11052 含 7622/3023 补报；入口前段 721 条待解释，transport_interrupted 数量为 null。 |
| 恢复后采集 | r7 PID 12252、r8 PID 3388 各 67 条记录完整匹配。 |
| r9 串口安装与诊断 | Windows `/vioserial /S` 实测 Exit 0，WIM、pnputil 259、problem 0 / ready 1、Done。按会话的 `health.session_helper_loss` 提供 pid、dropped_total、reported_total；17 项测试、check、clippy 通过。 |
| r9 压力对账 | PID 6672 调用 20001 次，用时 993.308 ms，单次最大 1.334 ms；SQLite 确认 2202 条唯一记录，序号 1..9342 等于 2202 接收加 7140 服务缺口。helper 缺口 10659，20001 = 2202 + 7140 + 10659，已知数量差额 0；另有一条 transport_interrupted、count null 事件。 |
| r9 排空与恢复 | Windows 独立快照显示会话 1、helper PID 11124 的 dropped_total = reported_total = 10659；恢复 PID 5148 的 67 条正文全部匹配。 |
| 快速转发开关 | Off/On 两次操作均 Exit 0，共 206.995 ms，结束状态 Running / On / All。 |
| 端口热移除与重接 | 专用端口移除后，服务保持 Running、PID 13696，直到整包安装更新；会话持续采集。重接后 QEMU 显示 host on、guest off，socket ACL 已恢复用户配置。Windows 的 `vport0p3` 为 IsPresent=False，服务重试报 os error 2。2026-11-10 15:44:48 UTC 单次 `pnputil /scan-devices` 退出 0，端口仍缺失；该轮自动恢复验收失败。 |
| 生产服务部署 | r9 整包 `/S /with-vioserial` 退出 1603；`oem64.inf` 已安装，下次启动生效。服务已注册为 Auto、Running、PID 13848，命令为 `C:\Windows\System32\DriverStore\FileRepository\venera.inf_amd64_ca61ec5dbb9f1be2\venera-debug-guest.exe service`。复制卸载程序遇到 AccessDenied；目标为 19,759,616 字节、ReadOnlyArchive，管理员有 full control，观察时 setup 进程已结束。已定位复制 ISO 文件时保留只读属性导致升级覆盖失败；来宾日志为 `C:\ProgramData\Venera\setup-full-r9-failed.log`。 |
| r10 安装修复验证 | Windows 实跑四项文件复制场景通过。整包 `/S /with-vioserial` 返回 3010，安装日志记录产品和卸载程序注册、Panel 安装及启动后清理任务创建成功。安装后的卸载程序为 34,283,008 字节，属性为 Archive；DriverStore 中的服务为 Auto、Running，PID 8520。来宾证据为 `C:\ProgramData\Venera\setup-copy-test-r10.txt` 和 `C:\ProgramData\Venera\setup-full-r10.log`。 |
| r10 正常重启 | 用户授权的 `shutdown /r /t 0` 已执行，LastBootUpTime 为 2026/11/11 08:53:49；服务 Auto、Running、PID 3900，程序位于 `C:\Windows\System32\DriverStore\FileRepository\venera.inf_amd64_5464bc85676d1177\venera-debug-guest.exe`，会话 1 helper PID 3152 自动 collecting。 |
| r10 生产查看与入库 | `%ProgramFiles%\Venera\venera-panel.exe` 的 Debug 页显示 On、installed/running/present/connected、All、revision 2；宿主独立 status 同为 revision 2、On、All，损失计数全为 0。恢复日志器 PID 14504、Exit 0，按 stream `2026-11-11T00:54:32.613Z-3900` 逐条比对 67 条字面预期，missing 0、extra 0。 |
| 后续诊断 | `windows/mod.rs` 的分片重组损失通过 queue_control 上报，返回 false 时未保留该报告，控制队列满时 native_capture_incomplete 可能遗漏；最后一个未完成分片在空闲时等待后续输入触发超时清理。两项作为后续跟进记录。 |
| 后续集成 | `viogpuap` 默认显示管理、Venera 异常、WGL/zink 与直接输出旁路、RDP、多会话、调试器附加、宿主停读继续实测。 |

r10 四项文件复制测试 Windows Exit 0，完整安装 Exit 3010 后已完成正常重启。热移除后缺失的端口在本次重启后恢复，热插拔自动重接的历史失败仍保留。重启后入库证据为 `build/channel-stress-media/r10-reboot-host-comparison.json`，来宾记录为 `stress-recovery-20261111-091658-9186376.json`。

路径复验的宿主比对记录为 `build/adapter-runtime-media/path-repeat-host-comparison.json`，来宾结果为 `C:\ProgramData\Venera\path-repeat-20261110-154036-333.json`。宿主已读取实时 SQLite 并按字面预期记录逐条核对。同一进程实例身份保留与缺路径等待补全修复已有 15 项相关测试通过，五轮实测确认路径记录完整。

来源比对证据为 `build/source-runtime-media/windows-host-comparison.json`，后台采集及 helper 恢复比对证据为 `build/adapter-runtime-media/background-helper-host-comparison.json`。

r7 压力及恢复证据为 `build/channel-stress-media/r7-host-comparison.json`，其中 transport_interrupted 保留 null 数量，已知对账记录为 19280 条，入口前段待解释为 721 条。r8 恢复比对为 `build/channel-stress-media/r8-recovery-host-comparison.json`。r9 新一轮独立累计对账证据为 `build/channel-stress-media/r9-host-comparison.json`，已知数量闭合，并保留一条未知数量的传输中断事件。Windows 排空快照独立核对了该轮 helper 的累计丢弃和已上报数量。

来源矩阵已覆盖实际 Venera `log.cpp` 的 `log_init/logf`、E/W/I/D、verbose 0/1，实际 Mesa util 的 `mesa_log/os_log_message` 各级别与 `MESA-VIRTIO` tag，DXVK 实际 Logger 五档与 none，以及 vkd3d 生产宏的 release/trace 构建、fixme、API/shader 独立配置。真实 `PrintMessage` 函数本地验证与原始 `vgpusrv.exe help` Windows 入库均已确认。原生来源记录位于 `build/native-source-media/windows-host-evidence.json`，32/64 位比对位于 `build/channel-stress-media/bitness-host-comparison.json`。详细来源及旁路台账见[通道设计](virtio-serial-debug.md)。

## 阶段验证清单

下表列出各阶段的完整验证目标，已取得的结果见上方证据与最新验证状态。后续继续覆盖会话生命周期、快速断连与热插拔恢复、压力与存储回收，以及来源覆盖和日志器耗时。

| 阶段 | 依赖 | 可观察交付与验证 |
|---|---|---|
| 1. 集中出口实验 | 当前源码与真实 Windows | DXVK/vkd3d logger 的 OutputDebugString 薄适配实验；中文、长消息、并发、调试器附加、采集暂停下的内容和耗时记录。形成主要日志器及旁路覆盖台账，审阅接入结论。 |
| 2. 共享协议与 Rust 构建 | 阶段 1 接入结论 | 公共类型、机器可读 schema、字面向量，guest EXE/host 原生产物可构建。验证 UTF-8、任意分片、坏行/超长行恢复、未知字段/type、major 错误、request 关联、控制优先；形成依赖与参数实验表。 |
| 3. 来宾持续聚合 | 阶段 2 | 一个 EXE 的 service/helper 覆盖全部登录会话，服务统一端口与筛选。验证 RDP/切换/注销、helper 重启、PID 重用、后续子进程、完整路径当前与未来匹配、到期恢复、本机开关及队列丢失。 |
| 4. 宿主持续保存 | 阶段 2，可与阶段 3 并行 | Rust 后台、SQLite 历史、可调容量/最旧淘汰、导出。验证查看工具退出后新记录持续入库、重连/半行/hello 缺失恢复、提交进度、空间回收及导出内容。首轮 Linux 验证结果用于确认平台。 |
| 5. panel 与安装 | 阶段 3；包版本核验可在阶段 2 后并行 | panel/host 返回同一有效筛选及 revision，验证并发修改、超时查询、到期通知和本机开关。固定官方完整驱动包与许可随安装器，panel 共用安装入口；验证四层状态、升级及重启。 |
| 6. 真实 Windows 集成 | 阶段 3–5 | 实际 QEMU/Windows/固定驱动下跨来源、多会话及 32/64 位应用矩阵；覆盖压力、停读、热移除、后台重启、快速重连与 pending 写取消。保存字节、记录、缺口与入库对照，实测确定队列、大小、等待与存储预算。 |

## 交付目标与验证证据

Venera 持续 VirtIO Serial 调试通道包含 DXVK/vkd3d 集中日志器的 Windows 调试输出适配、独立 Rust 来宾 service/session helper、Rust 宿主后台、共享 JSONL 契约、SQLite 历史、panel/CLI 控制及随安装器分发的官方 vioserial 驱动包。以 `docs/virtio-serial-debug.md` 为设计依据，通过下列验证补齐运行证据。

验收证据：

1. 集中 logger 实验覆盖中文、长消息、并发、调试器附加与阻塞；主要来源及旁路台账具有样本和接入结论。
2. 共享类型/schema/向量与两端行为一致；分帧恢复、版本兼容、请求关联、控制优先及缺口有固定输入与预期结果。
3. 全部登录会话持续采集，服务持有端口与筛选；两类目标、到期与本机开关在 Windows 可验证，panel/host 返回相同有效状态。
4. 宿主持续保存，退出查看工具后仍有新记录；可调容量淘汰最旧记录并回收空间，导出可读取。
5. 安装器与 panel 共用入口，发行物携带固定官方完整驱动包及许可，四层状态由实际探测呈现。
6. Windows/QEMU 集成记录可复核，写明交付层次、损失和恢复行为、实测参数及剩余证据缺口。

## 当前实现与参数验证

Linux/QEMU 为当前宿主实测环境。依赖版本已记录于 `debug-channel/Cargo.lock` 与构建配置；公共 schema 位于 `debug-channel/protocol/schema.json`。服务启动采用默认全部筛选，本机转发默认开启并持久保存开关；本机交接采用 Windows 命名管道，DXVK/vkd3d 长记录采用 native 分片重组。当前队列与单记录预算见[来宾说明](virtio-serial-guest.md)，宿主容量由部署参数设置。运行压力、空间回收和会话生命周期实验用于核验这些参数。集中 logger 实验若显示需要直接结构化接口，提交失败证据和扩展方案由用户决定。
