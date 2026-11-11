# VirtIO Serial 调试通道设计

状态：2026-11-11，核心交付完成，当前发行物为 r10，集成验证版本为 integration v13。资源、WIM、14 项组件、CAT 与 SYS 内容比对检查已通过，v13 来宾程序与 v12 逐字节一致。r10 四项文件复制测试、完整安装与正常重启后的自动采集和 67 条完整入库均已实测；r9 压力已知数量对账及 r8 四项 Windows 本机通信证据继续有效。扩展验证和后续跟进见实施台账。

## 1. 持续采集与查看

```text
Venera / Mesa / DXVK / vkd3d → 各会话采集端 ┐
系统服务 DBWIN / KMD ETW → 机器聚合端       ├→ virtserialport → 宿主后台 → SQLite
                         ↑ panel 控制      ┘                     ↑ 界面 / CLI
```

来宾采用独立 Rust EXE，提供 Windows service 和 session helper 两种模式。服务启动时枚举全部已登录会话，再处理登录、注销、RDP、用户切换；仍保留的断开会话继续采集。服务按会话启动和回收 helper，分别报告会话状态与采集健康状态，失败可重启 helper。服务负责 Global DBWIN 与系统 ETW，helper 负责所属会话 DBWIN；session 0 的本地与 Global 空间重合，在该空间建立一次采集。

服务统一汇聚、筛选、排队并持有专用端口。现有分辨率服务继续负责显示管理。panel 日志页和来宾 CLI 消费后台记录，服务与会话采集端管理 DBWIN。helper 的本机交接采用 Windows 命名管道，实现入口为 `debug-channel/guest/src/windows/ipc.rs`。

宿主采用 Rust 后台持续接收并保存，界面与 CLI 查询同一后台。查看工具退出时，采集、保存和筛选到期计时继续运行；记录来自后台持续采集。

## 2. 来源证据与接入

目标为各组件主要日志器的全部来源与级别，实际产量由用户配置、构建选项和生产者门控决定。组件侧从集中出口的薄 native 适配起步，Rust 汇聚后统一结构化。

| 来源 | 当前源码事实 | 接入与核验 |
|---|---|---|
| Venera D3D11/12 wrapper | `umd/common/log.cpp::logf` 使用 OutputDebugStringA 并向可用文件追加，正文缓冲 1024 字节 | 实际 `log.cpp` 的 `log_init/logf`、E/W/I/D 与 verbose 0/1 已完成 Windows 来源实验及宿主逐条比对；异常样本、截断边界继续核验。 |
| Mesa / Venus / WGL / zink | `util/log.c` 的 windbg logger 和 `util/os_misc.c::os_log_message` 使用 OutputDebugStringA；`vn_common.c::vn_log` 为 `MESA-VIRTIO` tag | 实际 util 各级别及 tag 已完成比对。真实 vkprobe 加载 Venus DLL 后，PID 14264 共收到 17 条记录，含完整多行 `vn_env`、venus/debug 分类及 connected to renderer；探测程序 Exit 0，报告 feature_gaps=2。WGL/zink 来源与旁路继续核验。 |
| DXVK | `Logger::emitMsg` 集中处理五档日志；`patches/debug-output/dxvk.patch` 已接入 Windows 调试输出，CMake 应用补丁 | 实际 Logger 五档及 none 门控已通过来源矩阵；与 vkd3d 的全部接收及后代筛选样本各完成 134/134 条完整比对。直接输出、异常分支和锁内调用耗时继续实测。 |
| vkd3d-proton | `vkd3d_dbg_printf` 集中处理 API/shader channel；`patches/debug-output/vkd3d-proton.patch` 已在普通/buffered 输出分支前接入，CMake 应用补丁 | release/trace 构建的生产宏门控、fixme、API/shader 独立配置已通过来源矩阵；双日志器完整比对已通过。初始化告警、buffered flush、其他旁路和耗时继续实测。 |
| KMD | `debug-channel/guest/src/windows/etw.rs` 接收系统实时 ETW DbgPrint，`collector.rs` 识别 VioGpu 文本来源 | 宿主 SQLite 已保存真实 kernel ETW 记录；更完整的事件字段、归属与门控矩阵待验证。 |
| 分辨率 service/helper | `kmd/yttrium/viogpu/shared/log.cpp::PrintMessage` 使用 OutputDebugString 与相对路径 `vgpusvp.log` | 真实函数本地验证通过；原始 `vgpusrv.exe help` PID 10028 的 `CService\n`、`~CService\n` 已完整入库为 service/unknown，same_directory=true。`viogpuap` 默认显示管理路径待触发。 |

