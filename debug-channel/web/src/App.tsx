import {
  Badge,
  Button,
  Card,
  DrawerBody,
  DrawerHeader,
  DrawerHeaderTitle,
  Field,
  FluentProvider,
  Input,
  MessageBar,
  MessageBarBody,
  OverlayDrawer,
  Switch,
  Tab,
  TabList,
  Toolbar,
  ToolbarButton,
  webDarkTheme,
} from '@fluentui/react-components';
import {
  ArrowDownloadRegular,
  ArrowSyncRegular,
  DesktopRegular,
  DismissRegular,
  FilterRegular,
  HistoryRegular,
  SearchRegular,
} from '@fluentui/react-icons';
import type { KeyboardEvent, PointerEvent } from 'react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import veneraLogo from '../../../docs/venera.svg';
import { type MessageKey } from './catalog';
import { EventDetails, type LogEvent } from './EventDetails';
import { enumLabel, formatDate, i18n } from './i18n';
import { LanguageSelect } from './LanguageSelect';
import { SystemInfo } from './SystemInfo';

type Page = {
  events: LogEvent[];
  oldest_id: string | null;
  latest_id: string | null;
};
type Filters = {
  component: string;
  level: string;
  program: string;
  q: string;
  from: string;
  until: string;
};
const empty: Filters = {
  component: '',
  level: '',
  program: '',
  q: '',
  from: '',
  until: '',
};
const labels = {
  component: 'component',
  level: 'level',
  program: 'program',
  q: 'searchText',
  from: 'from',
  until: 'until',
} as const satisfies Record<keyof Filters, MessageKey>;
function query(filters: Filters) {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(filters)) {
    if (value)
      params.set(
        key,
        key === 'from' || key === 'until'
          ? new Date(value).toISOString()
          : value,
      );
  }
  return params.toString();
}
async function read<T>(url: string, signal?: AbortSignal): Promise<T> {
  const response = await fetch(url, { signal });
  if (!response.ok) {
    const body = await response
      .json()
      .catch(() => ({ error: response.statusText }));
    throw new Error(body.error || 'readLogsError');
  }
  return response.json();
}
function summary(event: LogEvent) {
  return event.message.text ?? JSON.stringify(event.message);
}
function programName(event: LogEvent) {
  const process = event.message.process;
  if (process?.exe_path) return process.exe_path.split(/[\\/]/).at(-1);
  return process?.pid !== undefined
    ? i18n.t('process', { pid: process.pid })
    : i18n.t('system');
}

