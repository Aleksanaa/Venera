import { act, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, test, vi } from 'vitest';
import { SystemInfo } from './SystemInfo';

const report = {
  major: 1,
  type: 'system.info.result',
  request_id: '17',
  ok: true,
  collected_at: '2026-11-11T10:00:00Z',
  session_id: 4,
  sections: [
    {
      title: 'Graphics',
      rows: [
        {
          key: 'Adapter',
          value:
            'Venera GPU\nC:\\Windows\\System32\\DriverStore\\very-long-driver-directory\\driver.dll',
          bad: false,
          warning: '',
        },
        {
          key: 'Driver',
          value: 'Degraded',
          bad: true,
          warning: '请检查驱动状态',
        },
      ],
    },
  ],
};
function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status });
}

const connection = {
  transport: 'VirtIO Serial',
  channel: 'org.venera.debug.0',
  socket_path: '/run/venera/debug.sock',
  connected: true,
  last_received_at: null,
};

function deferredResponse() {
  let resolve: (response: Response) => void = () => {};
  const promise = new Promise<Response>((finish) => {
    resolve = finish;
  });
  return { promise, resolve };
}

test('采集提示贯穿连接与报告读取，完成后展示报告', async () => {
  const connectionResponse = deferredResponse();
  const reportResponse = deferredResponse();
  vi.stubGlobal(
    'fetch',
    vi.fn((url: string) =>
      url === '/api/connection'
        ? connectionResponse.promise
        : reportResponse.promise,
    ),
  );
  render(<SystemInfo />);
  expect(
    screen.getByRole('status', { name: '正在采集系统信息…' }),
  ).toBeVisible();
  expect(screen.getByRole('button', { name: '正在刷新' })).toBeDisabled();
  await act(async () => connectionResponse.resolve(json(connection)));
  expect(screen.getByText('VirtIO Serial')).toBeVisible();
  expect(
    screen.getByRole('status', { name: '正在采集系统信息…' }),
  ).toBeVisible();
  expect(screen.getByRole('button', { name: '正在刷新' })).toBeDisabled();
  await act(async () => reportResponse.resolve(json(report)));
  expect(screen.getByText('Graphics')).toBeVisible();
  expect(screen.queryByRole('status')).not.toBeInTheDocument();
  expect(screen.getByRole('button', { name: '刷新' })).toBeEnabled();
});

test('刷新期间保留报告与时间，失败后重试更新报告', async () => {
  let pending: ReturnType<typeof deferredResponse> | null = null;
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) =>
      url === '/api/connection'
        ? json(connection)
        : (pending?.promise ?? json(report)),
    ),
  );
  const user = userEvent.setup();
  render(<SystemInfo />);
  expect(await screen.findByText('Graphics')).toBeVisible();
  pending = deferredResponse();
  await user.click(screen.getByRole('button', { name: '刷新' }));
  expect(screen.getByText('Degraded')).toBeVisible();
  expect(screen.getByText('2026-11-11T10:00:00Z')).toBeVisible();
  expect(screen.getByRole('button', { name: '正在刷新' })).toBeDisabled();
  expect(
    screen.getByRole('status', { name: '正在采集系统信息…' }),
  ).toBeVisible();
  await act(async () =>
    pending?.resolve(json({ error: '客户机暂时离线' }, 503)),
  );
  expect(screen.getByText('客户机暂时离线')).toBeVisible();
  expect(screen.getByText('Degraded')).toBeVisible();
  expect(screen.getByText('2026-11-11T10:00:00Z')).toBeVisible();
  expect(screen.queryByRole('status')).not.toBeInTheDocument();
  pending = deferredResponse();
  await user.click(screen.getByRole('button', { name: '刷新' }));
  expect(screen.queryByText('客户机暂时离线')).not.toBeInTheDocument();
  expect(screen.getByText('2026-11-11T10:00:00Z')).toBeVisible();
  expect(
    screen.getByRole('status', { name: '正在采集系统信息…' }),
  ).toBeVisible();
  await act(async () =>
    pending?.resolve(json({ ...report, collected_at: '2026-11-11T11:30:00Z' })),
  );
  expect(screen.getByText('2026-11-11T11:30:00Z')).toBeVisible();
  expect(screen.queryByText('2026-11-11T10:00:00Z')).not.toBeInTheDocument();
  expect(screen.queryByRole('status')).not.toBeInTheDocument();
  expect(screen.getByRole('button', { name: '刷新' })).toBeEnabled();
});

