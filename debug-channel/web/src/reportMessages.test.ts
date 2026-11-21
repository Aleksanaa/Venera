import { expect, test } from 'vitest';
import { reportMessage } from './reportMessages';

test('renders whole messages with positional raw parameters and diagnostic suffixes', () => {
  expect(
    reportMessage(
      'fallback',
      { id: 'report.gpuFailed', args: ['error -3'] },
      'en',
    ),
  ).toBe('GPU query failed (error -3)');
  expect(
    reportMessage(
      'fallback',
      { id: 'report.gpuFailed', args: ['error -3'] },
      'zh-CN',
    ),
  ).toBe('GPU 查询失败（error -3）');
  expect(reportMessage('Memory', { id: 'report.memory' }, 'zh-CN')).toBe(
    '内存',
  );
  expect(
    reportMessage(
      'fallback',
      { id: 'report.memoryValue', args: ['8 GB', '7 GB'] },
      'zh-CN',
    ),
  ).toBe('8 GB（可用 7 GB）');
  expect(
    reportMessage(
      'fallback',
      { id: 'report.displayValue', args: ['1920', '1080', '60', '150'] },
      'zh-CN',
    ),
  ).toBe('1920 × 1080，60 Hz，缩放 150%');
  expect(
    reportMessage(
      'fallback',
      {
        id: 'report.apiFailed',
        args: ['设备 {1} $& 🚀'],
        diagnostic: '\nC:\\GPU\n原始诊断',
      },
      'zh-CN',
    ),
  ).toBe('失败（设备 {1} $& 🚀）\nC:\\GPU\n原始诊断');
  expect(
    reportMessage(
      'fallback',
      { id: 'report.memoryValue', args: ['8 GB', '7 GB'] },
      'en',
    ),
  ).toBe('8 GB (7 GB usable)');
});

test('uses producer text for earlier and unfamiliar messages and incomplete parameters', () => {
  expect(reportMessage('原始值 🚀\nC:\\GPU')).toBe('原始值 🚀\nC:\\GPU');
  expect(reportMessage('Future value', { id: 'report.future' }, 'zh-CN')).toBe(
    'Future value',
  );
  expect(
    reportMessage(
      'Original memory',
      { id: 'report.memoryValue', args: ['8 GB'] },
      'zh-CN',
    ),
  ).toBe('Original memory');
  expect(reportMessage('Raw hardware', { id: 'toString' }, 'zh-CN')).toBe(
    'Raw hardware',
  );
});
