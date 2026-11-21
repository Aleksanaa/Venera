export type LocalizedMessage = {
  id: string;
  args?: string[];
  diagnostic?: string | null;
};

const messages: Record<string, readonly [string, string]> = {
  'report.gpuFailed': ['GPU query failed ({0})', 'GPU 查询失败（{0}）'],
  'report.driver': ['Driver', '驱动'],
  'report.version': ['Version', '版本'],
  'report.installed': ['Installed', '安装时间'],
  'report.device': ['Device', '设备'],
  'report.noDevice': ['No VirtIO GPU found', '未找到 VirtIO GPU'],
  'report.deviceProblem': [
    'Device Manager reports a problem, code {0}',
    '设备管理器报告问题，代码 {0}',
  ],
  'report.deviceDriver': ['Device driver', '设备驱动'],
  'report.restart': [
    'Restart Windows to run the driver installed last',
    '请重启 Windows 以运行最近安装的驱动',
  ],
  'report.driverVersion': ['Driver version', '驱动版本'],
  'report.versionMismatch': [
    "Not the installed Venera's ({0}): install Venera again",
    '与已安装的 Venera（{0}）不符：请重新安装 Venera',
  ],
  'report.driverFiles': ['Driver files', '驱动文件'],
  'report.filesMixed': [
    'Not all from the installed driver',
    '部分文件来自其他驱动目录',
  ],
  'report.filesOutside': [
    "Loaded from outside the driver's folder: {0}",
    '从驱动目录之外加载：{0}',
  ],
  'report.filesInstalled': [
    'Loaded from the installed driver',
    '已从安装的驱动目录加载',
  ],
  'report.testSigning': ['Test signing', '测试签名'],
  'report.signingOff': [
    'Off, so Windows does not load the driver',
    '已关闭，Windows 无法加载驱动',
  ],
  'report.host': ['Host', '宿主机'],
  'report.vulkanDriver': ['Vulkan driver', 'Vulkan 驱动'],
  'report.venusUnknown': [
    'Unknown: Venus did not answer',
    '未知：Venus 未响应',
  ],
  'report.acceleration': ['3D acceleration', '3D 加速'],
  'report.accelerationOff': [
    'Off: QEMU needs virtio-vga-gl or virtio-gpu-gl',
    '已关闭：QEMU 需要 virtio-vga-gl 或 virtio-gpu-gl',
  ],
  'report.venusOff': [
    'Off: add venus=on to the virtio-gpu device',
    '已关闭：请为 virtio-gpu 设备添加 venus=on',
  ],
  'report.blob': ['Blob resources', 'Blob 资源'],
  'report.blobOff': [
    'Off: add blob=on to the virtio-gpu device',
    '已关闭：请为 virtio-gpu 设备添加 blob=on',
  ],
  'report.hostMemory': ['Host memory window', '宿主内存窗口'],
  'report.hostMemoryOff': [
    'None: add hostmem=4G to the virtio-gpu device',
    '未配置：请为 virtio-gpu 设备添加 hostmem=4G',
  ],
  'report.context': ['Context init', '上下文初始化'],
  'report.contextOff': [
    'Off: the host needs virglrenderer 1.0 or newer',
    '已关闭：宿主机需要 virglrenderer 1.0 或更新版本',
  ],
  'report.features': ['Features', '功能'],
  'report.driverUnknown': [
    'Unknown: the driver did not answer',
    '未知：驱动未响应',
  ],
  'report.guest': ['Guest', '虚拟机'],
  'report.vm': ['Virtual machine', '虚拟机'],
  'report.machine': ['Machine type', '机型'],
  'report.processor': ['Processor', '处理器'],
  'report.topology': ['Topology', '处理器拓扑'],
  'report.topologyValue': [
    'Sockets: {0}, cores: {1}, threads: {2}',
    '{0} 个插槽，{1} 个核心，{2} 个线程',
  ],
  'report.socketsWarning': [
    "You likely configured each core as its own socket, and Windows uses at most 2: change to 1 socket with at least 4 cores. With fewer cores, applications race the drivers' translation threads for them, which makes rendering slow and can leak memory",
    '您可能为每个核心配置了独立插槽，而 Windows 最多使用 2 个插槽。请改为 1 个插槽、至少 4 个核心。核心过少会导致应用与驱动的转换线程争用处理器，降低渲染速度，并可能泄漏内存',
  ],
  'report.coresWarning': [
    "Give the virtual machine 1 socket with at least 4 cores. With fewer cores, applications race the drivers' translation threads for them, which makes rendering slow and can leak memory",
    '请为虚拟机配置 1 个插槽、至少 4 个核心。核心过少会导致应用与驱动的转换线程争用处理器，降低渲染速度，并可能泄漏内存',
  ],
  'report.memory': ['Memory', '内存'],
  'report.memoryValue': ['{0} ({1} usable)', '{0}（可用 {1}）'],
  'report.swap': ['Swap', '交换空间'],
  'report.display': ['Display', '显示器'],
  'report.displayNumber': ['Display {0}', '显示器 {0}'],
  'report.displayValue': [
    '{0} × {1} at {2} Hz, {3}% scale',
    '{0} × {1}，{2} Hz，缩放 {3}%',
  ],
  'report.displayMissing': ['None attached', '未连接'],
  'report.mouse': ['Mouse', '鼠标'],
  'report.keyboard': ['Keyboard', '键盘'],
  'report.input': ['Input', '输入设备'],
  'report.inputMissing': ['No mouse or keyboard found', '未找到鼠标或键盘'],
  'report.apis': ['Graphics APIs', '图形 API'],
  'report.featureLevel': ['Feature level {0}', '功能级别 {0}'],
  'report.d3d12Features': [
    'Feature level {0}, shader model {1}',
    '功能级别 {0}，着色器模型 {1}',
  ],
  'report.d3d9On12': ['Through D3D9On12', '通过 D3D9On12'],
  'report.d3d12Needed': ['Needs Direct3D 12', '需要 Direct3D 12'],
  'report.otherVulkan': ['Other Vulkan driver', '其他 Vulkan 驱动'],
  'report.otherVulkanWarning': [
    "Applications may use it instead of Venera's",
    '应用可能优先使用该驱动',
  ],
  'report.otherVulkanWarnings': [
    "Applications may use these instead of Venera's",
    '应用可能优先使用这些驱动',
  ],
  'report.softwareGl': [
    "Windows' software renderer, not the GPU",
    'Windows 软件渲染器',
  ],
  'report.videoMemory': ['Video memory', '显存'],
  'report.dedicated': ['Dedicated', '专用'],
  'report.shared': ['Shared', '共享'],
  'report.budget': ['Budget per program', '每个程序的预算'],
  'report.budgetValue': ['{0} local, {1} shared', '本地 {0}，共享 {1}'],
  'report.heap': ['Vulkan heap {0}', 'Vulkan 内存堆 {0}'],
  'report.heapLocal': ['{0}, device-local', '{0}，设备本地'],
  'report.heapMappable': ['{0}, CPU-mappable', '{0}，可映射到 CPU'],
  'report.heapBoth': [
    '{0}, device-local, CPU-mappable',
    '{0}，设备本地，可映射到 CPU',
  ],
  'report.memoryUnknown': [
    'Unknown: no API answered',
    '未知：图形 API 均未响应',
  ],
  'report.changedSettings': ['Changed settings', '已更改的设置'],
  'report.environment': ['{0} (environment variable)', '{0}（环境变量）'],
  'report.defaultSettings': ['All at their defaults', '全部使用默认值'],
  'report.checkCrashed': ['The check crashed', '检查进程崩溃'],
  'report.checkTimeout': ['The check stopped responding', '检查停止响应'],
  'report.notChecked': ['Not checked', '尚未检查'],
  'report.noAdapter': ['No VirtIO GPU adapter', '未找到 VirtIO GPU 适配器'],
  'report.noLoader': [
    'The Vulkan Runtime is not installed',
    '尚未安装 Vulkan 运行时',
  ],
  'report.noVenus': ['No Venus device', '未找到 Venus 设备'],
  'report.apiFailed': ['Failed ({0})', '失败（{0}）'],
  'report.notInstalled': ['Not installed', '尚未安装'],
  'report.checking': ['Checking…', '正在检查…'],
  'report.none': ['None', '无'],
  'report.on': ['On', '开'],
  'report.settings': ['Settings', '设置'],
};

export function reportMessage(
  fallback: string,
  message?: LocalizedMessage | null,
  language = 'en',
): string {
  if (!message || !Object.hasOwn(messages, message.id)) return fallback;
  const template = messages[message.id][language === 'zh-CN' ? 1 : 0];
  const args = message.args ?? [];
  const slots = [...template.matchAll(/\{(\d+)\}/g)];
  if (slots.some((slot) => typeof args[Number(slot[1])] !== 'string'))
    return fallback;
  return (
    template.replace(/\{(\d+)\}/g, (_, index) => args[Number(index)]) +
    (message.diagnostic ?? '')
  );
}