test('展示连接响应中的实际协议与服务版本', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) =>
      json(
        url === '/api/connection'
          ? {
              ...connection,
              protocol_version: '1.4.2',
              software_version: '0.1.7',
            }
          : report,
      ),
    ),
  );
  render(<SystemInfo />);
  expect(await screen.findByText('Graphics')).toBeVisible();
  const protocol = screen.getByText('调试通道协议版本').parentElement;
  const software = screen.getByText('调试服务版本').parentElement;
  if (!protocol || !software) throw new Error('版本字段缺少显示区域');
  expect(within(protocol).getByRole('definition')).toHaveTextContent('1.4.2');
  expect(within(software).getByRole('definition')).toHaveTextContent('0.1.7');
});

test.each([{}, { protocol_version: null, software_version: null }])(
  '等待版本字段时展示占位：%j',
  async (versions) => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string) =>
        json(
          url === '/api/connection' ? { ...connection, ...versions } : report,
        ),
      ),
    );
    render(<SystemInfo />);
    expect(await screen.findByText('Graphics')).toBeVisible();
    const protocol = screen.getByText('调试通道协议版本').parentElement;
    const software = screen.getByText('调试服务版本').parentElement;
    if (!protocol || !software) throw new Error('版本字段缺少显示区域');
    expect(within(protocol).getByRole('definition')).toHaveTextContent('—');
    expect(within(software).getByRole('definition')).toHaveTextContent('—');
  },
);

test('各报告分区保留完整路径、多行诊断与驱动版本', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) =>
      json(
        url === '/api/connection'
          ? {
              ...connection,
              protocol_version: '1.0.0',
              software_version: '0.1.0',
            }
          : {
              ...report,
              sections: [
                ...report.sections,
                {
                  title: 'System',
                  rows: [
                    {
                      key: 'Driver version',
                      value: '0.2.2',
                      bad: true,
                      warning: '第一行诊断\n第二行诊断：请检查显示适配器',
                    },
                  ],
                },
              ],
            },
      ),
    ),
  );
  render(<SystemInfo />);
  expect(await screen.findByRole('heading', { name: 'System' })).toBeVisible();
  expect(screen.getByRole('heading', { name: 'Graphics' })).toBeVisible();
  expect(screen.getByText('0.2.2')).toBeVisible();
  expect(screen.getByText('0.1.0')).toBeVisible();
  expect(screen.getByText('1.0.0')).toBeVisible();
  expect(screen.getByText(/Venera GPU/)).toHaveTextContent(
    'Venera GPU C:\\Windows\\System32\\DriverStore\\very-long-driver-directory\\driver.dll',
  );
  expect(screen.getByText(/第一行诊断/)).toHaveTextContent(
    '第一行诊断 第二行诊断：请检查显示适配器',
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

test('展示连接上下文、系统字段与诊断提示，刷新失败保留报告并可恢复', async () => {
  let failed = false;
  let updated = false;
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      if (failed) return json({ error: '采集通道尚未配置' }, 503);
      if (url === '/api/connection')
        return json({
          transport: 'VirtIO Serial',
          channel: 'org.venera.debug.0',
          socket_path: '/run/venera/debug.sock',
          connected: updated,
          last_received_at: updated ? '2026-11-11T10:02:00Z' : null,
        });
      return json(
        updated
          ? { ...report, collected_at: '2026-11-11T10:02:00Z', session_id: 5 }
          : report,
      );
    }),
  );
  const user = userEvent.setup();
  render(<SystemInfo />);
  expect(await screen.findByText('Graphics')).toBeVisible();
  expect(screen.getByText('VirtIO Serial')).toBeVisible();
  expect(screen.getByText('/run/venera/debug.sock')).toBeVisible();
  expect(screen.getByText(window.location.href)).toBeVisible();
  expect(screen.getByText('等待连接')).toBeVisible();
  expect(screen.getByText('等待首条消息')).toBeVisible();
  expect(
    screen.getByText(
      'Venera GPU C:\\Windows\\System32\\DriverStore\\very-long-driver-directory\\driver.dll',
    ),
  ).toBeVisible();
  expect(screen.getByText('异常')).toBeVisible();
  expect(screen.getByText('请检查驱动状态')).toBeVisible();
  expect(screen.getByText(/会话：4/)).toBeVisible();
  failed = true;
  await user.click(screen.getByRole('button', { name: '刷新' }));
  expect(await screen.findAllByText('采集通道尚未配置')).toHaveLength(2);
  expect(screen.getByText('Degraded')).toBeVisible();
  expect(screen.getByText(/最后采集时间/)).toHaveTextContent(
    '2026-11-11T10:00:00Z',
  );
  failed = false;
  updated = true;
  await user.click(screen.getByRole('button', { name: '刷新' }));
  expect(await screen.findByText('已连接')).toBeVisible();
  expect(screen.getByText(/会话：5/)).toHaveTextContent('2026-11-11T10:02:00Z');
});

