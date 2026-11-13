import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  CSSProperties,
  KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { BrandMark } from "./BrandMark";
import "./App.css";

type Brightness = {
  raw: number;
  min: number;
  max: number;
  percent: number;
  method: "high_level" | "vcp";
};

type DdcError = {
  code: number;
  context: string;
  transient: boolean;
};

type WriteOutcome = {
  brightness: Brightness | null;
  error: DdcError | null;
};

type Monitor = {
  id: string;
  gdiName: string;
  name: string;
  description: string;
  devicePath: string;
  connection: string;
  primary: boolean;
  x?: number;
  y?: number;
  width?: number;
  height?: number;
  brightness: Brightness | null;
  lastPercent: number | null;
  error: DdcError | null;
};

type MonitorList = {
  monitors: Monitor[];
  notes: string[];
};

type Hotkeys = {
  enabled: boolean;
  increase: string;
  decrease: string;
};

const DDC_HINTS: Record<number, string> = {
  0xc0262580: "DDC/CI 未启用，请在显示器 OSD 中打开",
  0xc0262581: "显示器不在线，可能已休眠或断开",
  0xc0262582: "控制通道无响应，断电 30 秒再试",
  0xc0262583: "读取无响应，检查视频线或断电",
  0xc0262584: "该显示器不支持 DDC/CI 调亮度",
  0xc026258d: "显示器已断开，重新连接后刷新",
};

function monitorHint(monitor: Monitor) {
  const code = monitor.error?.code ?? 0;
  return DDC_HINTS[code] ?? "亮度暂不可读，检查 DDC/CI 或连接";
}

function monitorName(monitor: Monitor) {
  return monitor.name || monitor.description || monitor.gdiName;
}

function monitorMeta(monitor: Monitor) {
  const resolution =
    monitor.width && monitor.height
      ? `${monitor.width}×${monitor.height}`
      : null;
  return [monitor.connection, resolution].filter(Boolean).join(" · ");
}

function monitorValue(monitor: Monitor, values: Record<string, number>) {
  return (
    values[monitor.id] ??
    monitor.brightness?.percent ??
    monitor.lastPercent ??
    0
  );
}

function RefreshIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M19.5 12a7.5 7.5 0 1 1-2.2-5.3" />
      <path d="M19.8 4.6v3.9h-3.9" />
    </svg>
  );
}

function SettingsIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 15.2a3.2 3.2 0 1 0 0-6.4 3.2 3.2 0 0 0 0 6.4Z" />
      <path d="M19.4 13.4a7.8 7.8 0 0 0 0-2.8l1.9-1.4-1.9-3.2-2.2.9a7.8 7.8 0 0 0-2.4-1.4L14.5 3h-5l-.3 2.5a7.8 7.8 0 0 0-2.4 1.4l-2.2-.9-1.9 3.2 1.9 1.4a7.8 7.8 0 0 0 0 2.8l-1.9 1.4 1.9 3.2 2.2-.9a7.8 7.8 0 0 0 2.4 1.4l.3 2.5h5l.3-2.5a7.8 7.8 0 0 0 2.4-1.4l2.2.9 1.9-3.2-1.9-1.4Z" />
    </svg>
  );
}

function Slider({
  value,
  onChange,
  label,
  disabled = false,
  step = 1,
  inputRef,
  onFocus,
}: {
  value: number;
  onChange?: (value: number) => void;
  label: string;
  disabled?: boolean;
  step?: number;
  inputRef?: (node: HTMLInputElement | null) => void;
  onFocus?: () => void;
}) {
  const onKeyDown = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    if (!onChange) return;
    let next: number | null = null;
    switch (event.key) {
      case "ArrowRight":
      case "ArrowUp":
        next = value + step;
        break;
      case "ArrowLeft":
      case "ArrowDown":
        next = value - step;
        break;
      case "PageUp":
        next = value + 10;
        break;
      case "PageDown":
        next = value - 10;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = 100;
        break;
      default:
        return;
    }
    event.preventDefault();
    onChange(Math.min(100, Math.max(0, next)));
  };

  return (
    <input
      ref={inputRef}
      className="slider"
      type="range"
      min={0}
      max={100}
      step={1}
      value={value}
      aria-label={label}
      aria-valuetext={`${value}%`}
      disabled={disabled}
      style={{ "--fill": `${value}%` } as CSSProperties}
      onChange={(event) => onChange?.(Number(event.target.value))}
      onKeyDown={onKeyDown}
      onFocus={onFocus}
    />
  );
}