### 集中出口小实验

DXVK/vkd3d 集中 logger 适配已实现，两个日志器已有 134/134 条完整比对结果。以下为完整实验矩阵，后续继续补充不同运行条件下的内容、调用耗时和应用响应：

- 中文、emoji、多行、长消息和截断边界；核对 A/W 入口、代码页及转换。
- 多线程、多进程并发，使用已知文本编号核对交错、缺失和归属。
- 调试器附加前后、DebugView 占用、采集端暂停/退出及 helper 重启。
- 普通与高频输出，比较 logger 临界区耗时、帧耗时及阻塞影响。

当前薄适配采用 OutputDebugStringA 发送 `VDBG1` 分片，来宾按原始字节重组并严格解码 UTF-8。实验继续核验文本保真、元数据与阻塞表现；需要扩展时整理样本并提交本机结构化接口设计供用户确认。

### 逐项覆盖台账

主要日志器与旁路逐项记录触发样本、实际出口、采到结果和后续接法：

| 项目 | 核对清单与证据缺口 |
|---|---|
| Venera | 实际 `log_init/logf` 的 E/W/I/D、verbose 0/1 已确认；实际异常诊断样本待补。 |
| Mesa | 实际 util 各级别和 tag、真实 Venus DLL 的多行 `vn_env` 与 renderer 连接日志已确认；vkprobe 报告 feature_gaps=2。WGL/zink 来源继续实测。 |
| zink 旁路 | `zink_screen.c` 能力警告、`nir_to_spirv.c` 诊断、`zink_bo.c` 泄漏提示直接使用 `fprintf(stderr, ...)`；`zink_compiler.c` 含 shader 文本输出。逐类核验用途与采集接点。 |
| DXVK | 实际 Logger 五档及 none 门控已确认；双日志器完整样本比对已通过。`src` 直接输出、异常分支和耗时继续逐调用点登记。 |
| vkd3d | 生产宏的 release/trace 构建门控、fixme、API/shader 独立配置已确认；初始化告警、文件打开失败的 `fprintf`、buffered flush、profiler/转储文件继续按用途登记。 |
| service/helper | 原始 `vgpusrv.exe help` 的两条真实日志完整入库；会话 1 helper 自动恢复及恢复后日志入库已确认。`viogpuap` 默认显示管理、`ErrorHandler` 与文件旁路继续核验。 |
| KMD | 真实 ETW 记录已入库，该次状态快照损失为 0；DbgPrint 门控、完整字段及压力下损失统计待补。 |

已有文件用于比较编码、完整性和历史内容；文件尾随的共享打开、同名进程、截断和缓冲延迟属于研究验证。当前通道按来源与进程识别实时记录。

## 3. 筛选与权威状态

默认接收目标组件的全部已产生级别、全部目标进程。通道提供查询状态、设置及恢复接收筛选；生产者详细程度通过组件配置设置，按其初始化规则生效。

字段 `components/levels/targets` 各维度取交集，同维度内取并集，空数组表示该维度全部；组件维度的“全部”指来源台账中的目标组件集合。通道健康事件独立保存。目标支持：

- `process_instance`：PID 与 Windows 进程创建 FILETIME（十进制字符串）共同匹配。`include_descendants` 可包含规则生效后创建的子孙进程；根退出后已纳入实例保持匹配，直至规则重置或到期。
- `exe_path`：完整可执行文件路径，匹配当前和未来实例；基于 Windows 查询结果规范化并按 Windows 大小写语义比较，保留原始路径用于显示。别名与权限行为待实测。

服务维护实例、父子关系及可查询路径，PID 重用按创建时间区分。所需信息未知时计数为未确定；具体目标条件要求取得匹配证据，默认全部目标仍可接收。KMD 系统记录按独立组件识别，进程归属取决于事件证据。

机器聚合端维护唯一有效配置。panel/host 提交整份筛选，由服务串行应用、递增 `revision`，返回实际有效值并通知双方。并发修改按应用顺序生效，客户端以 revision 更新显示；请求超时后查询实际状态。重试设置是新的替换操作，临时期限定为该次成功应用时开始。

