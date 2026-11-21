import { act, fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, test, vi } from 'vitest';
import { App } from './App';
import { en, type MessageKey, zhCN } from './catalog';
import {
  createI18n,
  enumLabel,
  formatDate,
  i18n,
  languageStorageKey,
  resolveLocale,
  savedPreference,
  setLanguagePreference,
} from './i18n';

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function json(body: unknown) {
  return new Response(JSON.stringify(body));
}
const event = {
  id: '2',
  received_at: '2026-11-11T09:00:00Z',
  message: {
    type: 'log',
    level: 'warning',
    text: 'Original diagnostic',
    component: 'venus',
  },
  raw_message: '{"type":"log","text":"Original diagnostic"}',
};

test('language selection updates the workspace while retaining filters, history and record view', async () => {
  await i18n.changeLanguage('en');
  const fetcher = vi.fn(async (input: string) => {
    if (input === '/api/events/2') return json(event);
    return json({
      events: input.includes('before=') ? [{ ...event, id: '1' }] : [event],
      oldest_id: '1',
      latest_id: '2',
    });
  });
  vi.stubGlobal('fetch', fetcher);
  const user = userEvent.setup();
  render(<App />);
  expect(
    await screen.findByRole('button', { name: 'View event 2' }),
  ).toBeVisible();
  await user.click(screen.getByRole('switch', { name: 'Follow live' }));
  await user.click(screen.getByRole('button', { name: 'Filters' }));
  await user.type(screen.getByLabelText('Message search'), 'Original');
  await user.click(screen.getByRole('button', { name: 'Apply filters' }));
  await user.click(await screen.findByRole('button', { name: 'View event 2' }));
  await user.click(screen.getByRole('tab', { name: 'Raw record' }));
  await user.click(screen.getByRole('button', { name: 'Load earlier events' }));
  expect(
    await screen.findByRole('button', { name: 'View event 1' }),
  ).toBeVisible();
  expect(screen.getByText('2 events')).toBeVisible();
  expect(
    screen.getByRole('tabpanel', { name: 'Raw record' }),
  ).toHaveTextContent(event.raw_message);
  const requests = fetcher.mock.calls.length;
  await user.selectOptions(
    screen.getByRole('combobox', { name: 'Language' }),
    'zh-CN',
  );
  expect(screen.getByRole('heading', { name: '日志工作台' })).toBeVisible();
  expect(screen.getByLabelText('正文搜索')).toHaveValue('Original');
  expect(screen.getByText('正文搜索：Original')).toBeVisible();
  expect(screen.getByText('历史浏览')).toBeVisible();
  expect(screen.getByText('2 条事件')).toBeVisible();
  expect(screen.getByRole('button', { name: '查看事件 2' })).toHaveClass(
    'selected',
  );
  expect(screen.getByRole('tabpanel', { name: '原始记录' })).toHaveTextContent(
    event.raw_message,
  );
  expect(screen.getByRole('button', { name: '查看事件 1' })).toHaveTextContent(
    '警告',
  );
  expect(fetcher.mock.calls.length).toBe(requests);
  expect(document.documentElement.lang).toBe('zh-CN');
  expect(document.title).toBe('Venera · 日志工作台');
  await user.selectOptions(
    screen.getByRole('combobox', { name: '语言' }),
    'en',
  );
  expect(screen.getByRole('heading', { name: 'Log workbench' })).toBeVisible();
  expect(
    screen.getByRole('tabpanel', { name: 'Raw record' }),
  ).toHaveTextContent(event.raw_message);
  expect(document.documentElement.lang).toBe('en');
});

test('explicit selection persists across a fresh application initialization', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => json({ events: [], oldest_id: null, latest_id: null })),
  );
  const user = userEvent.setup();
  const first = render(<App />);
  await user.selectOptions(
    screen.getByRole('combobox', { name: '语言' }),
    'en',
  );
  expect(localStorage.getItem(languageStorageKey)).toBe('en');
  first.unmount();
  const reloaded = createI18n();
  expect(reloaded.t('workbench')).toBe('Log workbench');
  await i18n.changeLanguage(reloaded.resolvedLanguage);
  render(<App />);
  expect(screen.getByRole('combobox', { name: 'Language' })).toHaveValue('en');
  expect(screen.getByRole('heading', { name: 'Log workbench' })).toBeVisible();
  await user.selectOptions(
    screen.getByRole('combobox', { name: 'Language' }),
    'zh-CN',
  );
  expect(createI18n().t('workbench')).toBe('日志工作台');
});

