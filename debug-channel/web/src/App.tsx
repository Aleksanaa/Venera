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
import veneraLogo from '../../../docs/venera.svg';
import { EventDetails, type LogEvent } from './EventDetails';
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
const labels: Record<keyof Filters, string> = {
  component: '组件',
  level: '级别',
  program: '程序',
  q: '正文搜索',
  from: '开始时间',
  until: '结束时间',
};
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
    throw new Error(body.error || '读取日志失败');
  }
  return response.json();
}
function summary(event: LogEvent) {
  return event.message.text ?? JSON.stringify(event.message);
}
function programName(event: LogEvent) {
  const process = event.message.process;
  if (process?.exe_path) return process.exe_path.split(/[\\/]/).at(-1);
  return process?.pid !== undefined ? `进程 ${process.pid}` : '系统';
}

export function App() {
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
      .map((key) => `${labels[key]}：${filters[key]}`)
      .join(' · ') || '全部事件';
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

  const detail = <EventDetails selected={selected} detailError={detailError} />;

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
          aria-label="工作区导航"
          onTabSelect={(_, data) => {
            setPage(String(data.value));
            if (data.value === 'system') {
              setSystemVisited(true);
              setDrawer(null);
            }
          }}
        >
          <Tab value="logs" icon={<HistoryRegular />}>
            宿主日志
          </Tab>
          <Tab value="system" icon={<DesktopRegular />}>
            系统信息
          </Tab>
        </TabList>
        <p>检索事件、追踪运行状态。</p>
      </aside>
      <main hidden={page !== 'logs'}>
        <header>
          <div>
            <h1>日志工作台</h1>
            <p>查看宿主事件与程序日志，定位运行细节。</p>
          </div>
          <Badge appearance="tint" color={error ? 'danger' : 'brand'}>
            {error
              ? '连接异常'
              : loading
                ? '正在连接'
                : following
                  ? '实时跟随'
                  : '历史浏览'}
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
                  aria-label="关闭"
                  onClick={() => setDrawer(null)}
                />
              }
            >
              事件详情
            </DrawerHeaderTitle>
          </DrawerHeader>
          <DrawerBody className="drawer-body">{detail}</DrawerBody>
        </OverlayDrawer>
        <Toolbar aria-label="日志工具栏" className="toolbar">
          <ToolbarButton
            icon={<FilterRegular />}
            onClick={() => setFiltersOpen((open) => !open)}
            aria-expanded={filtersOpen}
            aria-controls="event-filters"
          >
            {filtersOpen ? '收起筛选' : '筛选'}
          </ToolbarButton>
          <ToolbarButton
            icon={<ArrowSyncRegular />}
            onClick={() => void loadOlder()}
            disabled={!hasOlder || olderBusy || loading}
          >
            {olderBusy ? '正在加载' : '加载更早'}
          </ToolbarButton>
          <ToolbarButton
            as="a"
            href={`/api/export${params ? `?${params}` : ''}`}
            icon={<ArrowDownloadRegular />}
          >
            导出 JSONL
          </ToolbarButton>
          <Switch
            label="实时跟随"
            checked={following}
            onChange={(_, data) => {
              follow.current = data.checked;
              setFollowing(data.checked);
            }}
          />
          <span className="count">{events.length} 条事件</span>
        </Toolbar>
        {filtersOpen && (
          <section
            id="event-filters"
            className="filter-panel"
            aria-label="筛选事件"
          >
            <form
              onSubmit={(event) => {
                event.preventDefault();
                setFilters({ ...draft });
              }}
            >
              <div className="filter-grid">
                {(Object.keys(labels) as (keyof Filters)[]).map((key) => (
                  <Field key={key} label={labels[key]}>
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
                  应用筛选
                </Button>
                <Button
                  onClick={() => {
                    setDraft(empty);
                    setFilters(empty);
                  }}
                >
                  重置
                </Button>
                <Button onClick={() => setFiltersOpen(false)}>收起</Button>
              </div>
            </form>
          </section>
        )}
        <div className="filter-summary" title={filterSummary}>
          {filterSummary}
        </div>
        {error && (
          <MessageBar intent="error">
            <MessageBarBody>{error}</MessageBarBody>
          </MessageBar>
        )}
        {trimmed && (
          <MessageBar>
            <MessageBarBody>
              当前会话已保留最近浏览的 5000
              条事件。可调整时间范围或导出当前筛选的全部事件。
            </MessageBarBody>
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
              <strong>事件历史</strong>
              <Button
                className="reset-columns"
                size="small"
                appearance="subtle"
                onClick={() => setColumnWidths([160, 170, 130, 80, 180])}
              >
                恢复列宽
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
                  {['程序', '时间', '组件', '级别', '正文'].map(
                    (label, index) => (
                      <div className="column-title" key={label}>
                        <span>{label}</span>
                        <div
                          className="column-resizer"
                          role="separator"
                          tabIndex={0}
                          aria-orientation="vertical"
                          aria-label={`调整${label}列宽`}
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
                    ),
                  )}
                </div>
                <section className="events" aria-label="事件历史">
                  {loading ? (
                    <p className="empty">正在读取事件…</p>
                  ) : events.length === 0 ? (
                    <p className="empty">当前条件下暂无事件</p>
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
                        aria-label={`查看事件 ${event.id}`}
                      >
                        <span
                          className="program"
                          title={event.message.process?.exe_path}
                        >
                          {programName(event)}
                        </span>
                        <time>
                          {new Date(event.received_at).toLocaleString()}
                        </time>
                        <span>
                          {event.message.component ??
                            event.message.type ??
                            '事件'}
                        </span>
                        <span
                          className={`level ${event.message.level === 'error' ? 'error' : ''}`}
                        >
                          {event.message.level ?? event.message.type ?? 'event'}
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
                aria-label="调整详情宽度"
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
