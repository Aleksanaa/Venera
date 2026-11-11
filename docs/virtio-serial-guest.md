# Windows 调试后台

`venera-debug-guest.exe` 汇聚登录会话调试输出、系统调试事件及 Venera 引擎记录，通过 `org.venera.debug.0` 发送共享 JSONL 消息。服务名为 `VeneraDebugGuest`，服务以 LocalSystem 自动启动；各登录会话运行同一程序的 `session-helper` 模式，包括保留登录的 RDP 断开会话。

## 服务安装

当前发行物提供 `venera-debug-0.2.2.147.fd9eadb-r10.iso` 和同名 `.exe` 安装器，集成验证版本为 integration v13，包含可选的官方 VirtIO Serial 驱动包。r10 已完成 Windows 完整安装、正常重启及生产采集验证；资源、WIM、14 项组件、CAT 与 SYS 内容比对检查已通过。交互安装时选择 VirtIO serial 驱动选项；无人值守安装显式选择驱动选项：

```powershell
.\venera-setup.exe /S /with-vioserial
# 使用已有的 VirtIO Serial 驱动
.\venera-setup.exe /S /skip-vioserial
```

单独安装随包的 VirtIO Serial 驱动：

```powershell
.\venera-setup.exe /vioserial /S
```

`/vioserial` 也支持交互安装。控制面板安装到 `%ProgramFiles%\Venera`，Debug channel 页共用驱动安装入口。调试服务程序随显示驱动包进入 Windows Driver Store，由 INF 注册服务；panel 按服务配置查找程序。

手动部署调试服务时，将 `venera-debug-guest.exe` 放入稳定目录，切换到该目录后在提升权限的 PowerShell 中注册并启动：

```powershell
.\venera-debug-guest.exe install-service
Start-Service VeneraDebugGuest
```

服务的启动命令为 `"完整路径\venera-debug-guest.exe" service`，直接启动程序也进入服务调度。卸载入口为：

```powershell
.\venera-debug-guest.exe uninstall-service
```

## panel 与命令行

每次查询与修改返回一行 UTF-8 JSON。panel 使用以下命令查询与控制服务，并解析返回值；命令行调用以 `venera-debug-guest.exe` 加表中参数执行。

| 命令 | 返回内容 |
|---|---|
| `status --json` | 共享 `Status` 对象：`forwarding_enabled`、`revision`、`effective_filter`、`expires_at`、`health`、`loss`。 |
| `forwarding on` / `forwarding off` | `{"ok":true,"status":{...}}`。需提升权限。 |
| `filter-set --json FILE` | 共享 `request.result` envelope，包含服务实际采用的配置与 revision。需提升权限。 |
| `filter-reset` | 共享 `request.result` envelope。需提升权限。 |
| `logs --follow` | 持续 JSONL：首先是共享 `status` envelope，随后是筛选后的 `log` 和状态通知。 |

`filter-set` 的文件使用 UTF-8 JSON：

```json
{
  "filter": {
    "components": ["dxvk", "venus"],
    "levels": [],
    "targets": [
      {"kind":"exe_path","path":"C:\\Games\\demo.exe"},
      {"kind":"process_instance","pid":4120,"start_time_filetime":"134124000000000000","include_descendants":true}
    ]
  },
  "duration_ms": 60000
}
```

也可直接使用共享 `Filter` 对象作为文件内容。各维度为空数组表示全部；组件取实际识别的来源集合。设置成功时服务增加 revision；到期恢复默认并发布状态。服务启动时采用默认筛选。本机转发默认开启，开关持久保存在 `HKLM\SOFTWARE\Venera\DebugChannel\ForwardingEnabled`。

`status --json` 的示例结构：

```json
{"forwarding_enabled":true,"revision":0,"effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{"driver":"installed","controller":{"state":"running","problem":0},"port":"present","host":"connected","dbwin":"collecting","etw":"collecting","sessions":[{"session_id":1,"connection_state":0,"helper":"running","error":null}],"session_capture":{"1":"collecting"}},"loss":{"queue_dropped":0,"parse_errors":0}}
```

字段中的状态来自实际探测；发生采集或设备异常时对应字段携带 `error`。修改失败通过 `ok:false` 和 `error` 对象返回，命令同时报告失败。`logs --follow` 使用实时有界订阅；重新启动订阅命令会先取得当前状态。

## 运行验证进度

2026-11-10，在 Windows 11 教育版 10.0.22621 上，官方 vioserial 100.103.104.30200 已安装为 `oem61.inf`，签名检查为 True。手动注册服务成功，随后 `Start-Service VeneraDebugGuest` 启动成功。

来宾和宿主状态查询均返回驱动已安装、控制器运行、端口存在、宿主已连接，转发开启，revision 为 0，接收筛选各维度为全部。panel 的 Debug channel 页已实际显示这四项设备状态及默认 On。该次状态快照中 DBWIN、ETW、进程事件和会话 1 均为 collecting，损失计数为 0；宿主 SQLite 已保存真实 kernel ETW 记录。

panel 已提交无期限的 DXVK / warning 筛选，服务返回 revision 6；手动 Reset 后 revision 7、各维度恢复 All，关闭 panel 后服务保持 Running。宿主接收端重启后，有效筛选及到期时间保持一致；命令行检查已验证关闭转发、重启服务后保持关闭、重新开启转发。

