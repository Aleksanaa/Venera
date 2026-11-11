# Venera 宿主调试记录

宿主后台持续接收 QEMU 调试端口的记录，保存来宾日志、有效筛选、连接变化与通道诊断。历史按接收顺序保存在 SQLite，容量达到预算后淘汰最旧记录。日志等级保留采集端提供的原始值与归一化值。

## 构建与运行

```sh
cd debug-channel
cargo build --release -p venera-debug-host
```

QEMU 为专用 virtserialport 提供 Unix socket listener，后台连接该路径并持续重连：

```sh
venera-debug-host receive \
  --socket /run/user/1000/venera-debug.sock \
  --control /run/user/1000/venera-control.sock \
  --database /home/user/.local/share/venera/history.db \
  --max-bytes 1073741824
```

先创建示例中的历史目录，并将路径替换为当前用户的实际路径。`receive` 持续运行，可交由 `systemd --user` 托管。每次连接建立后，后台发送端点介绍及状态查询。

`--max-bytes` 是历史库与 SQLite 工作空间的总容量预算，最小值为 65536 字节；其中预留事务工作空间，历史数据使用其余容量。历史按最旧记录分批淘汰，并在提交时回收数据库页。存储错误通过后台运行诊断报告。

## 用户服务部署

从仓库根目录运行，选择可访问的 QEMU socket 路径。以下命令将编译产物放在用户程序目录，并渲染服务模板中的启动命令：

```sh
nix develop --command cargo build --manifest-path debug-channel/Cargo.toml --release -p venera-debug-host
install -Dm755 debug-channel/target/release/venera-debug-host "$HOME/.local/bin/venera-debug-host"
mkdir -p "$HOME/.local/share/venera" "$XDG_RUNTIME_DIR/venera"
python3 scripts/venera-debug-host-service \
  --binary "$HOME/.local/bin/venera-debug-host" \
  --socket "$XDG_RUNTIME_DIR/venera/debug.sock" \
  --control "$XDG_RUNTIME_DIR/venera/control.sock" \
  --database "$HOME/.local/share/venera/history.db" \
  --max-bytes 1073741824 \
  --output "$HOME/.config/systemd/user/venera-debug-host.service"
systemd-analyze --user verify "$HOME/.config/systemd/user/venera-debug-host.service"
systemctl --user daemon-reload
systemctl --user enable --now venera-debug-host.service
systemctl --user status venera-debug-host.service
journalctl --user -u venera-debug-host.service -f
```

模板位于 `debug-channel/host/systemd/venera-debug-host.service.in`。脚本接受安装后的绝对路径，将路径转换为 systemd 参数格式；服务发生运行故障时由用户服务管理器重启。服务管理器创建 `$XDG_RUNTIME_DIR/venera` 运行目录；若 QEMU 使用持久目录，将 `--socket` 和 `--control` 换成对应目录中的路径。

```sh
systemctl --user stop venera-debug-host.service
systemctl --user start venera-debug-host.service
"$HOME/.local/bin/venera-debug-host" status --control "$XDG_RUNTIME_DIR/venera/control.sock"
```

停止服务由服务管理器发送终止信号；已提交历史在下次启动时继续使用。用户服务随用户服务管理器生命周期运行，需要开机后持续运行的部署可由管理员为该用户配置 lingering。

## QEMU 与 libvirt 端口

QEMU 命令示例，在虚拟机已有 virtio-serial 控制器时使用其总线名称：

```sh
-chardev socket,id=venera-debug,path=/run/user/1000/venera/debug.sock,server=on,wait=off \
-device virtserialport,bus=virtio-serial0.0,chardev=venera-debug,name=org.venera.debug.0
```

为使用 libvirt 的虚拟机，在现有 `<devices>` 中追加以下 channel；路径应与宿主 `--socket` 一致，并允许 QEMU 运行用户和宿主后台用户访问：

```xml
<channel type='unix'>
  <source mode='bind' path='/run/user/1000/venera/debug.sock'/>
  <target type='virtio' name='org.venera.debug.0'/>
</channel>
```

