import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, test, vi } from 'vitest';
import { App } from './App';

const first = {
  id: '9007199254740993',
  received_at: '2026-11-11T09:00:00Z',
  message: {
    type: 'log',
    component: 'renderer',
    level: 'info',
    text: '帧提交完成',
    process: { pid: 42, exe_path: 'C:\\demo.exe' },
  },
  raw_message: '{"type":"log","text":"帧提交完成"}',
};
const second = {
  id: '9007199254740994',
  received_at: '2026-11-11T09:00:01Z',
  message: {
    type: 'log',
    component: 'renderer',
    level: 'error',
    text: '设备重试\n恢复成功',
  },
  raw_message: '{"text":"设备重试\\n恢复成功"}',
};
function response(body: unknown) {
  return new Response(JSON.stringify(body), {
    headers: { 'Content-Type': 'application/json' },
  });
}
function page(events: unknown[]) {
  return response({ events, oldest_id: '1', latest_id: '9007199254740994' });
}
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

test('筛选组件、级别、程序、正文和时间，并导出当前结果', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: string) => {
      const url = new URL(input, 'http://localhost');
      if (url.pathname === '/api/events/9007199254740993')
        return response(first);
      const p = url.searchParams;
      if (
        p.get('component') === 'renderer' &&
        p.get('level') === 'error' &&
        p.get('program') === 'demo.exe' &&
        p.get('q') === '设备' &&
        p.has('from') &&
        p.has('until')
      )
        return page([second]);
      return page([first]);
    }),
  );
  const user = userEvent.setup();
  render(<App />);
  expect(await screen.findByText('帧提交完成')).toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '筛选' }));
  const history = screen.getByRole('region', { name: '事件历史' });
  expect(within(history).getByText('帧提交完成')).toBeVisible();
  await user.click(
    within(history).getByRole('button', { name: '查看事件 9007199254740993' }),
  );
  expect(screen.getByText('42')).toBeVisible();
  expect(screen.getByRole('region', { name: '筛选事件' })).toBeVisible();
  await user.type(screen.getByLabelText('组件'), 'renderer');
  await user.type(screen.getByLabelText('级别'), 'error');
  await user.type(screen.getByLabelText('程序'), 'demo.exe');
  await user.type(screen.getByLabelText('正文搜索'), '设备');
  fireEvent.change(screen.getByLabelText('开始时间'), {
    target: { value: '2026-11-11T00:00' },
  });
  fireEvent.change(screen.getByLabelText('结束时间'), {
    target: { value: '2026-11-12T00:00' },
  });
  await user.click(screen.getByRole('button', { name: '应用筛选' }));
  expect(await screen.findByText('设备重试 恢复成功')).toBeInTheDocument();
  expect(screen.getByLabelText('组件')).toHaveValue('renderer');
  expect(screen.getByRole('button', { name: '应用筛选' })).toBeVisible();
  await user.click(screen.getByRole('button', { name: '收起筛选' }));
  expect(within(history).getByText('设备重试 恢复成功')).toBeVisible();
  const url = new URL(
    (await screen.findByRole('link', { name: '导出 JSONL' })).getAttribute(
      'href',
    ) ?? '',
    'http://localhost',
  );
  expect(url.pathname).toBe('/api/export');
  expect(Object.fromEntries(url.searchParams)).toEqual({
    component: 'renderer',
    level: 'error',
    program: 'demo.exe',
    q: '设备',
    from: new Date('2026-11-11T00:00').toISOString(),
    until: new Date('2026-11-12T00:00').toISOString(),
  });
});

test('实时追加去重，暂停后保留历史与筛选', async () => {
  vi.useFakeTimers();
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: string) => {
      const url = new URL(input, 'http://localhost');
      return url.searchParams.get('after') === '9007199254740993'
        ? page([first, second])
        : url.searchParams.has('after')
          ? page([])
          : page([first]);
    }),
  );
  await act(async () => {
    render(<App />);
  });
  expect(screen.getByText('帧提交完成')).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: '筛选' }));
  fireEvent.change(screen.getByLabelText('组件'), {
    target: { value: 'renderer' },
  });
  fireEvent.click(screen.getByRole('button', { name: '收起筛选' }));
  await act(async () => {
    await vi.advanceTimersByTimeAsync(1000);
  });
  const region = screen.getByRole('region', { name: '事件历史' });
  expect(within(region).getAllByRole('button')).toHaveLength(2);
  expect(within(region).getByText('设备重试 恢复成功')).toBeInTheDocument();
  fireEvent.click(screen.getByRole('switch', { name: '实时跟随' }));
  await act(async () => {
    await vi.advanceTimersByTimeAsync(2000);
  });
  expect(screen.getByText('历史浏览')).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: '筛选' }));
  expect(screen.getByLabelText('组件')).toHaveValue('renderer');
  fireEvent.click(screen.getByRole('button', { name: '收起筛选' }));
  expect(within(region).getByText('帧提交完成')).toBeInTheDocument();
});

