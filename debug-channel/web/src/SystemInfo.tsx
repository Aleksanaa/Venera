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
import { useTranslation } from 'react-i18next';
import { formatDate } from './i18n';
import { type LocalizedMessage, reportMessage } from './reportMessages';

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
    title_message?: LocalizedMessage;
    rows: {
      key: string;
      value: string;
      bad: boolean;
      warning: string;
      key_message?: LocalizedMessage;
      value_message?: LocalizedMessage;
      warning_message?: LocalizedMessage;
    }[];
  }[];
  error?: { code: string; message: string };
};

async function load<T>(url: string, signal: AbortSignal): Promise<T> {
  const response = await fetch(url, { signal });
  const body = await response.json();
  if (!response.ok || body.ok === false)
    throw new Error(
      (typeof body.error === 'string' ? body.error : body.error?.message) ||
        'readSystemError',
    );
  return body;
}

export function SystemInfo() {
  const { t, i18n } = useTranslation();
  const message = (fallback: string, metadata?: LocalizedMessage) =>
    reportMessage(fallback, metadata, i18n.resolvedLanguage);
  function errorText(error: string) {
    return error === 'readSystemError' || error === 'collectError'
      ? t(error)
      : error;
  }
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
        if (!value.ok) throw new Error(value.error?.message || 'collectError');
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
          <h1>{t('systemInfo')}</h1>
          <p>{t('systemHint')}</p>
        </div>
        <Button
          icon={<ArrowSyncRegular />}
          disabled={busy}
          onClick={() => void refresh()}
        >
          {busy ? t('refreshing') : t('refresh')}
        </Button>
      </header>
      <section className="system-page" aria-label={t('systemReport')}>
        <Card className="system-card">
          <h2>{t('connectionInfo')}</h2>
          <dl>
            <dt>{t('browserAddress')}</dt>
            <dd>{window.location.href}</dd>
          </dl>
          {connectionError && (
            <MessageBar intent="error">
              <MessageBarBody>{errorText(connectionError)}</MessageBarBody>
            </MessageBar>
          )}
          {connection && (
            <>
              <Badge
                className="connection-status"
                color={connection.connected ? 'success' : 'warning'}
              >
                {connection.connected ? t('connected') : t('waitingConnection')}
              </Badge>
              <dl className="connection-grid">
                <div>
                  <dt>{t('transport')}</dt>
                  <dd>{connection.transport}</dd>
                </div>
                <div>
                  <dt>{t('channel')}</dt>
                  <dd>{connection.channel}</dd>
                </div>
                <div>
                  <dt>{t('protocolVersion')}</dt>
                  <dd>{connection.protocol_version ?? '—'}</dd>
                </div>
                <div>
                  <dt>{t('softwareVersion')}</dt>
                  <dd>{connection.software_version ?? '—'}</dd>
                </div>
                <div>
                  <dt>{t('socket')}</dt>
                  <dd>{connection.socket_path}</dd>
                </div>
                <div>
                  <dt>{t('lastReceived')}</dt>
                  <dd>
                    {connection.last_received_at
                      ? formatDate(connection.last_received_at)
                      : t('waitingMessage')}
                  </dd>
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
              label={t('collecting')}
            />
          </div>
        )}
        {reportError && (
          <MessageBar intent="error">
            <MessageBarBody>{errorText(reportError)}</MessageBarBody>
          </MessageBar>
        )}
        {report && (
          <>
            <p className="report-context">
              {t(reportError ? 'lastCollected' : 'collected', {
                date: formatDate(report.collected_at),
                session: report.session_id ?? t('system'),
              })}
            </p>
            <div className="system-report-grid">
              {report.sections.map((section, index) => (
                <Card className="system-card" key={`${section.title}-${index}`}>
                  <h2>{message(section.title, section.title_message)}</h2>
                  <dl>
                    {section.rows.map((row, rowIndex) => (
                      <div
                        className={`system-row ${row.bad ? 'system-row-bad' : ''}`}
                        key={`${row.key}-${rowIndex}`}
                      >
                        <dt>
                          {message(row.key, row.key_message)}
                          {row.bad && (
                            <Badge color="danger">{t('abnormal')}</Badge>
                          )}
                        </dt>
                        <dd>{message(row.value, row.value_message)}</dd>
                        {row.warning && (
                          <dd className="system-warning">
                            {message(row.warning, row.warning_message)}
                          </dd>
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