DXVK 与 vkd3d 两个日志器在全部接收和后代进程筛选下，均完成 134/134 条逐条完整比对；指定错误进程实例时接收为 0。路径筛选五轮复验成功，每轮 67 条、共 335 条，正文、组件、级别、原始级别与字面预期完全相同，记录及路径完整，每轮损失为 0。结束时服务为 Running、转发 On、筛选 All。

安装载荷已通过 Windows WIMGAPI 10.0.22621.4745 全部 API 检查。r5、r6 生产安装器执行 `/vioserial /S` 均成功；r6 完成载荷展开及驱动安装，控制器运行正常。Windows 本机通信的延迟读取与停止读取时的刷新取消测试均通过。

r8 串口驱动安装实测成功，完成 17 个载荷文件处理，控制器运行正常，服务保持 Running 并沿用原配置的测试目录。r8 Windows 本机通信四项测试全部通过，用时 1.04 秒，覆盖读取、刷新及监听端释放时的取消处理；恢复后 67 条日志逐条完整匹配。

来源实验 29 项全部通过，宿主收到 124 条记录，输出门控、组件、级别与原始级别均符合字面预期。已覆盖 Venera 日志等级与 verbose 开关、Mesa util 各级别和 `MESA-VIRTIO` 标识、DXVK 五档及关闭输出配置、vkd3d 的构建门控与 API/shader 独立配置。

关闭 panel 后新增的 67 条日志已完整入库；会话 1 采集进程终止后，5 秒观察到自动恢复，服务持续运行，采集正常且损失为 0，随后新增的 67 条日志再次完整入库。

真实 vkprobe 加载 Venus DLL 后已采集完整多行环境与 renderer 连接日志；探测程序成功退出并报告两项功能缺口。原始分辨率服务 `vgpusrv.exe help` 的两条日志完整入库。32 位和 64 位来源的四组样本分别收到 4、5、4、5 条，级别正确。恢复日志器的 67 条正文全部匹配。快速关闭、开启转发共用时 206.995 ms，结束状态 Running / On / All。

r9 高频实验完成 20001 次调用，用时 993.308 ms，单次最大 1.334 ms；2202 条唯一入库记录、7140 条服务缺口及 10659 条会话采集缺口的已知数量精确闭合。排空后的独立状态快照确认会话采集累计丢弃与已上报数量均为 10659。该轮另有一条传输中断事件，其损失数量为未知。恢复后 67 条日志完整匹配，r9 串口驱动安装成功。r9 的会话损失诊断已有 17 项测试及静态检查通过；v13 来宾程序与 v12 逐字节一致。

r10 四项文件复制测试已在 Windows 全部通过，完整安装返回 3010，产品、Panel 与清理任务安装完成。随后已正常重启 Windows，服务从 Driver Store 自动启动，登录会话自动采集。生产 Panel 显示转发 On、驱动已安装、控制器运行、端口存在、宿主已连接，筛选 All、revision 2；宿主查询同样返回 revision 2、On、All，损失计数全为 0。重启后新增 67 条日志逐条完整入库。

此前端口热移除与重接后，Windows 扫描仍显示端口缺失，该轮自动恢复验收失败；端口已在此次正常重启后恢复。扩展验证包括热插拔自动重接、默认显示管理、Venera 异常、WGL/zink 与旁路来源、RDP、多会话、调试器附加和宿主停读；分片损失报告与空闲超时清理的后续跟进见[实施计划](virtio-serial-plan.md)。

## 来源与元数据

识别组件为 `venera_d3d11`、`venera_d3d12`、`mesa`、`venus`、`dxvk`、`vkd3d`、`vkd3d-shader`、`kernel` 和 `service`。Venus 使用 `MESA-VIRTIO` tag。服务/辅助程序日志按安装目录中的实际程序路径识别。内核记录按已识别的 VioGpu 文本来源归类。

DXVK/vkd3d 的 `VDBG1` 记录按照 native 适配的生产者、线程、序列和分片编号重组，拼接原始字节后严格解码 UTF-8。普通 DBWIN 内容优先解码 UTF-8，其他字节按 Windows 当前 ANSI 代码页转换。完整正文保存在 `text`，原始级别保存在 `raw_level`；来源未携带级别时使用 `unknown`，获取到的进程创建时间采用十进制 FILETIME。ETW 的 `source_time` 由系统时间戳转换为 UTC，显示精度为毫秒。其他未知字段使用共享协议的空值。

日志发送队列容量为 2048 条、8 MiB；会话交接队列为 2048 条；控制队列为 64 条。native 重组容量为 128 条、1 MiB，重组等待为 5 秒。发生队列淘汰、native 记录异常或链路中断时生成相应 `gap`。控制消息在完整日志行之间优先发送。

服务在自身 ETW 会话中接收进程创建与退出事件，维护进程创建时间、父进程、会话及完整程序路径。历史父子关系可跨父进程退出保留，PID 再次分配时按创建时间分别匹配。两秒进程快照补充当前进程信息。携带后代规则的记录可等待进程事件完成身份关联，等待队列为 256 条、每条至多 4092 字节，等待期限为 5 秒。进程事件处理与已观测 ETW 损失通过状态查询呈现。

## 构建

在包含 `guest` 的 workspace 中运行：

```sh
nix develop --command cargo test --manifest-path debug-channel/Cargo.toml -p venera-debug-guest
nix develop --command cargo build --manifest-path debug-channel/Cargo.toml -p venera-debug-guest --target x86_64-pc-windows-gnu
```

Windows GNU 产物名为 `venera-debug-guest.exe`。