test('选择事件展示完整多行详情并复制原始 JSON', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: string) =>
      input.includes('/api/events/9007199254740994')
        ? response({
            ...second,
            message: {
              ...second.message,
              process: { pid: 72, exe_path: 'C:\\game.exe' },
            },
          })
        : page([second]),
    ),
  );
  const user = userEvent.setup();
  render(<App />);
  await user.click(
    await screen.findByRole('button', { name: '查看事件 9007199254740994' }),
  );
  expect(await screen.findByText('C:\\game.exe')).toBeInTheDocument();
  expect(screen.getByText('72')).toBeInTheDocument();
  const body = screen
    .getAllByText('设备重试 恢复成功')
    .find((element) => element.tagName === 'PRE');
  expect(body?.textContent).toBe('设备重试\n恢复成功');
  await user.click(screen.getByRole('tab', { name: '原始记录' }));
  await user.click(screen.getByRole('button', { name: '复制原始 JSON' }));
  expect(await navigator.clipboard.readText()).toBe(
    '{"text":"设备重试\\n恢复成功"}',
  );
  expect(await screen.findByText('已复制')).toBeInTheDocument();
});

test('切换条件后采用新查询结果并加载更早事件', async () => {
  let resolveOld: (response: Response) => void = () => {};
  vi.stubGlobal(
    'fetch',
    vi.fn((input: string) => {
      const url = new URL(input, 'http://localhost');
      if (!url.searchParams.has('q'))
        return new Promise<Response>((resolve) => {
          resolveOld = resolve;
        });
      if (url.searchParams.has('before'))
        return Promise.resolve(
          page([
            {
              id: '1',
              received_at: '2026-11-11T08:00:00Z',
              message: { type: 'status', text: '宿主已连接' },
              raw_message: '{"type":"status"}',
            },
          ]),
        );
      return Promise.resolve(page([second]));
    }),
  );
  const user = userEvent.setup();
  render(<App />);
  await user.click(screen.getByRole('button', { name: '筛选' }));
  await user.type(screen.getByLabelText('正文搜索'), '设备');
  await user.click(screen.getByRole('button', { name: '应用筛选' }));
  expect(await screen.findByText('设备重试 恢复成功')).toBeInTheDocument();
  await act(async () => {
    resolveOld(page([first]));
  });
  await user.click(screen.getByRole('button', { name: '加载更早' }));
  expect(await screen.findByText('宿主已连接')).toBeInTheDocument();
  const buttons = within(
    screen.getByRole('region', { name: '事件历史' }),
  ).getAllByRole('button');
  expect(buttons.map((button) => button.getAttribute('aria-label'))).toEqual([
    '查看事件 1',
    '查看事件 9007199254740994',
  ]);
});

test('程序列显示文件名、进程编号与系统事件', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () =>
      page([
        first,
        {
          ...second,
          id: '2',
          message: {
            ...second.message,
            process: { exe_path: '/opt/games/game.exe', pid: 9 },
          },
        },
        {
          ...second,
          id: '3',
          message: { ...second.message, process: { pid: 123 } },
        },
        { ...second, id: '4' },
      ]),
    ),
  );
  render(<App />);
  const region = screen.getByRole('region', { name: '事件历史' });
  expect(await within(region).findByText('demo.exe')).toHaveAttribute(
    'title',
    'C:\\demo.exe',
  );
  expect(within(region).getByText('game.exe')).toHaveAttribute(
    'title',
    '/opt/games/game.exe',
  );
  expect(within(region).getByText('进程 123')).toBeInTheDocument();
  expect(within(region).getByText('系统')).toBeInTheDocument();
});