export function App() {
  const { t } = useTranslation();
  const [page, setPage] = useState('logs');
  const [systemVisited, setSystemVisited] = useState(false);
  const workspace = useRef<HTMLDivElement>(null);
  const [workspaceWidth, setWorkspaceWidth] = useState(1000);
  const [detailWidth, setDetailWidth] = useState(340);
  const detailDrag = useRef<{ x: number; width: number } | null>(null);
  const maxDetailWidth = Math.max(
    260,
    Math.min(workspaceWidth * 0.6, workspaceWidth - 312),
  );
  function resizeDetail(width: number) {
    setDetailWidth(Math.max(260, Math.min(maxDetailWidth, width)));
  }
  useEffect(() => {
    const element = workspace.current;
    if (!element) return;
    const update = () => {
      const width = element.getBoundingClientRect().width;
      if (width > 0) {
        setWorkspaceWidth(width);
        setDetailWidth((current) =>
          Math.max(260, Math.min(current, width * 0.6, width - 312)),
        );
      }
    };
    update();
    const observer =
      typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(update);
    observer?.observe(element);
    window.addEventListener('resize', update);
    return () => {
      observer?.disconnect();
      window.removeEventListener('resize', update);
    };
  }, []);
  const [narrow, setNarrow] = useState(
    () => window.matchMedia('(max-width: 1150px)').matches,
  );
  const [drawer, setDrawer] = useState<'detail' | null>(null);
  const [filtersOpen, setFiltersOpen] = useState(false);
  const [draft, setDraft] = useState(empty);
  const [filters, setFilters] = useState(empty);
  const [events, setEvents] = useState<LogEvent[]>([]);
  const [following, setFollowing] = useState(true);
  const follow = useRef(true);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);
  const [olderBusy, setOlderBusy] = useState(false);
  const [hasOlder, setHasOlder] = useState(false);
  const [trimmed, setTrimmed] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [selected, setSelected] = useState<LogEvent | null>(null);
  const [detailError, setDetailError] = useState('');
  const epoch = useRef(0);
  const olderRequest = useRef(false);
  const requestBusy = useRef(false);
  const cursor = useRef<string | null>(null);
  const rows = useRef<LogEvent[]>([]);
  const list = useRef<HTMLDivElement>(null);
  const [columnWidths, setColumnWidths] = useState([160, 170, 130, 80, 180]);
  const resizeStart = useRef<{
    index: number;
    x: number;
    width: number;
  } | null>(null);
  const params = query(filters);
  const filterSummary =
    (Object.keys(labels) as (keyof Filters)[])
      .filter((key) => filters[key])
      .map((key) =>
        t('filterSummary', { label: t(labels[key]), value: filters[key] }),
      )
      .join(' · ') || t('allEvents');
  const columnTemplate = `${columnWidths
    .slice(0, 4)
    .map((width) => `${width}px`)
    .join(' ')} minmax(${columnWidths[4]}px, 1fr)`;
  const tableMinWidth = columnWidths.reduce((sum, width) => sum + width, 64);

  function resizeColumn(index: number, nextWidth: number) {
    setColumnWidths((current) =>
      current.map((width, currentIndex) =>
        currentIndex === index
          ? Math.max(index === 4 ? 180 : index === 3 ? 64 : 96, nextWidth)
          : width,
      ),
    );
  }

  function beginResize(index: number, event: PointerEvent<HTMLDivElement>) {
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
    resizeStart.current = {
      index,
      x: event.clientX,
      width:
        index === 4
          ? event.currentTarget.parentElement?.getBoundingClientRect().width ||
            columnWidths[index]
          : columnWidths[index],
    };
  }

  function moveResize(event: PointerEvent<HTMLDivElement>) {
    const start = resizeStart.current;
    if (!start) return;
    resizeColumn(start.index, start.width + event.clientX - start.x);
  }

  function endResize(event: PointerEvent<HTMLDivElement>) {
    if (event.currentTarget.hasPointerCapture(event.pointerId))
      event.currentTarget.releasePointerCapture(event.pointerId);
    resizeStart.current = null;
  }

  function handleResizeKey(
    index: number,
    event: KeyboardEvent<HTMLDivElement>,
  ) {
    if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
    event.preventDefault();
    const width =
      index === 4
        ? event.currentTarget.parentElement?.getBoundingClientRect().width ||
          columnWidths[index]
        : columnWidths[index];
    resizeColumn(index, width + (event.key === 'ArrowRight' ? 16 : -16));
  }

  useEffect(() => {
    const media = window.matchMedia('(max-width: 1150px)');
    const update = () => {
      setNarrow(media.matches);
      setDrawer((current) => (current === 'detail' ? null : current));
    };
    media.addEventListener('change', update);
    return () => media.removeEventListener('change', update);
  }, []);

  function merge(batch: LogEvent[], older: boolean, oldest: string | null) {
    const ids = new Set(rows.current.map((event) => event.id));
    const fresh = batch.filter((event) => !ids.has(event.id));
    const merged = older
      ? [...fresh, ...rows.current]
      : [...rows.current, ...fresh];
    if (merged.length > 5000) setTrimmed(true);
    rows.current = older ? merged.slice(0, 5000) : merged.slice(-5000);
    setEvents(rows.current);
    const first = rows.current[0]?.id;
    setHasOlder(Boolean(first && oldest && BigInt(first) > BigInt(oldest)));
  }
  const mergeRef = useRef(merge);
  mergeRef.current = merge;

  useEffect(() => {
    const generation = ++epoch.current;
    const controller = new AbortController();
    let busy = false;
    let ready = false;
    olderRequest.current = false;
    requestBusy.current = false;
    setOlderBusy(false);
    cursor.current = null;
    rows.current = [];
    setEvents([]);
    setSelectedId(null);
    setSelected(null);
    setTrimmed(false);
    setHasOlder(false);
    setLoading(true);
    setError('');
    async function load() {
      if (busy || requestBusy.current) return;
      busy = true;
      requestBusy.current = true;
      const search = new URLSearchParams(params);
      search.set('limit', '200');
      if (ready && cursor.current) search.set('after', cursor.current);
      try {
        const page = await read<Page>(
          `/api/events?${search}`,
          controller.signal,
        );
        if (generation !== epoch.current) return;
        cursor.current =
          page.events.at(-1)?.id ?? (ready ? cursor.current : page.latest_id);
        mergeRef.current(page.events, false, page.oldest_id);
        ready = true;
        setError('');
      } catch (reason) {
        if (!controller.signal.aborted && generation === epoch.current)
          setError(reason instanceof Error ? reason.message : String(reason));
      } finally {
        busy = false;
        if (generation === epoch.current) {
          requestBusy.current = false;
          setLoading(false);
        }
      }
    }
    void load();
    const timer = setInterval(() => {
      if (follow.current) void load();
    }, 1000);
    return () => {
      controller.abort();
      clearInterval(timer);
    };
  }, [params]);

  useEffect(() => {
    if (following && events.length)
      list.current?.scrollTo?.({ top: list.current.scrollHeight });
  }, [events, following]);

  useEffect(() => {
    if (!selectedId) return;
    const controller = new AbortController();
    setDetailError('');
    void read<LogEvent>(`/api/events/${selectedId}`, controller.signal)
      .then((event) => {
        if (!controller.signal.aborted) setSelected(event);
      })
      .catch((reason) => {
        if (!controller.signal.aborted) setDetailError(String(reason.message));
      });
    return () => controller.abort();
  }, [selectedId]);

  async function loadOlder() {
    const first = rows.current[0]?.id;
    if (!first || olderRequest.current || requestBusy.current) return;
    const generation = epoch.current;
    olderRequest.current = true;
    requestBusy.current = true;
    setOlderBusy(true);
    const search = new URLSearchParams(params);
    search.set('limit', '200');
    search.set('before', first);
    try {
      const page = await read<Page>(`/api/events?${search}`);
      if (generation !== epoch.current) return;
      mergeRef.current(page.events, true, page.oldest_id);
      cursor.current = rows.current.at(-1)?.id ?? cursor.current;
      if (!page.events.length) setHasOlder(false);
      setError('');
    } catch (reason) {
      if (generation === epoch.current)
        setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      if (generation === epoch.current) {
        olderRequest.current = false;
        requestBusy.current = false;
        setOlderBusy(false);
      }
    }
  }

  const detail = (
    <EventDetails
      selected={selected}
      detailError={
        detailError === 'readLogsError' ? t('readLogsError') : detailError
      }
    />
  );

  return (
    <FluentProvider theme={webDarkTheme} className="app">
      <aside className="navigation">
        <div className="brand" role="img" aria-label="Venera">
          <img src={veneraLogo} alt="" />
          <strong>Venera</strong>
        </div>
        <TabList
          vertical
          selectedValue={page}
          aria-label={t('navigation')}
          onTabSelect={(_, data) => {
            setPage(String(data.value));
            if (data.value === 'system') {
              setSystemVisited(true);
              setDrawer(null);
            }
          }}
        >
          <Tab value="logs" icon={<HistoryRegular />}>
            {t('hostLogs')}
          </Tab>
          <Tab value="system" icon={<DesktopRegular />}>
            {t('systemInfo')}
          </Tab>
        </TabList>
        <p>{t('navigationHint')}</p>
        <LanguageSelect />
      </aside>
      <main hidden={page !== 'logs'}>
        <header>
          <div>
            <h1>{t('workbench')}</h1>
            <p>{t('workbenchHint')}</p>
          </div>
          <Badge appearance="tint" color={error ? 'danger' : 'brand'}>
            {error
              ? t('connectionError')
              : loading
                ? t('connecting')
                : following
                  ? t('following')
                  : t('historyBrowsing')}
          </Badge>
        </header>
        <OverlayDrawer
          className="workbench-drawer"
          position="end"
          open={narrow && drawer === 'detail'}
          onOpenChange={(_, data) => {
            if (!data.open) setDrawer(null);
          }}
        >
          <DrawerHeader>
            <DrawerHeaderTitle
              action={
                <Button
                  appearance="subtle"
                  icon={<DismissRegular />}
                  aria-label={t('close')}
                  onClick={() => setDrawer(null)}
                />
              }
            >
              {t('eventDetails')}
            </DrawerHeaderTitle>
          </DrawerHeader>
          <DrawerBody className="drawer-body">{detail}</DrawerBody>
        </OverlayDrawer>
        <Toolbar aria-label={t('toolbar')} className="toolbar">
          <ToolbarButton
            icon={<FilterRegular />}
            onClick={() => setFiltersOpen((open) => !open)}
            aria-expanded={filtersOpen}
            aria-controls="event-filters"
          >
            {filtersOpen ? t('collapseFilters') : t('filters')}
          </ToolbarButton>
          <ToolbarButton
            icon={<ArrowSyncRegular />}
            onClick={() => void loadOlder()}
            disabled={!hasOlder || olderBusy || loading}
          >
            {olderBusy ? t('loading') : t('loadOlder')}
          </ToolbarButton>
          <ToolbarButton
            as="a"
            href={`/api/export${params ? `?${params}` : ''}`}
            icon={<ArrowDownloadRegular />}
          >
            {t('export')}
          </ToolbarButton>
          <Switch
            label={t('following')}
            checked={following}
            onChange={(_, data) => {
              follow.current = data.checked;
              setFollowing(data.checked);
            }}
          />
          <span className="count">
            {t('eventCount', { count: events.length })}
          </span>
        </Toolbar>
        {filtersOpen && (
          <section
            id="event-filters"
            className="filter-panel"
            aria-label={t('filterEvents')}
          >
            <form
              onSubmit={(event) => {
                event.preventDefault();
                setFilters({ ...draft });
              }}
            >
              <div className="filter-grid">
                {(Object.keys(labels) as (keyof Filters)[]).map((key) => (
                  <Field key={key} label={t(labels[key])}>
                    <Input
                      type={
                        key === 'from' || key === 'until'
                          ? 'datetime-local'
                          : 'text'
                      }
                      value={draft[key]}
                      onChange={(_, data) =>
                        setDraft((previous) => ({
                          ...previous,
                          [key]: data.value,
                        }))
                      }
                    />
                  </Field>
                ))}
              </div>
              <div className="filter-actions">
                <Button
                  type="submit"
                  appearance="primary"
                  icon={<SearchRegular />}
                >
                  {t('applyFilters')}
                </Button>
                <Button
                  onClick={() => {
                    setDraft(empty);
                    setFilters(empty);
                  }}
                >
                  {t('reset')}
                </Button>
                <Button onClick={() => setFiltersOpen(false)}>
                  {t('collapse')}
                </Button>
              </div>
            </form>
          </section>
        )}
        <div className="filter-summary" title={filterSummary}>
          {filterSummary}
        </div>
        {error && (
          <MessageBar intent="error">
            <MessageBarBody>
              {error === 'readLogsError' ? t('readLogsError') : error}
            </MessageBarBody>
          </MessageBar>
        )}
        {trimmed && (
          <MessageBar>
            <MessageBarBody>{t('retained', { count: 5000 })}</MessageBarBody>
          </MessageBar>
        )}
        <div
          className="workspace"
          ref={workspace}
          style={
            narrow
              ? undefined
              : {
                  gridTemplateColumns: `minmax(300px, 1fr) 12px ${detailWidth}px`,
                }
          }
        >
          <Card className="event-card">
            <div className="list-heading">
              <strong>{t('eventHistory')}</strong>
              <Button
                className="reset-columns"
                size="small"
                appearance="subtle"
                onClick={() => setColumnWidths([160, 170, 130, 80, 180])}
              >
                {t('resetColumns')}
              </Button>
            </div>
            <div className="table-scroll" ref={list}>
              <div
                className="table-content"
                style={{ minWidth: tableMinWidth }}
              >
                <div
                  className="column-heading"
                  style={{ gridTemplateColumns: columnTemplate }}
                >
                  {(
                    [
                      'program',
                      'time',
                      'component',
                      'level',
                      'message',
                    ] as const
                  ).map((label, index) => (
                    <div className="column-title" key={label}>
                      <span>{t(label)}</span>
                      <div
                        className="column-resizer"
                        role="separator"
                        tabIndex={0}
                        aria-orientation="vertical"
                        aria-label={t('resizeColumn', { label: t(label) })}
                        aria-valuemin={
                          index === 4 ? 180 : index === 3 ? 64 : 96
                        }
                        aria-valuenow={columnWidths[index]}
                        onPointerDown={(event) => beginResize(index, event)}
                        onPointerMove={moveResize}
                        onPointerUp={endResize}
                        onPointerCancel={endResize}
                        onLostPointerCapture={() => {
                          resizeStart.current = null;
                        }}
                        onKeyDown={(event) => handleResizeKey(index, event)}
                      />
                    </div>
                  ))}
                </div>
                <section className="events" aria-label={t('eventHistory')}>
                  {loading ? (
                    <p className="empty">{t('readingEvents')}</p>
                  ) : events.length === 0 ? (
                    <p className="empty">{t('emptyEvents')}</p>
                  ) : (
                    events.map((event) => (
                      <button
                        type="button"
                        key={event.id}
                        className={`event-row ${selectedId === event.id ? 'selected' : ''}`}
                        style={{ gridTemplateColumns: columnTemplate }}
                        onClick={() => {
                          setSelected(event);
                          setSelectedId(event.id);
                          if (narrow) setDrawer('detail');
                        }}
                        aria-label={t('viewEvent', { id: event.id })}
                      >
                        <span
                          className="program"
                          title={event.message.process?.exe_path}
                        >
                          {programName(event)}
                        </span>
                        <time>{formatDate(event.received_at)}</time>
                        <span>
                          {event.message.component ??
                            enumLabel(event.message.type ?? 'event')}
                        </span>
                        <span
                          className={`level ${event.message.level === 'error' ? 'error' : ''}`}
                        >
                          {enumLabel(
                            event.message.level ??
                              event.message.type ??
                              'event',
                          )}
                        </span>
                        <span className="summary">{summary(event)}</span>
                      </button>
                    ))
                  )}
                </section>
              </div>
            </div>
          </Card>
          {!narrow && (
            <>
              <div
                className="detail-resizer"
                role="separator"
                tabIndex={0}
                aria-label={t('resizeDetails')}
                aria-orientation="vertical"
                aria-valuemin={260}
                aria-valuemax={maxDetailWidth}
                aria-valuenow={detailWidth}
                onPointerDown={(event) => {
                  event.currentTarget.setPointerCapture(event.pointerId);
                  detailDrag.current = { x: event.clientX, width: detailWidth };
                }}
                onPointerMove={(event) => {
                  if (detailDrag.current)
                    resizeDetail(
                      detailDrag.current.width +
                        detailDrag.current.x -
                        event.clientX,
                    );
                }}
                onPointerUp={(event) => {
                  if (event.currentTarget.hasPointerCapture(event.pointerId))
                    event.currentTarget.releasePointerCapture(event.pointerId);
                  detailDrag.current = null;
                }}
                onPointerCancel={() => {
                  detailDrag.current = null;
                }}
                onLostPointerCapture={() => {
                  detailDrag.current = null;
                }}
                onKeyDown={(event) => {
                  if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
                    event.preventDefault();
                    resizeDetail(
                      detailWidth + (event.key === 'ArrowLeft' ? 16 : -16),
                    );
                  }
                }}
              />
              <Card className="details">{detail}</Card>
            </>
          )}
        </div>
      </main>
      {systemVisited && (
        <main hidden={page !== 'system'}>
          <SystemInfo />
        </main>
      )}
    </FluentProvider>
  );
}
