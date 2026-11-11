import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, test, vi } from 'vitest';
import { EventDetails, type LogEvent } from './EventDetails';

const event: LogEvent = {
  id: '9007199254740993',
  received_at: '2026-11-11T09:00:00Z',
  message: {
    type: 'log',
    component: 'renderer',
    level: 'info',
    text: '设备重试\n恢复成功',
    process: { pid: 42, exe_path: 'C:\\demo.exe' },
    details: { queue: 'graphics', frame: 17 },
  },
  raw_message: '{"tick":18446744073709551615,"text":"恢复成功"}',
};

test('选择详情视图查看概览与完整嵌套元数据', async () => {
  const user = userEvent.setup();
  render(<EventDetails selected={event} />);
  const overview = screen.getByRole('tabpanel', { name: '概览' });
  for (const value of [
    '9007199254740993',
    '2026-11-11T09:00:00Z',
    'log',
    'renderer',
    'info',
    'C:\\demo.exe',
    '42',
  ]) {
    expect(within(overview).getByText(value)).toBeVisible();
  }
  expect(within(overview).getByText('设备重试 恢复成功').textContent).toBe(
    '设备重试\n恢复成功',
  );
  await user.click(screen.getByRole('tab', { name: '元数据' }));
  expect(screen.getByRole('tab', { name: '元数据' })).toHaveAttribute(
    'aria-selected',
    'true',
  );
  expect(screen.getByRole('tabpanel', { name: '元数据' }).textContent).toBe(
    '{\n  "type": "log",\n  "component": "renderer",\n  "level": "info",\n  "text": "设备重试\\n恢复成功",\n  "process": {\n    "pid": 42,\n    "exe_path": "C:\\\\demo.exe"\n  },\n  "details": {\n    "queue": "graphics",\n    "frame": 17\n  }\n}',
  );
});

test('展示长正文与长原始记录的末尾并复制完整原文', async () => {
  const user = userEvent.setup();
  render(
    <EventDetails
      selected={{
        ...event,
        message: { ...event.message, text: `${'正文\n'.repeat(2000)}正文末尾` },
        raw_message: `{"tick":18446744073709551615,"text":"${'记录'.repeat(10000)}原文末尾"}`,
      }}
    />,
  );
  expect(screen.getByRole('tabpanel', { name: '概览' })).toHaveTextContent(
    /正文末尾$/,
  );
  await user.click(screen.getByRole('tab', { name: '原始记录' }));
  expect(screen.getByRole('tabpanel', { name: '原始记录' })).toHaveTextContent(
    /原文末尾"}$/,
  );
  await user.click(screen.getByRole('button', { name: '复制原始 JSON' }));
  expect(await navigator.clipboard.readText()).toBe(
    `{"tick":18446744073709551615,"text":"${'记录'.repeat(10000)}原文末尾"}`,
  );
  expect(screen.getByRole('status')).toHaveTextContent('已复制');
});

test('切换事件保留视图并更新复制结果', async () => {
  const user = userEvent.setup();
  const { rerender } = render(<EventDetails selected={event} />);
  await user.click(screen.getByRole('tab', { name: '原始记录' }));
  await user.click(screen.getByRole('button', { name: '复制原始 JSON' }));
  expect(screen.getByRole('status')).toHaveTextContent('已复制');
  rerender(
    <EventDetails
      selected={{ ...event, id: '2', raw_message: '{"text":"下一条事件"}' }}
    />,
  );
  expect(screen.getByRole('tabpanel', { name: '原始记录' }).textContent).toBe(
    '复制原始 JSON{"text":"下一条事件"}',
  );
  await user.click(screen.getByRole('button', { name: '复制原始 JSON' }));
  expect(await navigator.clipboard.readText()).toBe('{"text":"下一条事件"}');
  expect(screen.getByRole('status')).toHaveTextContent('已复制');
});

test('根据详情加载状态展示错误与选取引导', () => {
  const { rerender } = render(<EventDetails />);
  expect(
    screen.getByText('选择一条事件，查看完整正文、元数据与原始记录。'),
  ).toBeVisible();
  rerender(<EventDetails selected={null} detailError="事件读取失败，请重试" />);
  expect(screen.getByText('事件读取失败，请重试')).toBeVisible();
  rerender(<EventDetails selected={event} />);
  expect(screen.queryByText('事件读取失败，请重试')).not.toBeInTheDocument();
  expect(screen.getByRole('tabpanel', { name: '概览' })).toHaveTextContent(
    '恢复成功',
  );
});

test('复制遇到异常后展示重试结果', async () => {
  const user = userEvent.setup();
  const write = vi.spyOn(navigator.clipboard, 'writeText');
  write.mockRejectedValueOnce(new Error('clipboard unavailable'));
  render(<EventDetails selected={event} />);
  await user.click(screen.getByRole('tab', { name: '原始记录' }));
  await user.click(screen.getByRole('button', { name: '复制原始 JSON' }));
  expect(screen.getByRole('status')).toHaveTextContent('复制失败，请重试');
  await user.click(screen.getByRole('button', { name: '复制原始 JSON' }));
  expect(await navigator.clipboard.readText()).toBe(
    '{"tick":18446744073709551615,"text":"恢复成功"}',
  );
  expect(screen.getByRole('status')).toHaveTextContent('已复制');
  write.mockRestore();
});
