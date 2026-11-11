import {
  Badge,
  Button,
  Card,
  MessageBar,
  MessageBarBody,
  Spinner,
} from '@fluentui/react-components';
import { ArrowSyncRegular } from '@fluentui/react-icons';
import { useEffect, useRef, useState } from 'react';

type Connection = {
  transport: string;
  channel: string;
  socket_path: string;
  connected: boolean;
  last_received_at: string | null;
  protocol_version?: string | null;
  software_version?: string | null;
};
type Report = {
  major: number;
  type: string;
  request_id: string;
  ok: boolean;
  collected_at: string;
  session_id: number | null;
  sections: {
    title: string;
    rows: { key: string; value: string; bad: boolean; warning: string }[];
  }[];
  error?: { code: string; message: string };
};

async function load<T>(url: string, signal: AbortSignal): Promise<T> {
  const response = await fetch(url, { signal });
  const body = await response.json();
  if (!response.ok || body.ok === false)
    throw new Error(
      (typeof body.error === 'string' ? body.error : body.error?.message) ||
        '读取系统信息失败',
    );
  return body;
}

export function SystemInfo() {
  const [connection, setConnection] = useState<Connection | null>(null);
  const [report, setReport] = useState<Report | null>(null);
  const [connectionError, setConnectionError] = useState('');
  const [reportError, setReportError] = useState('');
  const [busy, setBusy] = useState(false);
  const active = useRef<AbortController | null>(null);

  async function refresh() {
    active.current?.abort();
    const controller = new AbortController();
    active.current = controller;
    setBusy(true);
    setConnectionError('');
    setReportError('');
    await load<Connection>('/api/connection', controller.signal)
      .then((value) => {
        if (controller.signal.aborted) return;
        setConnection(value);
        setConnectionError('');
      })
      .catch((reason: Error) => {
        if (!controller.signal.aborted) {
          setConnection(null);
          setConnectionError(reason.message);
        }
      });
    if (controller.signal.aborted) return;
    await load<Report>('/api/system-info', controller.signal)
      .then((value) => {
        if (controller.signal.aborted) return;
        if (!value.ok)
          throw new Error(value.error?.message || '系统信息采集失败');
        setReport(value);
        setReportError('');
      })
      .catch((reason: Error) => {
        if (!controller.signal.aborted) setReportError(reason.message);
      });
    if (!controller.signal.aborted) setBusy(false);
  }

  useEffect(() => {
    void refresh();
    return () => active.current?.abort();
  }, []);

  return (
    <>
      <header>
        <div>
          <h1>系统信息</h1>
          <p>查看调试通道与客户机系统报告。</p>
        </div>
        <Button
          icon={<ArrowSyncRegular />}
          disabled={busy}
          onClick={() => void refresh()}
        >
          {busy ? '正在刷新' : '刷新'}
        </Button>
      </header>
      <section className="system-page" aria-label="系统报告">
        <Card className="system-card">
          <h2>连接信息</h2>
          <dl>
            <dt>浏览器地址</dt>
            <dd>{window.location.href}</dd>
          </dl>
          {connectionError && (
            <MessageBar intent="error">
              <MessageBarBody>{connectionError}</MessageBarBody>
            </MessageBar>
          )}
          {connection && (
            <>
              <Badge
                className="connection-status"
                color={connection.connected ? 'success' : 'warning'}
              >
                {connection.connected ? '已连接' : '等待连接'}
              </Badge>
              <dl className="connection-grid">
                <div>
                  <dt>通道方式</dt>
                  <dd>{connection.transport}</dd>
                </div>
                <div>
                  <dt>通道名称</dt>
                  <dd>{connection.channel}</dd>
                </div>
                <div>
                  <dt>调试通道协议版本</dt>
                  <dd>{connection.protocol_version ?? '—'}</dd>
                </div>
                <div>
                  <dt>调试服务版本</dt>
                  <dd>{connection.software_version ?? '—'}</dd>
                </div>
                <div>
                  <dt>接收端 Socket</dt>
                  <dd>{connection.socket_path}</dd>
                </div>
                <div>
                  <dt>最后收到时间</dt>
                  <dd>{connection.last_received_at ?? '等待首条消息'}</dd>
                </div>
              </dl>
            </>
          )}
        </Card>
        {busy && (
          <div
            className={`system-collection ${report ? '' : 'system-collection-initial'}`}
          >
            <Spinner
              size="small"
              role="status"
              aria-live="polite"
              label="正在采集系统信息…"
            />
          </div>
        )}
        {reportError && (
          <MessageBar intent="error">
            <MessageBarBody>{reportError}</MessageBarBody>
          </MessageBar>
        )}
        {report && (
          <>
            <p className="report-context">
              {reportError ? '最后采集时间' : '采集时间'}：
              <time>{report.collected_at}</time> · 会话：
              {report.session_id ?? '系统'}
            </p>
            <div className="system-report-grid">
              {report.sections.map((section, index) => (
                <Card className="system-card" key={`${section.title}-${index}`}>
                  <h2>{section.title}</h2>
                  <dl>
                    {section.rows.map((row, rowIndex) => (
                      <div
                        className={`system-row ${row.bad ? 'system-row-bad' : ''}`}
                        key={`${row.key}-${rowIndex}`}
                      >
                        <dt>
                          {row.key}
                          {row.bad && <Badge color="danger">异常</Badge>}
                        </dt>
                        <dd>{row.value}</dd>
                        {row.warning && (
                          <dd className="system-warning">{row.warning}</dd>
                        )}
                      </div>
                    ))}
                  </dl>
                </Card>
              ))}
            </div>
          </>
        )}
      </section>
    </>
  );
}