libvirt 的 `bind` 模式创建监听 socket，QEMU 启动后等待后台连接；命令行形式对应 `server=on,wait=off`。现有 SPICE channel 和其他设备与新增 channel 一同保留，由 libvirt 分配空闲 virtio-serial 端口。系统 libvirt 常使用 `/var/lib/libvirt/qemu/channel/target/` 下的域专属目录；实际 socket 目录及访问权限按当前域的运行用户配置。域配置保存后，在计划的虚拟机重启中启用端口。

## 查询与筛选

```sh
venera-debug-host status --control /run/user/1000/venera-control.sock
venera-debug-host filter-set --control /run/user/1000/venera-control.sock \
  --filter '{"components":["dxvk"],"levels":["warning","error"],"targets":[]}' \
  --duration-ms 60000
venera-debug-host filter-reset --control /run/user/1000/venera-control.sock
```

命令向持续运行的后台提交请求，后台转交来宾并返回对应响应。响应中的有效筛选、修订号、到期时间及转发状态来自来宾。`status` 查询当前有效状态。

筛选对已经产生的记录进行选择。组件、等级和目标三个维度取交集，同一维度内取并集；空数组匹配该维度全部值。默认筛选的三个数组均为空。目标支持进程实例及可执行文件完整路径。

## 保存导出

```sh
venera-debug-host export --database /home/user/.local/share/venera/history.db \
  > session.jsonl
```

按宿主接收时间选择导出范围：

```sh
venera-debug-host export --database /home/user/.local/share/venera/history.db \
  --from 2026-11-10T08:00:00Z --until 2026-11-10T09:00:00Z \
  > session-hour.jsonl
```

`--from` 包含起点，`--until` 在终点前截止；两者可单独使用。时间使用 RFC3339，支持小数秒和时区偏移，按对应 UTC 时刻比较。范围同时适用于日志、状态、连接及缺口记录，以宿主 `received_at` 为准。两个参数同时提供时，起点应早于终点。默认导出当前全部历史。

导出逐行生成 JSON，每行包含宿主接收 UTC 时间 `received_at` 与原始消息对象 `message`，保留消息的附加字段。导出开始时确定末尾位置，按接收顺序逐条读取当时仍保留的历史；后台在导出期间继续接收和淘汰，读取到某条记录之前已被淘汰的历史会跳过。入库编号持久递增，导出的截止位置对应开始时的历史批次。需要完整保存某个当前历史批次时，先停止服务，再运行导出。用户保存的文件独立于后台历史容量维护。

## 共享 Rust 接口

来宾通过 `venera-debug-protocol` 路径依赖共享契约。主要类型为 `Envelope`、`Message`、`LogRecord`、`ProcessIdentity`、`Filter`、`Target`、`Status`、`RequestResult` 和 `RequestError`。

`Envelope::new(Message)` 创建版本 1 消息；`encode` 生成带 LF 的 UTF-8 JSONL，`decode` 解析单条消息。`Framer::new(max_bytes)`、`push` 与 `reset` 支持增量读取；`push_raw` 同时返回类型化消息和完整 JSON 对象。`DecodeError` 区分编码、JSON、版本、未知类型与行长度错误。

`matches_filter(&filter, &record, &ancestors)` 进行纯函数匹配。`ancestors` 由调用方提供当前规则生命周期内已确认的祖先进程实例；规则管理与实例跟踪由来宾维护。路径输入使用来宾查询后的完整路径；Windows 构建调用系统 ordinal 大小写比较，Linux 构建采用 Unicode 小写比较。

机器可读契约见 `debug-channel/protocol/schema.json`；字面样本位于 `debug-channel/protocol/tests/vectors.jsonl`。

## 验证

```sh
cd debug-channel
cargo fmt --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python3 protocol/tests/schema.py
python3 host/tests/deployment.py
```

schema 验证脚本使用 Python `jsonschema`。Rust 测试覆盖字节切分、异常行恢复、筛选匹配，以及真实 Unix socket、CLI 控制、重连、重启、SQLite 淘汰与导出。

从仓库根目录运行宿主集成检查：

```sh
nix develop --command cargo test --manifest-path debug-channel/Cargo.toml -p venera-debug-host
python3 debug-channel/host/tests/deployment.py
```
