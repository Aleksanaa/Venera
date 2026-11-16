import {
  Button,
  MessageBar,
  MessageBarBody,
  Tab,
  TabList,
} from '@fluentui/react-components';
import { ClipboardRegular } from '@fluentui/react-icons';
import { useEffect, useId, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { enumLabel, formatDate } from './i18n';
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
  overview: 'overview',
  metadata: 'metadata',
  raw: 'raw',
} as const;

export function EventDetails({ selected, detailError }: EventDetailsProps) {
  const { t } = useTranslation();
  const [tab, setTab] = useState<DetailTab>('overview');
  const [copyState, setCopyState] = useState<'' | 'copied' | 'copyFailed'>('');
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
      if (selection.current === event) setCopyState('copied');
    } catch {
      if (selection.current === event) setCopyState('copyFailed');
    }
  }

  return (
    <section className="event-details" aria-label={t('eventDetails')}>
      <h2 className="event-details-title">{t('eventDetails')}</h2>
      {detailError && (
        <MessageBar intent="error">
          <MessageBarBody>{detailError}</MessageBarBody>
        </MessageBar>
      )}
      {selected ? (
        <>
          <TabList
            className="event-details-tabs"
            aria-label={t('detailViews')}
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
                {t(tabLabels[value])}
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
                  <dt>{t('eventId')}</dt>
                  <dd>{selected.id}</dd>
                  <dt>{t('receivedAt')}</dt>
                  <dd>{formatDate(selected.received_at)}</dd>
                  <dt>{t('type')}</dt>
                  <dd>{enumLabel(selected.message.type ?? 'event')}</dd>
                  <dt>{t('component')}</dt>
                  <dd>{selected.message.component ?? '—'}</dd>
                  <dt>{t('level')}</dt>
                  <dd>{enumLabel(selected.message.level ?? '—')}</dd>
                  <dt>{t('program')}</dt>
                  <dd>{selected.message.process?.exe_path ?? '—'}</dd>
                  <dt>{t('processId')}</dt>
                  <dd>{selected.message.process?.pid ?? '—'}</dd>
                </dl>
                <h3>{t('message')}</h3>
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
                    {t('copyRaw')}
                  </Button>
                  {copyState && <span role="status">{t(copyState)}</span>}
                </div>
                <pre>{selected.raw_message}</pre>
              </>
            )}
          </div>
        </>
      ) : (
        <p className="event-details-empty">{t('selectEvent')}</p>
      )}
    </section>
  );
}