type PaneBox = {
  monitor: Monitor;
  index: number;
  left: number;
  top: number;
  width: number;
  height: number;
};

function layoutPanes(
  monitors: Monitor[],
  stage: { width: number; height: number },
): PaneBox[] {
  if (monitors.length === 0 || stage.width < 40 || stage.height < 30) {
    return [];
  }
  const padX = 12;
  const padTop = 28;
  const padBottom = 12;
  const rects = monitors.map((monitor, index) => {
    const width = monitor.width && monitor.width > 0 ? monitor.width : 1920;
    const height = monitor.height && monitor.height > 0 ? monitor.height : 1080;
    return {
      monitor,
      index,
      x: monitor.x ?? index * 1960,
      y: monitor.y ?? 0,
      w: width,
      h: height,
    };
  });
  const minX = Math.min(...rects.map((rect) => rect.x));
  const minY = Math.min(...rects.map((rect) => rect.y));
  const totalW = Math.max(...rects.map((rect) => rect.x + rect.w)) - minX;
  const totalH = Math.max(...rects.map((rect) => rect.y + rect.h)) - minY;
  const availableW = stage.width - padX * 2;
  const availableH = stage.height - padTop - padBottom;
  const scale = Math.min(availableW / totalW, availableH / totalH);
  const offsetX = padX + (availableW - totalW * scale) / 2;
  const offsetY = padTop + (availableH - totalH * scale) / 2;
  const gap = 1.6;
  return rects.map((rect) => ({
    monitor: rect.monitor,
    index: rect.index,
    left: offsetX + (rect.x - minX) * scale + gap,
    top: offsetY + (rect.y - minY) * scale + gap,
    width: Math.max(8, rect.w * scale - gap * 2),
    height: Math.max(6, rect.h * scale - gap * 2),
  }));
}