test('初次采集失败展示实际错误，刷新后显示系统报告', async () => {
  let recovered = false;
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      if (url === '/api/connection')
        return json({ error: '接收端未配置' }, 503);
      return json(
        recovered
          ? report
          : {
              ...report,
              ok: false,
              error: { code: 'unavailable', message: '客户机尚未响应' },
            },
      );
    }),
  );
  const user = userEvent.setup();
  render(<SystemInfo />);
  expect(await screen.findByText('客户机尚未响应')).toBeVisible();
  expect(screen.getByText('接收端未配置')).toBeVisible();
  recovered = true;
  await user.click(screen.getByRole('button', { name: '刷新' }));
  expect(await screen.findByText('Graphics')).toBeVisible();
});

test('系统报告保持本次采集直到用户刷新', async () => {
  vi.useFakeTimers();
  let calls = 0;
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      calls++;
      if (url === '/api/connection')
        return json({
          transport: 'VirtIO Serial',
          channel: 'org.venera.debug.0',
          socket_path: '/run/debug.sock',
          connected: true,
          last_received_at: null,
        });
      return json({
        ...report,
        collected_at: calls > 2 ? '2026-11-11T11:00:00Z' : report.collected_at,
      });
    }),
  );
  await act(async () => {
    render(<SystemInfo />);
  });
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5000);
  });
  expect(screen.getByText('2026-11-11T10:00:00Z')).toBeVisible();
});

test('连接响应完成后采集系统报告，每次刷新依次完成两项读取', async () => {
  let finishConnection: (response: Response) => void = () => {};
  const requests: string[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn((url: string) => {
      requests.push(url);
      if (url === '/api/connection')
        return new Promise<Response>((resolve) => {
          finishConnection = resolve;
        });
      return Promise.resolve(json(report));
    }),
  );
  const user = userEvent.setup();
  render(<SystemInfo />);
  expect(screen.getByRole('button', { name: '正在刷新' })).toBeDisabled();
  expect(requests).toEqual(['/api/connection']);
  await act(async () =>
    finishConnection(
      json({
        transport: 'VirtIO Serial',
        channel: 'org.venera.debug.0',
        socket_path: '/run/debug.sock',
        connected: true,
        last_received_at: null,
      }),
    ),
  );
  expect(await screen.findByText('Graphics')).toBeVisible();
  expect(requests).toEqual(['/api/connection', '/api/system-info']);
  await user.click(screen.getByRole('button', { name: '刷新' }));
  expect(requests).toEqual([
    '/api/connection',
    '/api/system-info',
    '/api/connection',
  ]);
  await act(async () =>
    finishConnection(json({ error: '接收端暂时离线' }, 503)),
  );
  expect(await screen.findByText('接收端暂时离线')).toBeVisible();
  expect(requests).toEqual([
    '/api/connection',
    '/api/system-info',
    '/api/connection',
    '/api/system-info',
  ]);
  expect(screen.getByText('Degraded')).toBeVisible();
});

test.each([200, 503])(
  '连接错误包展示具体消息并继续采集报告（HTTP %s）',
  async (status) => {
    let recovered = false;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string) => {
        if (url === '/api/connection')
          return recovered
            ? json({
                transport: 'VirtIO Serial',
                channel: 'org.venera.debug.0',
                socket_path: '/run/debug.sock',
                connected: true,
                last_received_at: null,
              })
            : json(
                {
                  ok: false,
                  error: {
                    code: 'host.busy',
                    message: '调试通道正在处理请求，请稍后重试',
                  },
                },
                status,
              );
        return json(report);
      }),
    );
    const user = userEvent.setup();
    render(<SystemInfo />);
    expect(
      await screen.findByText('调试通道正在处理请求，请稍后重试'),
    ).toBeVisible();
    expect(await screen.findByText('Graphics')).toBeVisible();
    expect(screen.queryByText('等待连接')).not.toBeInTheDocument();
    expect(screen.queryByText('已连接')).not.toBeInTheDocument();
    expect(screen.queryByText('通道名称')).not.toBeInTheDocument();
    recovered = true;
    await user.click(screen.getByRole('button', { name: '刷新' }));
    expect(await screen.findByText('已连接')).toBeVisible();
    expect(screen.getByText('org.venera.debug.0')).toBeVisible();
  },
);