可选 `duration_ms` 由服务单调时钟计时，到期恢复默认并通知。断连、关闭查看工具维持原配置和期限。服务启动时采用默认全部筛选并发布状态，本机转发开关独立持久保存，默认开启。

本机转发开关独立于筛选。本机关闭立即停止新日志入队、清空待发送日志并取消当前日志写请求；已被传输层接受的字节按恢复语义处理。控制状态继续可查询，宿主修改筛选维持开关状态，再次开启由 Windows 本机操作。响应同时包含开关和有效筛选。

## 4. 公开串口协议

VirtIO Serial 公共通道采用 UTF-8 JSONL，每行承载一条 JSON-RPC 2.0 消息或一组批量消息，协议契约由 OpenRPC 1.4.1 文档描述。方法包括 `endpoint.info`、`status.get`、`filter.set`、`filter.reset`、`system.info.get`、`rpc.discover`、`status.changed`、`log` 与 `gap`；端点发起请求时使用字符串 ID 和具名参数对象，通知携带方法名与参数。

`rpc.discover` 返回当前接收端的 OpenRPC 文档，便于查看端点方法与数据结构。序号、修订、计数、持续毫秒和 FILETIME 使用 DecimalU64 字符串表示；PID、线程 ID 和会话 ID 使用 JSON 整数。完整方法表、字段定义、示例与生成入口见 [Venera protocol v1](venera-protocol-v1.md)。

协议文档与双端接口由共享 Rust 类型生成并验证：

```bash
cargo run --manifest-path debug-channel/Cargo.toml -p venera-debug-protocol --example generate-v1 --features schema
cargo run --manifest-path debug-channel/Cargo.toml -p venera-debug-protocol --example generate-v1 --features schema -- --check
```

以下示例展示公开串口上的 JSON-RPC 2.0 请求、通知和端点发现：

```jsonl
{"jsonrpc":"2.0","id":"host-1","method":"status.get","params":{}}
{"jsonrpc":"2.0","method":"status.changed","params":{"forwarding_enabled":true,"revision":"0","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}}}
{"jsonrpc":"2.0","id":"host-2","method":"rpc.discover","params":{}}
{"jsonrpc":"2.0","method":"log","params":{"stream_id":"guest-run-1","seq":"42","component":"dxvk","level":"warning","text":"中文警告\n下一行"}}
```

### 元数据、时间与顺序

`source` 为采集路径，`component` 为生产组件。等级为 trace/debug/info/warning/error/fatal/unknown，另保留 `raw_level`；vkd3d fixme 保留原始值并归入 unknown。未知元数据采用共享协议的空值。DBWIN 提供 PID，启动时间和路径通过进程查询与事件关联补充；native 分片提供原始 TID。ETW 的 `source_time` 来自系统时间戳，转换为毫秒精度 UTC。更多字段核验继续保留实际事件样本。

`observed_at` 为来宾采集 UTC 时间；`source_time` 仅在来源具有可解释时间时填写，并说明精度与来源。只有日内本地时刻的文本保留在原文。宿主添加 `received_at` 表示完整有效记录接收时间；时钟域可能偏移。入库按宿主接收顺序，时间排序作为另一种视图。

`stream_id` 由服务启动 UTC 时间与进程 PID 组合。`seq` 在该运行内对筛选通过的日志于入队前递增，表示聚合到达顺序。筛选在编号前发生，已编号丢弃可报告范围；后台重启换 stream_id，链路重连保持。序号缺口说明已编号记录缺失，采集入口以前的损失依赖其他证据。协议中的序号字段使用 DecimalU64 字符串。

### 排队与连接恢复

每方向使用串行 writer，短写按完成字节续写当前行；控制优先选择下一条待写消息，当前 JSON 行保持边界。日志/控制队列均有有限预算。控制拥塞返回繁忙；日志拥塞淘汰最旧的尚未写入记录，累计 gap 后择机发送。

连接建立或 parser 重置后，接收方从收到的首字节累计至 LF，首段若为完整有效记录即正常消费；若为残留半行且解析失败，计数后在该 LF 边界恢复，继续解析后续行。连接建立后，双方通过 `endpoint.info` 与 `rpc.discover` 交换端点身份、软件版本、协议版本和接收接口；协商就绪后，宿主通过 `status.get` 获取状态。断连清理未完成半行，writer 收拢/取消旧 pending I/O，随后从完整记录开始；完整的旧排队记录仍可能到达。每条 JSON-RPC 记录可独立解析，端点发现消息延迟到达时后续日志通知继续消费。