test('窄屏查看详情并展开筛选，收起后继续浏览事件', async () => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({
      matches: true,
      addEventListener() {},
      removeEventListener() {},
    })),
  );
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: string) =>
      input.includes('/api/events/') ? response(first) : page([first]),
    ),
  );
  const user = userEvent.setup();
  render(<App />);
  await user.click(
    await screen.findByRole('button', { name: '查看事件 9007199254740993' }),
  );
  const detail = screen.getByRole('dialog', { name: '事件详情' });
  expect(await within(detail).findByText('C:\\demo.exe')).toBeInTheDocument();
  expect(within(detail).getByText('42')).toBeInTheDocument();
  await user.click(within(detail).getByRole('button', { name: '关闭' }));
  expect(
    await screen.findByRole('region', { name: '事件历史' }),
  ).toHaveTextContent('帧提交完成');
  await user.click(screen.getByRole('button', { name: '筛选' }));
  const filters = screen.getByRole('region', { name: '筛选事件' });
  expect(screen.getByRole('region', { name: '事件历史' })).toHaveTextContent(
    '帧提交完成',
  );
  await user.type(within(filters).getByLabelText('组件'), 'renderer');
  await user.click(screen.getByRole('button', { name: '收起筛选' }));
  expect(
    await screen.findByRole('region', { name: '事件历史' }),
  ).toHaveTextContent('demo.exe');
  await user.click(screen.getByRole('button', { name: '筛选' }));
  expect(screen.getByLabelText('组件')).toHaveValue('renderer');
  await user.click(screen.getByRole('button', { name: '应用筛选' }));
  expect(await screen.findByText('组件：renderer')).toBeInTheDocument();
  expect(screen.getByLabelText('组件')).toHaveValue('renderer');
  expect(
    await screen.findByRole('region', { name: '事件历史' }),
  ).toHaveTextContent('帧提交完成');
});

test('拖动与键盘调整列宽，表头和事件保持对齐并可恢复', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => page([first])),
  );
  vi.stubGlobal('PointerEvent', MouseEvent);
  const user = userEvent.setup();
  render(<App />);
  const row = await screen.findByRole('button', {
    name: '查看事件 9007199254740993',
  });
  const programHandle = screen.getByRole('separator', { name: '调整程序列宽' });
  const header = programHandle.parentElement?.parentElement;
  Object.defineProperties(programHandle, {
    setPointerCapture: { value: () => {} },
    hasPointerCapture: { value: () => true },
    releasePointerCapture: { value: () => {} },
  });
  fireEvent.pointerDown(programHandle, { clientX: 160, pointerId: 1 });
  fireEvent.pointerMove(programHandle, { clientX: 220, pointerId: 1 });
  fireEvent.pointerUp(programHandle, { clientX: 220, pointerId: 1 });
  expect(programHandle).toHaveAttribute('aria-valuenow', '220');
  expect(header).toHaveStyle({
    gridTemplateColumns: '220px 170px 130px 80px minmax(180px, 1fr)',
  });
  expect(row).toHaveStyle({
    gridTemplateColumns: '220px 170px 130px 80px minmax(180px, 1fr)',
  });
  programHandle.focus();
  await user.keyboard('{ArrowLeft}');
  expect(row).toHaveStyle({
    gridTemplateColumns: '204px 170px 130px 80px minmax(180px, 1fr)',
  });
  const bodyHandle = screen.getByRole('separator', { name: '调整正文列宽' });
  bodyHandle.focus();
  await user.keyboard('{ArrowRight}');
  expect(bodyHandle).toHaveAttribute('aria-valuenow', '196');
  expect(row).toHaveStyle({
    gridTemplateColumns: '204px 170px 130px 80px minmax(196px, 1fr)',
  });
  await user.click(screen.getByRole('button', { name: '恢复列宽' }));
  expect(programHandle).toHaveAttribute('aria-valuenow', '160');
  expect(header).toHaveStyle({
    gridTemplateColumns: '160px 170px 130px 80px minmax(180px, 1fr)',
  });
  expect(row).toHaveStyle({
    gridTemplateColumns: '160px 170px 130px 80px minmax(180px, 1fr)',
  });
});

