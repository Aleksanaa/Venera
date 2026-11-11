import {
  Button,
  MessageBar,
  MessageBarBody,
  Tab,
  TabList,
} from '@fluentui/react-components';
import { ClipboardRegular } from '@fluentui/react-icons';
import { useEffect, useId, useRef, useState } from 'react';
import './EventDetails.css';

export type LogEvent = {
  id: string;
  received_at: string;
  message: {
    type?: string;
    component?: string;
    level?: string;
    text?: string;
    process?: { pid?: number; start_time_filetime?: string; exe_path?: string };
    [key: string]: unknown;
  };
  raw_message: string;
};

export type EventDetailsProps = {
  selected?: LogEvent | null;
  detailError?: string;
};

type DetailTab = 'overview' | 'metadata' | 'raw';
const tabLabels = {
  overview: '概览',
  metadata: '元数据',
  raw: '原始记录',
};

export function EventDetails({ selected, detailError }: EventDetailsProps) {
  const [tab, setTab] = useState<DetailTab>('overview');
  const [copyState, setCopyState] = useState('');
  const selection = useRef(selected);
  selection.current = selected;
  const id = useId();

  useEffect(() => {
    setCopyState('');
  }, [selected]);

  async function copyRaw() {
    if (!selected) return;
    const event = selected;
    try {
      await navigator.clipboard.writeText(event.raw_message);
      if (selection.current === event) setCopyState('已复制');
    } catch {
      if (selection.current === event) setCopyState('复制失败，请重试');
    }
  }

  return (
    <section className="event-details" aria-label="事件详情">
      <h2 className="event-details-title">事件详情</h2>
      {detailError && (
        <MessageBar intent="error">
          <MessageBarBody>{detailError}</MessageBarBody>
        </MessageBar>
      )}
      {selected ? (
        <>
          <TabList
            className="event-details-tabs"
            aria-label="详情视图"
            selectedValue={tab}
            onTabSelect={(_, data) => setTab(data.value as DetailTab)}
          >
            {(Object.keys(tabLabels) as DetailTab[]).map((value) => (
              <Tab
                key={value}
                value={value}
                id={`${id}-${value}`}
                aria-controls={`${id}-panel`}
              >
                {tabLabels[value]}
              </Tab>
            ))}
          </TabList>
          <div
            id={`${id}-panel`}
            className="event-details-content"
            role="tabpanel"
            aria-labelledby={`${id}-${tab}`}
            tabIndex={0}
          >
            {tab === 'overview' && (
              <>
                <dl>
                  <dt>事件编号</dt>
                  <dd>{selected.id}</dd>
                  <dt>接收时间</dt>
                  <dd>{selected.received_at}</dd>
                  <dt>类型</dt>
                  <dd>{selected.message.type ?? '事件'}</dd>
                  <dt>组件</dt>
                  <dd>{selected.message.component ?? '—'}</dd>
                  <dt>级别</dt>
                  <dd>{selected.message.level ?? '—'}</dd>
                  <dt>程序</dt>
                  <dd>{selected.message.process?.exe_path ?? '—'}</dd>
                  <dt>进程编号</dt>
                  <dd>{selected.message.process?.pid ?? '—'}</dd>
                </dl>
                <h3>正文</h3>
                <pre>
                  {selected.message.text ?? JSON.stringify(selected.message)}
                </pre>
              </>
            )}
            {tab === 'metadata' && (
              <pre>{JSON.stringify(selected.message, null, 2)}</pre>
            )}
            {tab === 'raw' && (
              <>
                <div className="event-details-copy">
                  <Button
                    icon={<ClipboardRegular />}
                    onClick={() => void copyRaw()}
                  >
                    复制原始 JSON
                  </Button>
                  {copyState && <span role="status">{copyState}</span>}
                </div>
                <pre>{selected.raw_message}</pre>
              </>
            )}
          </div>
        </>
      ) : (
        <p className="event-details-empty">
          选择一条事件，查看完整正文、元数据与原始记录。
        </p>
      )}
    </section>
  );
}