按字节分帧后严格解码 UTF-8/JSON；坏行计数后继续，超长行流式丢弃到 LF。当前单记录上限为 1 MiB，日志队列为 2048 条/8 MiB，控制队列为 64 条；native 重组为 128 条/1 MiB、等待 5 秒。完整参数与来源处理见[来宾说明](virtio-serial-guest.md)，压力与重连实验继续核验预算。

分帧验证覆盖首条完整状态或日志记录的正常消费、残留半行加 LF 后的恢复、前导空行、坏行与超长行后的有效记录，以及连接建立后的日志消费；对 UTF-8 字节流的任意读取切点核对相同结果。真实断连下的缺口另由集成实验观测。

交付为有限缓冲下持续尽力发送。guest 写完成代表传输层消费；宿主完整接收与 SQLite 事务提交分别观测。断连、崩溃、采集或队列压力可造成缺口，精确可知时报告数量，否则 `count:null` 并记录时段/原因。筛选、本机开关、连接变化与解析异常保存为状态历史。

## 5. 保存与安装

宿主后台持续将日志、配置变化、连接及缺口写入 SQLite，保留原字段、宿主接收时间和入库顺序。接收命令通过 `--max-bytes` 显式设置容量；`store.rs` 使用页预算、FULL auto_vacuum 和最旧记录分批淘汰，每批约为当前记录数的十分之一。导出为 JSONL，包含 `received_at` 与原始 `message`，按导出开始时的入库上界读取；`--from` / `--until` 使用 RFC3339 时间选择宿主接收范围，包含起始时刻、以结束时刻为下一范围起点。范围导出已有 9 项测试通过，真实导出获得 1206 个事件。数据库页、索引、临时日志占用及空间回收继续实测。

当前实测环境为 Linux/QEMU，采用 QEMU socket server（`wait=off`）与 Rust 后台 client，由后台重连，VM 可先启动。专用端口名为 `org.venera.debug.0`，socket 路径按部署环境设置。部署与宿主命令见[宿主说明](virtio-serial-host.md)。

当前构建 `venera-debug-0.2.2.147.fd9eadb-r10.iso` / `.exe` 包含可选的官方 vioserial 驱动包与许可材料，panel 驱动安装复用安装器入口。无人值守整包安装使用 `/S /with-vioserial` 或 `/S /skip-vioserial`，单独安装驱动使用 `/vioserial [/S]`。Windows 11 教育版 10.0.22621 实测已安装官方 100.103.104.30200，发布名 `oem61.inf`，签名检查 True。LZX 载荷已通过 Windows WIMGAPI 10.0.22621.4745 全部 API 检查；r5、r6、r8、r9 的串口驱动安装 `/vioserial /S` 均实测成功。r9 该命令 Exit 0，日志包含 WIM、pnputil 259、problem 0 / ready 1、Done。

r9 完整安装 `/S /with-vioserial` 已发布 `oem64.inf` 并注册生产服务，随后复制卸载程序 AccessDenied、整包退出 1603；根因为 ISO 复制保留只读属性，历史日志为 `C:\ProgramData\Venera\setup-full-r9-failed.log`。r10 覆盖处理修复的四项实际文件测试 Windows Exit 0，完整 `/S /with-vioserial` 返回 3010，产品、Panel 与清理任务安装完成；卸载程序为 34,283,008 字节、Archive 属性。正常重启后已验证生产服务、会话采集、Panel 与宿主状态及新增日志入库。安装目录与服务命令见[来宾说明](virtio-serial-guest.md)。

驱动包就绪、控制器运行、命名端口出现、host 后端打开分别探测。服务供 panel 查询状态；panel 的 Debug channel 页已实际显示四项设备状态及默认 On。端口连接状态、宿主解析和提交进度分别观测。

Rust 公共消息类型位于 `debug-channel/protocol/src/lib.rs`，Venera protocol v1 的 OpenRPC 文档与 JSON Schema 由生成示例产出，字面测试向量与协议检查位于 `debug-channel/protocol/tests`。依赖版本记录于 `debug-channel/Cargo.lock`，构建接点为 `flake.nix`、根 CMake、CI 和安装器载荷；Windows 来宾 EXE 与 Linux 宿主程序已有构建产物。