test.each([
  [['fr-FR', 'zh-Hans-CN', 'en-US'], 'zh-CN'],
  [['en-GB', 'zh-CN'], 'en'],
  [['zh-SG', 'en'], 'zh-CN'],
  [['zh-TW', 'en'], 'en'],
  [['de-DE', 'ja-JP'], 'en'],
  [[], 'en'],
] as const)(
  'system language preferences %j resolve to %s',
  (languages, expected) => {
    expect(resolveLocale(languages)).toBe(expected);
  },
);

test('system preference follows browser language changes and resets explicit storage', async () => {
  const languages = vi
    .spyOn(navigator, 'languages', 'get')
    .mockReturnValue(['fr-FR', 'zh-CN']);
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => json({ events: [], oldest_id: null, latest_id: null })),
  );
  await setLanguagePreference('en');
  const user = userEvent.setup();
  render(<App />);
  await user.selectOptions(
    screen.getByRole('combobox', { name: 'Language' }),
    'system',
  );
  expect(screen.getByRole('heading', { name: '日志工作台' })).toBeVisible();
  expect(savedPreference()).toBe('system');
  expect(localStorage.getItem(languageStorageKey)).toBeNull();
  languages.mockReturnValue(['fr-FR']);
  await act(async () => fireEvent(window, new Event('languagechange')));
  expect(screen.getByRole('heading', { name: 'Log workbench' })).toBeVisible();
  expect(
    screen.getByRole('option', { name: 'System default' }),
  ).toBeInTheDocument();
  expect(createI18n().t('workbench')).toBe('Log workbench');
  languages.mockRestore();
});

test('language changes preserve an ongoing system collection and display its returned report', async () => {
  await i18n.changeLanguage('en');
  let finish: (response: Response) => void = () => {};
  const pending = new Promise<Response>((resolve) => {
    finish = resolve;
  });
  const fetcher = vi.fn(async (input: string) => {
    if (input === '/api/system-info') return pending;
    if (input === '/api/connection')
      return json({
        transport: 'VirtIO Serial',
        channel: 'graphics.venera.debug.0',
        socket_path: '/run/debug.sock',
        connected: true,
        last_received_at: null,
      });
    return json({ events: [event], oldest_id: '2', latest_id: '2' });
  });
  vi.stubGlobal('fetch', fetcher);
  const user = userEvent.setup();
  render(<App />);
  await user.click(screen.getByRole('tab', { name: 'System information' }));
  expect(await screen.findByText('graphics.venera.debug.0')).toBeVisible();
  expect(
    screen.getByRole('status', { name: 'Collecting system information…' }),
  ).toBeVisible();
  await user.selectOptions(
    screen.getByRole('combobox', { name: 'Language' }),
    'zh-CN',
  );
  expect(
    screen.getByRole('status', { name: '正在采集系统信息…' }),
  ).toBeVisible();
  expect(screen.getByRole('button', { name: '正在刷新' })).toBeDisabled();
  await act(async () =>
    finish(
      json({
        ok: true,
        collected_at: '2026-11-11T09:00:00Z',
        session_id: 4,
        sections: [
          {
            title: 'Graphics',
            rows: [
              { key: 'Adapter', value: 'Venera GPU', bad: false, warning: '' },
            ],
          },
        ],
      }),
    ),
  );
  expect(screen.getByRole('heading', { name: 'Graphics' })).toBeVisible();
  expect(screen.getByText('Adapter')).toBeVisible();
  expect(screen.getByText('Venera GPU')).toBeVisible();
  expect(
    screen.getByText('采集时间：2026年11月11日 09:00:00 · 会话：4'),
  ).toBeVisible();
  expect(screen.getByRole('button', { name: '刷新' })).toBeEnabled();
  expect(
    fetcher.mock.calls.filter(([url]) => url === '/api/system-info'),
  ).toHaveLength(1);
});