test('连接失败保留已显示的事件，后续成功恢复跟随', async () => {
  vi.useFakeTimers();
  let count = 0;
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => {
      count += 1;
      if (count === 2)
        return new Response('{"error":"日志服务暂时离线"}', { status: 503 });
      return count === 1 ? page([first]) : page([second]);
    }),
  );
  await act(async () => {
    render(<App />);
  });
  await act(async () => {
    await vi.advanceTimersByTimeAsync(1000);
  });
  expect(screen.getByText('日志服务暂时离线')).toBeInTheDocument();
  expect(screen.getByText('帧提交完成')).toBeInTheDocument();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(1000);
  });
  expect(screen.getByText('设备重试 恢复成功')).toBeInTheDocument();
  expect(screen.getByText('2 条事件')).toBeInTheDocument();
  expect(screen.getAllByText('实时跟随')).toHaveLength(2);
});

test('系统导航切换后保留日志筛选、选择与跟随状态', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: string) => {
      if (input === '/api/connection')
        return response({
          transport: 'VirtIO Serial',
          channel: 'org.venera.debug.0',
          socket_path: '/run/debug.sock',
          connected: false,
          last_received_at: null,
        });
      if (input === '/api/system-info')
        return response({
          major: 1,
          type: 'system.info.result',
          request_id: '1',
          ok: true,
          collected_at: '2026-11-11T10:00:00Z',
          session_id: 2,
          sections: [],
        });
      return input.includes('/api/events/') ? response(first) : page([first]);
    }),
  );
  const user = userEvent.setup();
  render(<App />);
  await user.click(
    await screen.findByRole('button', { name: '查看事件 9007199254740993' }),
  );
  await user.click(screen.getByRole('switch', { name: '实时跟随' }));
  await user.click(screen.getByRole('button', { name: '筛选' }));
  await user.type(screen.getByLabelText('组件'), 'renderer');
  await user.click(screen.getByRole('tab', { name: '系统信息' }));
  expect(await screen.findByText('org.venera.debug.0')).toBeVisible();
  expect(screen.getByText('等待连接')).toBeVisible();
  await user.click(screen.getByRole('tab', { name: '宿主日志' }));
  expect(screen.getByLabelText('组件')).toHaveValue('renderer');
  expect(screen.getByText('历史浏览')).toBeVisible();
  expect(screen.getByText('42')).toBeVisible();
  expect(
    screen.getByRole('button', { name: '查看事件 9007199254740993' }),
  ).toHaveClass('selected');
});

test('拖动和键盘调整详情宽度，并随工作区变化收敛', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => page([first])),
  );
  vi.stubGlobal('PointerEvent', MouseEvent);
  render(<App />);
  await screen.findByText('demo.exe');
  const handle = screen.getByRole('separator', { name: '调整详情宽度' });
  Object.defineProperties(handle, {
    setPointerCapture: { value: () => {} },
    hasPointerCapture: { value: () => true },
    releasePointerCapture: { value: () => {} },
  });
  fireEvent.keyDown(handle, { key: 'ArrowLeft' });
  expect(handle).toHaveAttribute('aria-valuenow', '356');
  fireEvent.pointerDown(handle, { clientX: 700, pointerId: 1 });
  fireEvent.pointerMove(handle, { clientX: 600, pointerId: 1 });
  fireEvent.pointerUp(handle, { pointerId: 1 });
  expect(handle).toHaveAttribute('aria-valuenow', '456');
  expect(handle.parentElement).toHaveStyle({
    gridTemplateColumns: 'minmax(300px, 1fr) 12px 456px',
  });
  fireEvent.pointerDown(handle, { clientX: 600, pointerId: 1 });
  fireEvent.pointerMove(handle, { clientX: 2000, pointerId: 1 });
  fireEvent.pointerUp(handle, { pointerId: 1 });
  expect(handle).toHaveAttribute('aria-valuenow', '260');
  fireEvent.pointerDown(handle, { clientX: 600, pointerId: 1 });
  fireEvent.pointerMove(handle, { clientX: -2000, pointerId: 1 });
  fireEvent.pointerUp(handle, { pointerId: 1 });
  expect(handle).toHaveAttribute('aria-valuenow', '600');
  vi.spyOn(
    handle.parentElement as HTMLElement,
    'getBoundingClientRect',
  ).mockReturnValue({ width: 800 } as DOMRect);
  fireEvent(window, new Event('resize'));
  expect(handle).toHaveAttribute('aria-valuenow', '480');
});