function LightStage({
  monitors,
  values,
  activeId,
  onSelect,
  onHover,
}: {
  monitors: Monitor[];
  values: Record<string, number>;
  activeId: string | null;
  onSelect: (id: string) => void;
  onHover: (id: string | null) => void;
}) {
  const stageRef = useRef<HTMLDivElement | null>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });

  useEffect(() => {
    const node = stageRef.current;
    if (!node) return;
    const observer = new ResizeObserver(() => {
      const rect = node.getBoundingClientRect();
      setSize((current) =>
        Math.abs(current.width - rect.width) > 0.5 ||
        Math.abs(current.height - rect.height) > 0.5
          ? { width: rect.width, height: rect.height }
          : current,
      );
    });
    observer.observe(node);
    const rect = node.getBoundingClientRect();
    setSize({ width: rect.width, height: rect.height });
    return () => observer.disconnect();
  }, []);

  const panes = useMemo(() => layoutPanes(monitors, size), [monitors, size]);
  const active = monitors.find((monitor) => monitor.id === activeId);

  const ambient = useMemo(() => {
    if (monitors.length === 0) return 0;
    const total = monitors.reduce(
      (sum, monitor) => sum + monitorValue(monitor, values),
      0,
    );
    return total / monitors.length / 100;
  }, [monitors, values]);

  return (
    <div
      className="light-stage"
      ref={stageRef}
      style={{ "--ambient": ambient } as CSSProperties}
      onMouseLeave={() => onHover(null)}
    >
      <span className="stage-label">桌面光图</span>
      <span className="stage-note" aria-hidden="true">
        {active
          ? active.brightness === null
            ? `${monitorName(active)} · 不可控`
            : `${monitorName(active)} · ${monitorValue(active, values)}%`
          : "按实际排布"}
      </span>
      <span className="stage-ambient" aria-hidden="true" />
      <span className="stage-floor" aria-hidden="true" />
      {panes.map((pane) => {
        const level = monitorValue(pane.monitor, values) / 100;
        const selected = activeId === pane.monitor.id;
        const disabled = pane.monitor.brightness === null;
        return (
          <button
            key={pane.monitor.id}
            type="button"
            className={`pane ${selected ? "is-active" : ""} ${
              disabled ? "is-off" : ""
            }`}
            style={
              {
                left: `${pane.left}px`,
                top: `${pane.top}px`,
                width: `${pane.width}px`,
                height: `${pane.height}px`,
                "--level": level,
                "--i": pane.index,
              } as CSSProperties
            }
            aria-label={`${monitorName(pane.monitor)} ${
              disabled ? "不可控" : `${Math.round(level * 100)}%`
            }，定位到滑块`}
            title={monitorName(pane.monitor)}
            onClick={() => onSelect(pane.monitor.id)}
            onMouseEnter={() => onHover(pane.monitor.id)}
            onFocus={() => onHover(pane.monitor.id)}
            onBlur={() => onHover(null)}
          >
            <span className="pane-screen">
              <span className="pane-fill" aria-hidden="true" />
              <span className="pane-sheen" aria-hidden="true" />
            </span>
            <span className="pane-index" aria-hidden="true">
              {String(pane.index + 1).padStart(2, "0")}
            </span>
            {disabled && (
              <span className="pane-warn" aria-hidden="true">
                !
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}

function Skeleton() {
  return (
    <div className="skeleton" aria-hidden="true">
      <div className="skeleton-stage" />
      <div className="skeleton-master" />
      <div className="skeleton-row" />
    </div>
  );
}

function hotkeyKeys(accelerator: string) {
  return accelerator.split("+").map((key) => {
    if (key === "Up") return "↑";
    if (key === "Down") return "↓";
    return key;
  });
}

function MonitorSwatch({ level, stale }: { level: number; stale?: boolean }) {
  return (
    <span
      className={`monitor-swatch ${stale ? "is-stale" : ""}`}
      style={{ "--level": level } as CSSProperties}
      aria-hidden="true"
    >
      <span className="swatch-fill" />
      <span className="swatch-sheen" />
    </span>
  );
}

function App() {
  const [monitors, setMonitors] = useState<Monitor[]>([]);
  const [notes, setNotes] = useState<string[]>([]);
  const [values, setValues] = useState<Record<string, number>>({});
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [step, setStep] = useState(5);
  const [hotkeys, setHotkeys] = useState<Hotkeys | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [hoveredId, setHoveredId] = useState<string | null>(null);

  const pending = useRef(new Map<string, number>());
  const inFlight = useRef(new Set<string>());
  const monitorsRef = useRef<Monitor[]>([]);
  const loadingRef = useRef(false);
  const retryArmed = useRef(true);
  const rootRef = useRef<HTMLElement | null>(null);
  const sliderRefs = useRef(new Map<string, HTMLInputElement>());

  useEffect(() => {
    monitorsRef.current = monitors;
  }, [monitors]);

  const send = (id: string) => {
    if (inFlight.current.has(id)) return;
    const percent = pending.current.get(id);
    if (percent === undefined) return;
    pending.current.delete(id);
    inFlight.current.add(id);
    void invoke<WriteOutcome>("set_brightness", { id, percent })
      .then((outcome) => {
        const stillPending = pending.current.has(id);
        setMonitors((current) =>
          current.map((monitor) =>
            monitor.id === id
              ? {
                  ...monitor,
                  brightness: outcome.brightness ?? monitor.brightness,
                  error: outcome.error,
                }
              : monitor,
          ),
        );
        if (stillPending) return;
        if (outcome.brightness) {
          const actual = outcome.brightness.percent;
          setValues((current) => ({ ...current, [id]: actual }));
        } else if (outcome.error) {
          const known = monitorsRef.current.find(
            (monitor) => monitor.id === id,
          )?.brightness;
          if (known) {
            setValues((current) => ({ ...current, [id]: known.percent }));
          }
        }
      })
      .catch((error: unknown) => {
        const ddc = (error ?? {}) as Partial<DdcError>;
        setMonitors((current) =>
          current.map((monitor) =>
            monitor.id === id
              ? {
                  ...monitor,
                  error: {
                    code: ddc.code ?? 0,
                    context: ddc.context ?? String(error),
                    transient: ddc.transient ?? false,
                  },
                }
              : monitor,
          ),
        );
        const known = monitorsRef.current.find(
          (monitor) => monitor.id === id,
        )?.brightness;
        if (known) {
          setValues((current) => ({ ...current, [id]: known.percent }));
        }
      })
      .finally(() => {
        inFlight.current.delete(id);
        if (pending.current.has(id)) send(id);
      });
  };

  const queueWrite = (id: string, percent: number) => {
    pending.current.set(id, percent);
    send(id);
  };

  const load = useCallback(async (silent = false) => {
    if (loadingRef.current) return;
    loadingRef.current = true;
    if (!silent) setRefreshing(true);
    try {
      const list = await invoke<MonitorList>("list_monitors");
      setMonitors(list.monitors);
      setNotes(list.notes ?? []);
      const fresh = Object.fromEntries(
        list.monitors
          .filter((monitor) => monitor.brightness)
          .map((monitor) => [monitor.id, monitor.brightness!.percent]),
      );
      setValues((current) => {
        const next = { ...fresh };
        for (const id of [...pending.current.keys(), ...inFlight.current]) {
          if (current[id] !== undefined) next[id] = current[id];
        }
        return next;
      });
      setLoadError(null);
    } catch (error) {
      setLoadError(String(error));
    } finally {
      loadingRef.current = false;
      setLoading(false);
      if (!silent) setRefreshing(false);
    }
  }, []);

  // One free re-read per failure episode: monitors often need a few seconds
  // after power or cable recovery before their DDC channel answers again.
  const hasTransientError = monitors.some((monitor) => monitor.error?.transient);
  useEffect(() => {
    if (!hasTransientError) {
      retryArmed.current = true;
      return;
    }
    if (!retryArmed.current) return;
    retryArmed.current = false;
    const timer = window.setTimeout(() => void load(true), 3000);
    return () => window.clearTimeout(timer);
  }, [hasTransientError, load]);

  useEffect(() => {
    let cancelled = false;
    const hydrate = async () => {
      try {
        const snapshot = await invoke<MonitorList | null>(
          "get_monitor_snapshot",
        );
        if (cancelled) return;
        if (snapshot && snapshot.monitors.length > 0) {
          setMonitors(snapshot.monitors);
          setValues(
            Object.fromEntries(
              snapshot.monitors
                .filter((monitor) => monitor.brightness)
                .map((monitor) => [monitor.id, monitor.brightness!.percent]),
            ),
          );
          setLoading(false);
        }
      } catch {
        // 没有缓存就等完整枚举
      }
      if (!cancelled) void load(true);
    };
    void hydrate();
    return () => {
      cancelled = true;
    };
  }, [load]);

  useEffect(() => {
    const subscription = listen<{ id: string; percent: number }[]>(
      "brightness-changed",
      (event) => {
        const updates = (event.payload ?? []).filter(
          (update) =>
            !pending.current.has(update.id) && !inFlight.current.has(update.id),
        );
        if (updates.length === 0) return;
        const changed = new Map(
          updates.map((update) => [update.id, update.percent]),
        );
        setValues((current) => {
          const next = { ...current };
          for (const [id, percent] of changed) next[id] = percent;
          return next;
        });
        setMonitors((current) =>
          current.map((monitor) => {
            const percent = changed.get(monitor.id);
            if (percent === undefined || !monitor.brightness) return monitor;
            return {
              ...monitor,
              brightness: { ...monitor.brightness, percent },
            };
          }),
        );
      },
    );
    return () => {
      void subscription.then((off) => off());
    };
  }, []);

  useEffect(() => {
    const subscriptions = ["refresh-monitors", "monitors-changed"].map((event) =>
      listen(event, () => {
        void load();
      }),
    );
    return () => {
      for (const subscription of subscriptions) {
        void subscription.then((off) => off());
      }
    };
  }, [load]);

  useEffect(() => {
    const loadSettings = () => {
      void invoke<{
        step: number;
        glassOpacity?: number;
        hotkeysEnabled?: boolean;
        hotkeyIncrease?: string;
        hotkeyDecrease?: string;
      }>("get_settings")
        .then((settings) => {
          setStep(settings.step);
          setHotkeys({
            enabled: settings.hotkeysEnabled ?? false,
            increase: settings.hotkeyIncrease ?? "",
            decrease: settings.hotkeyDecrease ?? "",
          });
          const glass = Math.min(100, Math.max(85, settings.glassOpacity ?? 95));
          document.documentElement.style.setProperty(
            "--glass-alpha",
            String(glass / 100),
          );
        })
        .catch(() => {});
    };
    loadSettings();
    const subscription = listen("settings-changed", loadSettings);
    return () => {
      void subscription.then((off) => off());
    };
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        void invoke("close_flyout").catch(() => {});
      }
    };
    // 透明 gutter 上的点击也算“点外面”，与系统弹窗的心智一致。
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Element | null;
      if (!target?.closest(".flyout")) {
        void invoke("close_flyout").catch(() => {});
      }
    };
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("pointerdown", onPointerDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("pointerdown", onPointerDown);
    };
  }, []);

  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const pad =
      parseFloat(
        getComputedStyle(document.documentElement).getPropertyValue("--window-pad"),
      ) || 16;
    let frame = 0;
    let reported = 0;
    let retry = 0;
    let attempts = 0;
    let disposed = false;
    let revealed = false;
    const reveal = () => {
      if (disposed || revealed) return;
      revealed = true;
      const start = () => {
        requestAnimationFrame(() => {
          if (disposed) return;
          requestAnimationFrame(() => {
            if (!disposed) document.documentElement.dataset.flyout = "in";
          });
        });
      };
      // 先让透明窗完成一次合成，再启动入场动画，避免前几帧被窗口显示吃掉。
      void invoke("reveal_flyout").then(start, start);
    };
    const report = () => {
      if (disposed) return;
      // offsetHeight 忽略入场时的缩放变换，窗口高度才能和布局高度一致。
      const target = root.offsetHeight + pad * 2;
      if (Math.abs(target - reported) < 1) {
        reveal();
        return;
      }
      void invoke("resize_flyout", { height: target })
        .then(() => {
          reported = target;
          reveal();
        })
        .catch(() => {
          if (attempts++ < 5) retry = window.setTimeout(report, 150);
          else reveal();
        });
    };
    const schedule = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(report);
    };
    const observer = new ResizeObserver(schedule);
    observer.observe(root);
    schedule();
    const settle = window.setTimeout(schedule, 400);
    void document.fonts?.ready.then(schedule).catch(() => {});
    window.addEventListener("resize", schedule);
    return () => {
      disposed = true;
      observer.disconnect();
      cancelAnimationFrame(frame);
      clearTimeout(retry);
      clearTimeout(settle);
      window.removeEventListener("resize", schedule);
    };
  }, []);

  const ordered = useMemo(
    () =>
      [...monitors].sort(
        (a, b) => (a.x ?? 0) - (b.x ?? 0) || (a.y ?? 0) - (b.y ?? 0),
      ),
    [monitors],
  );

  const available = useMemo(
    () =>
      monitors.filter(
        (monitor): monitor is Monitor & { brightness: Brightness } =>
          monitor.brightness !== null && monitor.error === null,
      ),
    [monitors],
  );

  const unavailable = monitors.length - available.length;

  const master = useMemo(() => {
    if (available.length === 0) return null;
    const list = available.map(
      (monitor) => values[monitor.id] ?? monitor.brightness.percent,
    );
    const min = Math.min(...list);
    const max = Math.max(...list);
    return {
      mixed: min !== max,
      value: Math.round(
        list.reduce((sum, value) => sum + value, 0) / list.length,
      ),
    };
  }, [available, values]);

  const focusMonitor = useCallback((id: string) => {
    setSelectedId(id);
    document.getElementById(`monitor-row-${id}`)?.scrollIntoView({
      block: "nearest",
      behavior: "smooth",
    });
    sliderRefs.current.get(id)?.focus({ preventScroll: true });
  }, []);

  const setOne = (id: string, percent: number) => {
    setValues((current) => ({ ...current, [id]: percent }));
    queueWrite(id, percent);
  };

  const setAll = (percent: number) => {
    setValues((current) => {
      const next = { ...current };
      for (const monitor of available) next[monitor.id] = percent;
      return next;
    });
    for (const monitor of available) queueWrite(monitor.id, percent);
  };

  const statusClass =
    available.length === 0 ? "error" : unavailable > 0 ? "warn" : "ok";

  const activeId = hoveredId ?? selectedId;
  const hotkeyUp = hotkeys?.enabled ? hotkeyKeys(hotkeys.increase) : null;
  const hotkeyDown = hotkeys?.enabled ? hotkeyKeys(hotkeys.decrease) : null;

  return (
    <main className="flyout" ref={rootRef}>
      <header className="brand-bar">
        <span className="brand-mark">
          <BrandMark />
        </span>
        <span className="brand-name">LumenTray</span>
        <span className={`status-dot ${statusClass}`} aria-hidden="true" />
        <span className="brand-state">
          {available.length > 0
            ? unavailable > 0
              ? `${available.length} 台可控 · ${unavailable} 台异常`
              : `${available.length} 台可控`
            : monitors.length > 0
              ? `${unavailable} 台异常`
              : "等待显示器"}
        </span>
        <div className="titlebar-actions">
          <button
            className={`icon-button ${refreshing ? "refreshing" : ""}`}
            type="button"
            title="重新检测显示器"
            aria-label="重新检测显示器"
            onClick={() => void load()}
            disabled={refreshing}
          >
            <RefreshIcon />
          </button>
          <button
            className="icon-button"
            type="button"
            title="设置"
            aria-label="设置"
            onClick={() => void invoke("open_settings").catch(() => {})}
          >
            <SettingsIcon />
          </button>
        </div>
      </header>

      {loading ? (
        <Skeleton />
      ) : loadError ? (
        <>
          <div className="empty-state">
            <p className="empty-title">读取显示器失败</p>
            <p className="empty-hint">{loadError}</p>
            <button className="retry" type="button" onClick={() => void load()}>
              重试
            </button>
          </div>
          <footer className="foot" role="status">
            <span className="foot-status">DDC/CI 本地控制</span>
            <span className="foot-step">读取失败</span>
          </footer>
        </>
      ) : monitors.length === 0 ? (
        <>
          <div className="empty-state">
            <div className="empty-panes" aria-hidden="true">
              <span />
              <span />
              <span />
            </div>
            <p className="empty-title">没有检测到显示器</p>
            <p className="empty-hint">检查视频线连接后重新检测。</p>
            <button className="retry" type="button" onClick={() => void load()}>
              重新检测
            </button>
          </div>
          <footer className="foot" role="status">
            <span className="foot-status">DDC/CI 本地控制</span>
            <span className="foot-step">等待显示器</span>
          </footer>
        </>
      ) : (
        <>
          <LightStage
            monitors={ordered}
            values={values}
            activeId={activeId}
            onSelect={focusMonitor}
            onHover={setHoveredId}
          />

          {master && (
            <section className="master-strip" aria-label="全部显示器">
              <div className="master-copy">
                <span className="master-title">全部显示器</span>
                <span className="master-sub">
                  {master.mixed
                    ? "亮度不一致 · 拖动同步"
                    : `${available.length} 台已同步`}
                </span>
              </div>
              <div className={`master-readout ${master.mixed ? "mixed" : ""}`}>
                <span className="master-value">{master.value}</span>
                <span className="master-unit">%</span>
                {master.mixed && <span className="master-flag">混合</span>}
              </div>
              <Slider
                value={master.value}
                onChange={setAll}
                label="全部显示器亮度"
                step={step}
              />
            </section>
          )}

          <section className="monitor-group" aria-label="显示器列表">
            {ordered.map((monitor, index) => {
              const known = monitor.brightness !== null;
              const value = monitorValue(monitor, values);
              const active = activeId === monitor.id;
              return (
                <article
                  id={`monitor-row-${monitor.id}`}
                  className={`monitor-row ${active ? "is-active" : ""} ${
                    known ? "" : "is-unavailable"
                  }`}
                  key={monitor.id}
                  onMouseEnter={() => setHoveredId(monitor.id)}
                  onMouseLeave={() => setHoveredId(null)}
                >
                  <div className="row-main">
                    <span className="row-index" aria-hidden="true">
                      {String(index + 1).padStart(2, "0")}
                    </span>
                    <MonitorSwatch level={value / 100} stale={!known} />
                    <div className="monitor-title">
                      <span className="monitor-name">
                        {monitorName(monitor)}
                        {monitor.primary && (
                          <span className="primary-tag">主屏</span>
                        )}
                      </span>
                      <span className="monitor-meta">
                        {monitorMeta(monitor) ||
                          (monitor.error ? monitorHint(monitor) : "无响应")}
                      </span>
                    </div>
                    {known ? (
                      <span className={`row-value ${monitor.error ? "stale" : ""}`}>
                        {value}
                        <span className="row-unit">%</span>
                      </span>
                    ) : monitor.lastPercent !== null ? (
                      <span
                        className="row-last"
                        title="上次记录的亮度，当前读取不到"
                      >
                        <span className="row-last-tag">上次</span>
                        <span className="row-last-value">
                          {monitor.lastPercent}
                        </span>
                        <span className="row-last-unit">%</span>
                      </span>
                    ) : null}
                  </div>
                  {known && (
                    <Slider
                      value={value}
                      onChange={(next) => setOne(monitor.id, next)}
                      label={`${monitorName(monitor)} 亮度`}
                      step={step}
                      inputRef={(node) => {
                        if (node) sliderRefs.current.set(monitor.id, node);
                        else sliderRefs.current.delete(monitor.id);
                      }}
                      onFocus={() => setSelectedId(monitor.id)}
                    />
                  )}
                  {monitor.error && (
                    <div
                      className="monitor-error-row"
                      role="status"
                      title={`${monitorHint(monitor)}（DDC/CI 0x${monitor.error.code.toString(16).toUpperCase()}）`}
                    >
                      <span className="monitor-error-dot" aria-hidden="true" />
                      <p className="monitor-error">{monitorHint(monitor)}</p>
                      <button
                        className="inline-retry"
                        type="button"
                        onClick={() => void load()}
                        disabled={refreshing}
                      >
                        重试
                      </button>
                    </div>
                  )}
                </article>
              );
            })}
          </section>

          <footer
            className="foot"
            role="status"
            aria-live="polite"
            title={notes.length > 0 ? notes.join("\n") : undefined}
          >
            <span className="foot-status">DDC/CI 本地控制</span>
            {hotkeyUp && hotkeyDown ? (
              <span className="foot-keys">
                <span className="key-group">
                  {hotkeyUp.map((key) => (
                    <kbd key={`up-${key}`}>{key}</kbd>
                  ))}
                </span>
                <span className="key-pair">调光</span>
                <span className="key-group">
                  {hotkeyDown.map((key) => (
                    <kbd key={`down-${key}`}>{key}</kbd>
                  ))}
                </span>
              </span>
            ) : (
              <span className="foot-step">步进 {step}%</span>
            )}
          </footer>
        </>
      )}
    </main>
  );
}

export default App;