## 6. 证据与待验证事项

实现核对：上述日志适配、`debug-channel/{guest,host,protocol}`、`panel/debugchannel.cpp`、`panel/debugpage.cpp`、`setup/main.cpp`、`setup/install.cpp`、`setup/CMakeLists.txt`、`pkg/venera.inf`、`flake.nix`。

运行证据：手动 `install-service` 注册和 `Start-Service VeneraDebugGuest` 均成功。首次来宾与宿主状态查询均为 driver installed、controller running、port present、host connected，转发为 true，revision 为 0，各筛选维度为全部。该次状态快照中 DBWIN、ETW、process_events、会话 1 均为 collecting，loss 为 0；宿主 SQLite 已保存真实 kernel ETW 记录。

panel 无期限 DXVK / warning 筛选提交返回 revision 6，手动 Reset 返回 revision 7、All；关闭 panel 后服务保持 Running。宿主接收端重启保持有效筛选及到期时间。v6 核心矩阵脚本结果为 true，其中命令行检查验证了关闭转发、服务重启后保持关闭、重新开启转发。全部接收与后代进程筛选各完成双日志器 134/134 条逐条完整比对，错误进程实例接收为 0。

路径筛选五轮 RepeatPath 结果为 success true，PID 为 10216、14020、14460、1280、5684，各日志器运行成功，每轮损失为 0；结束状态为 Running / On / All。实时 SQLite 与字面预期逐条比对每轮 67 条、共 335 条，正文、组件、级别、原始级别完全相同，missing、extra、missing_paths 均为 0。宿主证据为 `build/adapter-runtime-media/path-repeat-host-comparison.json`，来宾结果为 `C:\ProgramData\Venera\path-repeat-20261110-154036-333.json`。同实例身份保留与缺路径等待补全修复已有 15 项相关测试通过。

Windows 实际运行 `guest-tests.exe windows::ipc::tests --test-threads=1 --nocapture`，结果为 2 passed、0 failed、Exit 0，覆盖延迟读取和接收方停止读取时的刷新取消。

r8 Windows 本机通信测试扩展为 4 项，全部通过，Exit 0，用时 1.04 秒；新增监听端释放时的首次读取与空闲订阅取消测试，局部取消及等待线程结束的修复已有实机验证。r8 恢复日志器 PID 3388 的 67 条记录逐条完整匹配，证据为 `build/channel-stress-media/r8-recovery-host-comparison.json`。

Windows 来源实验共 29 项，29 项 success、Exit 0，环境恢复检查 `env_preserved` 为 true。按 PID 与本地矩阵核对全部 229 条字面预期的输出/门控结果及组件、级别、原始级别，宿主共接收 124 条，29 项比对均通过。证据为 `build/source-runtime-media/windows-host-comparison.json`；来源覆盖与下一步见上方台账。

关闭 panel PID 11240 后，日志器 PID 14300 的 67 条记录全部入库并逐条完整匹配。终止会话 1 helper PID 12740 后，5 秒观察到同一服务 PID 6652 启动新 helper PID 12400；服务保持 Running，采集状态 collecting，损失为 0。恢复后日志器 PID 14816 的 67 条记录再次完整匹配。证据为 `build/adapter-runtime-media/background-helper-host-comparison.json`。

官方来源：