test('catalogs have matching keys and interpolation arguments with English fallback', () => {
  const instance = createI18n('zh-CN');
  expect(Object.keys(zhCN).sort()).toEqual(Object.keys(en).sort());
  const placeholders = (text: string) =>
    [...text.matchAll(/{{\s*([^}]+)\s*}}/g)]
      .map((match) => match[1].trim())
      .sort();
  for (const key of Object.keys(en) as MessageKey[]) {
    expect(instance.exists(key), key).toBe(true);
    expect(placeholders(zhCN[key]), key).toEqual(placeholders(en[key]));
    expect(zhCN[key], key).toMatch(/\S/);
  }
  expect(instance.t('viewEvent', { id: '42' })).toBe('查看事件 42');
  expect(instance.t('resizeColumn', { label: '程序' })).toBe('调整程序列宽');
  instance.removeResourceBundle('zh-CN', 'translation');
  expect(instance.t('workbench')).toBe('Log workbench');
  expect(instance.t('viewEvent', { id: '42' })).toBe('View event 42');
});

test.each([
  ['en', 0, '0 events'],
  ['en', 1, '1 event'],
  ['en', 2500, '2,500 events'],
  ['zh-CN', 0, '0 条事件'],
  ['zh-CN', 1, '1 条事件'],
  ['zh-CN', 2500, '2,500 条事件'],
] as const)('%s formats an event count of %s', (locale, count, expected) => {
  expect(createI18n(locale).t('eventCount', { count })).toBe(expected);
});

test('dates, numbers and known event labels follow the active language', async () => {
  await i18n.changeLanguage('en');
  expect(formatDate('2026-11-11T09:00:00Z')).toBe('Nov 11, 2026, 9:00:00 AM');
  expect(i18n.t('eventCount', { count: 12345 })).toBe('12,345 events');
  expect(enumLabel('warning')).toBe('Warning');
  await i18n.changeLanguage('zh-CN');
  expect(formatDate('2026-11-11T09:00:00Z')).toBe('2026年11月11日 09:00:00');
  expect(i18n.t('eventCount', { count: 12345 })).toBe('12,345 条事件');
  expect(enumLabel('warning')).toBe('警告');
  expect(enumLabel('system.info.result')).toBe('系统信息结果');
  expect(enumLabel('custom.event')).toBe('custom.event');
});

test('empty states and accessibility labels change immediately', async () => {
  await i18n.changeLanguage('en');
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => json({ events: [], oldest_id: null, latest_id: null })),
  );
  const user = userEvent.setup();
  render(<App />);
  expect(
    await screen.findByText('No events match the current filters.'),
  ).toBeVisible();
  expect(
    within(screen.getByRole('region', { name: 'Event details' })).getByText(
      'Select an event to view its full message, metadata and raw record.',
    ),
  ).toBeVisible();
  expect(
    screen.getByRole('separator', { name: 'Resize Program column' }),
  ).toBeVisible();
  await user.selectOptions(
    screen.getByRole('combobox', { name: 'Language' }),
    'zh-CN',
  );
  expect(screen.getByText('当前条件下暂无事件')).toBeVisible();
  expect(screen.getByRole('separator', { name: '调整程序列宽' })).toBeVisible();
});

test('request error messages follow language selection and retain refresh actions', async () => {
  await i18n.changeLanguage('en');
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response('{}', { status: 503 })),
  );
  const user = userEvent.setup();
  render(<App />);
  expect(
    await screen.findByText('Unable to load events. Please try again.'),
  ).toBeVisible();
  await user.selectOptions(
    screen.getByRole('combobox', { name: 'Language' }),
    'zh-CN',
  );
  expect(screen.getByText('读取日志失败，请重试。')).toBeVisible();
  await user.click(screen.getByRole('tab', { name: '系统信息' }));
  expect(await screen.findAllByText('读取系统信息失败，请重试。')).toHaveLength(
    2,
  );
  await user.selectOptions(
    screen.getByRole('combobox', { name: '语言' }),
    'en',
  );
  expect(
    screen.getAllByText('Unable to load system information. Please try again.'),
  ).toHaveLength(2);
  expect(screen.getByRole('button', { name: 'Refresh' })).toBeEnabled();
});