- [virtio-win public.h](https://github.com/virtio-win/kvm-guest-drivers-windows/blob/defa22ad4ba72b9a35662da40e29b0df40e9fab8/vioserial/sys/public.h)：端口接口、IOCTL 与信息结构。
- [virtio-win Port.c](https://github.com/virtio-win/kvm-guest-drivers-windows/blob/defa22ad4ba72b9a35662da40e29b0df40e9fab8/vioserial/sys/Port.c)：打开/关闭、名称查询及连接状态。该源码快照与最终发行包的对应关系需另行核验。
- [QEMU virtio-serial-bus.c](https://github.com/qemu/qemu/blob/71f25c75417d31c214ba2cd26b273c114fc99987/hw/char/virtio-serial-bus.c)：连接、背压与断连队列处理。
- [Microsoft OutputDebugStringW](https://learn.microsoft.com/en-us/windows/win32/api/debugapi/nf-debugapi-outputdebugstringw)：调试输出、调试器和 Unicode 行为，作为 DBWIN 接入实验的 API 背景。
- [Microsoft Kernel Object Namespaces](https://learn.microsoft.com/en-us/windows/win32/termserv/kernel-object-namespaces)：会话与 Global 命名对象空间。

原生来源证据为 `build/native-source-media/windows-host-evidence.json`：恢复日志器 PID 4564 的 67 条正文与字面预期完全相同；Venus PID 14264 共 17 条，`vn_env` 多行正文精确匹配；原始 `vgpusrv.exe help` PID 10028 两条正文完整匹配。32/64 位来源证据为 `build/channel-stress-media/bitness-host-comparison.json`：PID 10780、1772、6936、15292 分别为真实 32、32、64、64 位，收到 4、5、4、5 条，级别正确。

r6 压力实验 PID 10852 共调用 20001 次，用时 931.991 ms，单次最大 2.330 ms；宿主收到 2260 条唯一记录，服务已分配序号的 9076 条等于 2260 条接收加 6816 条 gap。r7 PID 11200 调用 20001 次，用时 921.450 ms，单次最大 1.096 ms；宿主收到 2234 条，序号范围共 8228 条，等于 2234 接收加 5994 序号缺口。会话采集队列另报告 11052 条损失，包含 7622、3023 条补报；入口前段仍有 721 条待解释，transport_interrupted 的数量为 null。证据 `build/channel-stress-media/r7-host-comparison.json` 保留这一未知数量事件。r7 恢复日志器 PID 12252 的 67 条记录完整匹配。快速 Off/On 两次操作均成功，共 206.995 ms，结束状态 Running / On / All。

r9 状态新增 `health.session_helper_loss`，按会话独立提供 `pid`、`dropped_total`、`reported_total`，17 项测试、check 与 clippy 通过。r9 压力 PID 6672 调用 20001 次，用时 993.308 ms，单次最大 1.334 ms。独立 SQLite 比对确认 2202 条唯一记录，序号 1..9342 等于 2202 接收加 7140 服务缺口；helper 缺口为 10659，已知数量满足 `20001 = 2202 + 7140 + 10659`，差额为 0。Windows 排空后会话 1、helper PID 11124 的独立状态快照显示 dropped_total 与 reported_total 均为 10659。该轮仍记录一条 transport_interrupted、count 为 null 的未知数量事件。恢复日志器 PID 5148 的 67 条正文全部匹配。证据为 `build/channel-stress-media/r9-host-comparison.json`；r7 历史的 721 条仍单独保留，r9 为新一轮独立累计对账结果。

端口热移除与重接实测中，Windows `vport0p3` 为 IsPresent=False；2026-11-10 15:44:48 UTC 扫描退出 0 后仍缺失。原服务 PID 13696 保持 Running 直到整包更新，QEMU 为 host on、guest off，socket ACL 已恢复用户配置；该轮自动恢复验收失败。

r10 完整安装后，用户授权的正常 Windows 重启已执行，LastBootUpTime 为 2026/11/11 08:53:49。服务 Auto、Running、PID 3900，程序位于 `C:\Windows\System32\DriverStore\FileRepository\venera.inf_amd64_5464bc85676d1177\venera-debug-guest.exe`；会话 1 helper PID 3152 自动 collecting，端口在正常重启后恢复。生产 `%ProgramFiles%\Venera\venera-panel.exe` Debug 页显示 On、installed/running/present/connected、All、revision 2，宿主独立 status 同为 revision 2、On、All，损失计数全为 0。

恢复日志器 PID 14504、Exit 0，主端按 stream `2026-11-11T00:54:32.613Z-3900` 从 SQLite 逐条比对字面预期，67 条完整，missing 0、extra 0。证据为 `build/channel-stress-media/r10-reboot-host-comparison.json`，来宾记录为 `stress-recovery-20261111-091658-9186376.json`。

后续跟进包括分片重组损失上报在控制队列满时的报告保留，以及空闲末尾未完成分片的超时清理；当前清理在后续输入时触发。扩展实测包括宿主停读、调试器附加、`viogpuap` 默认显示管理、Venera 异常、WGL/zink 与直接输出旁路、RDP、多会话、热插拔自动重接、快速重连与串口在途写取消、SQLite 容量回收及更多应用场景。实施进度与完整验证目标见[实施计划](virtio-serial-plan.md)。
