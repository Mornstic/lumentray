import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import type { CSSProperties, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { BrandMark } from "./BrandMark";
import "./Settings.css";

type ThemeChoice = "system" | "light" | "dark";

type HotkeyStatus = {
  action: string;
  accelerator: string;
  registered: boolean;
  error: string | null;
};

type SettingsSnapshot = {
  theme: ThemeChoice;
  glassOpacity: number;
  launchAtLogin: boolean;
  restoreOnWake: boolean;
  step: number;
  hotkeysEnabled: boolean;
  hotkeyIncrease: string;
  hotkeyDecrease: string;
  hotkeyStep: number;
  hotkeyStatus: HotkeyStatus[];
  monitorCount: number;
  settingsPath: string;
  version: number;
  appVersion: string;
};

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

type DiagnosticsInfo = {
  appVersion: string;
  exePath: string;
  settingsPath: string;
  logPath: string;
  hotkeys: HotkeyStatus[];
  monitorCount: number;
  logTail: string[];
};

type Page = "general" | "hotkeys" | "monitors" | "diagnostics" | "about";
type SaveStatus = "idle" | "saving" | "saved" | "error";

function setGlassAlpha(percent: number) {
  const clamped = Math.min(100, Math.max(85, Math.round(percent)));
  document.documentElement.style.setProperty(
    "--glass-alpha",
    String(clamped / 100),
  );
}

const PAGE_META: Record<
  Page,
  { title: string; hint: string; eyebrow: string; index: string }
> = {
  general: {
    title: "通用",
    hint: "启动、外观与键盘调节。",
    eyebrow: "General",
    index: "01",
  },
  hotkeys: {
    title: "快捷键",
    hint: "在任何窗口下调整所有显示器的亮度。",
    eyebrow: "Shortcuts",
    index: "02",
  },
  monitors: {
    title: "显示器",
    hint: "当前连接的显示器与 DDC/CI 状态。",
    eyebrow: "Displays",
    index: "03",
  },
  diagnostics: {
    title: "诊断",
    hint: "环境、快捷键注册与最近的运行日志。",
    eyebrow: "Diagnostics",
    index: "04",
  },
  about: {
    title: "关于",
    hint: "版本、配置位置与数据边界。",
    eyebrow: "About",
    index: "05",
  },
};

function CloseIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6 6l12 12M18 6L6 18" />
    </svg>
  );
}

function MonitorIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <rect x="3" y="4.5" width="18" height="12" rx="1.8" />
      <path d="M9.2 20h5.6M12 16.5V20" />
    </svg>
  );
}

function RefreshGlyph() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M19 12a7 7 0 1 1-2-4.9" />
      <path d="M19.4 5.2v3.6h-3.6" />
    </svg>
  );
}

function CopyGlyph() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <rect x="9" y="9" width="11" height="11" rx="2" />
      <path d="M15 5.5A1.5 1.5 0 0 0 13.5 4h-8A1.5 1.5 0 0 0 4 5.5v8A1.5 1.5 0 0 0 5.5 15" />
    </svg>
  );
}

function FolderGlyph() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M3.5 7.5A1.5 1.5 0 0 1 5 6h4l2 2h8a1.5 1.5 0 0 1 1.5 1.5v8A1.5 1.5 0 0 1 19 19H5a1.5 1.5 0 0 1-1.5-1.5v-10Z" />
    </svg>
  );
}

function ShieldGlyph() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 3.5 19 6v5.4c0 4-2.8 7.4-7 8.6-4.2-1.2-7-4.6-7-8.6V6l7-2.5Z" />
      <path d="m9.2 11.8 2 2 3.6-3.8" />
    </svg>
  );
}

type PillTone = "ok" | "warn" | "danger" | "muted";

function StatusPill({
  tone,
  label,
  title,
}: {
  tone: PillTone;
  label: string;
  title?: string;
}) {
  return (
    <span className={`status-pill ${tone}`} title={title}>
      <span className="status-pill-dot" aria-hidden="true" />
      {label}
    </span>
  );
}

function ActionButton({
  onClick,
  icon,
  children,
  disabled = false,
  busy = false,
}: {
  onClick: () => void;
  icon?: ReactNode;
  children: ReactNode;
  disabled?: boolean;
  busy?: boolean;
}) {
  return (
    <button
      className={`action-button ${busy ? "is-busy" : ""}`}
      type="button"
      onClick={onClick}
      disabled={disabled}
    >
      {icon}
      <span>{children}</span>
    </button>
  );
}

const DDC_HINTS: Record<number, string> = {
  0xc0262580: "DDC/CI 未启用",
  0xc0262581: "显示器不在线",
  0xc0262582: "控制通道无响应",
  0xc0262583: "读取无响应",
  0xc0262584: "不支持 DDC/CI 调光",
  0xc026258d: "显示器已断开",
};

function ddcHint(code: number) {
  return DDC_HINTS[code] ?? "DDC/CI 无响应";
}

function KeyCombo({ value }: { value: string }) {
  return (
    <span className="key-combo">
      {value.split("+").map((key) => (
        <kbd key={key}>{key}</kbd>
      ))}
    </span>
  );
}

type NavIconName = "general" | "hotkeys" | "monitors" | "diagnostics" | "about";

const NAV_ITEMS: { page: Page; label: string; icon: NavIconName }[] = [
  { page: "general", label: "通用", icon: "general" },
  { page: "hotkeys", label: "快捷键", icon: "hotkeys" },
  { page: "monitors", label: "显示器", icon: "monitors" },
  { page: "diagnostics", label: "诊断", icon: "diagnostics" },
  { page: "about", label: "关于", icon: "about" },
];

function NavIcon({ name }: { name: NavIconName }) {
  if (name === "general") {
    return (
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M4 7h9M19 7h1M4 17h1M11 17h9" />
        <circle cx="16" cy="7" r="2.2" />
        <circle cx="8" cy="17" r="2.2" />
      </svg>
    );
  }
  if (name === "hotkeys") {
    return (
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <rect x="3" y="6" width="18" height="12" rx="2" />
        <path d="M7 10h.01M11 10h.01M15 10h.01M7 14h6M15.5 14h1.5" />
      </svg>
    );
  }
  if (name === "monitors") {
    return <MonitorIcon />;
  }
  if (name === "diagnostics") {
    return (
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M3 12h3.5L9 6l4 12 2.5-6H21" />
      </svg>
    );
  }
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 11v5.5M12 7.8h.01" />
    </svg>
  );
}

function Toggle({
  checked,
  onChange,
  label,
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: string;
}) {
  return (
    <button
      className={`toggle ${checked ? "on" : ""}`}
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      onClick={() => onChange(!checked)}
    >
      <span className="toggle-knob" />
    </button>
  );
}

function Segmented<T extends string | number>({
  value,
  options,
  onChange,
  label,
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (value: T) => void;
  label: string;
}) {
  const trackRef = useRef<HTMLDivElement>(null);
  const [thumb, setThumb] = useState<{
    x: number;
    y: number;
    w: number;
    h: number;
  } | null>(null);
  const activeIndex = options.findIndex((option) => option.value === value);

  useLayoutEffect(() => {
    const track = trackRef.current;
    if (!track) return;
    const measure = () => {
      const button =
        track.querySelectorAll<HTMLButtonElement>(".segment")[activeIndex];
      if (!button) return;
      setThumb({
        x: button.offsetLeft,
        y: button.offsetTop,
        w: button.offsetWidth,
        h: button.offsetHeight,
      });
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(track);
    void document.fonts.ready.then(measure);
    return () => observer.disconnect();
  }, [activeIndex]);

  return (
    <div
      className="segmented"
      ref={trackRef}
      role="group"
      aria-label={label}
      style={
        thumb
          ? ({
              "--seg-x": `${thumb.x}px`,
              "--seg-y": `${thumb.y}px`,
              "--seg-w": `${thumb.w}px`,
              "--seg-h": `${thumb.h}px`,
            } as CSSProperties)
          : undefined
      }
    >
      <span className="segment-thumb" aria-hidden="true" />
      {options.map((option) => (
        <button
          key={String(option.value)}
          className={`segment ${option.value === value ? "active" : ""}`}
          type="button"
          aria-pressed={option.value === value}
          onClick={() => onChange(option.value)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

function MonitorMini({
  level,
  stale = false,
}: {
  level: number;
  stale?: boolean;
}) {
  return (
    <span
      className={`mini-screen ${stale ? "is-stale" : ""}`}
      style={{ "--level": level } as CSSProperties}
      aria-hidden="true"
    >
      <span className="mini-fill" />
      <span className="mini-sheen" />
    </span>
  );
}

const MODIFIER_KEYS = new Set(["Control", "Alt", "Shift", "Meta"]);

function acceleratorKey(code: string): string | null {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  const named: Record<string, string> = {
    ArrowUp: "Up",
    ArrowDown: "Down",
    ArrowLeft: "Left",
    ArrowRight: "Right",
    Home: "Home",
    End: "End",
    PageUp: "PageUp",
    PageDown: "PageDown",
    Insert: "Insert",
    Delete: "Delete",
    Space: "Space",
    Tab: "Tab",
  };
  return named[code] ?? null;
}

function HotkeyRecorder({
  value,
  onChange,
  disabled = false,
}: {
  value: string;
  onChange: (value: string) => void;
  disabled?: boolean;
}) {
  const [recording, setRecording] = useState(false);
  const [hint, setHint] = useState<string | null>(null);
  const recordingRef = useRef(false);

  const stop = useCallback((resume: boolean) => {
    recordingRef.current = false;
    setRecording(false);
    setHint(null);
    if (resume) {
      void invoke("set_hotkey_recording", { recording: false }).catch(() => {});
    }
  }, []);

  useEffect(
    () => () => {
      if (recordingRef.current) {
        void invoke("set_hotkey_recording", { recording: false }).catch(() => {});
      }
    },
    [],
  );

  useEffect(() => {
    if (!recording) return;
    const onKeyDown = (event: KeyboardEvent) => {
      event.preventDefault();
      event.stopPropagation();
      if (event.key === "Escape") {
        stop(true);
        return;
      }
      if (MODIFIER_KEYS.has(event.key)) {
        setHint("继续按一个主键（字母、数字、F1–F24 或方向键）");
        return;
      }
      const key = acceleratorKey(event.code);
      if (!key) {
        setHint("这个键不能用作快捷键，请换一个");
        return;
      }
      const parts: string[] = [];
      if (event.ctrlKey) parts.push("Ctrl");
      if (event.altKey) parts.push("Alt");
      if (event.shiftKey) parts.push("Shift");
      if (event.metaKey) parts.push("Win");
      if (parts.length === 0) {
        setHint("至少需要一个修饰键（Ctrl/Alt/Shift/Win）");
        return;
      }
      parts.push(key);
      stop(true);
      onChange(parts.join("+"));
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [recording, onChange, stop]);

  const begin = () => {
    if (disabled || recording) return;
    recordingRef.current = true;
    setRecording(true);
    setHint("请按下新的组合键，Esc 取消");
    void invoke("set_hotkey_recording", { recording: true }).catch(() => {});
  };

  return (
    <div className="hotkey-control">
      <button
        className={`hotkey-button ${recording ? "recording" : ""}`}
        type="button"
        disabled={disabled}
        aria-label="录制快捷键"
        onClick={begin}
      >
        {recording ? (
          "按下组合键…"
        ) : (
          <span className="hotkey-keys">
            {value.split("+").map((key) => (
              <kbd key={key}>{key}</kbd>
            ))}
          </span>
        )}
      </button>
      {hint && <span className="hotkey-hint">{hint}</span>}
    </div>
  );
}

function HotkeyState({
  status,
  enabled,
}: {
  status: HotkeyStatus | undefined;
  enabled: boolean;
}) {
  if (!enabled) return null;
  if (status?.error) {
    return (
      <StatusPill
        tone="danger"
        label="注册失败"
        title={status.error}
      />
    );
  }
  if (status?.registered) return <StatusPill tone="ok" label="已注册" />;
  return <StatusPill tone="muted" label="等待注册" />;
}

function PathText({ value }: { value: string }) {
  const parts = value.split(/([\\#])/);
  return (
    <span className="about-path">
      {parts.map((part, index) => (
        <span key={index}>
          {part}
          {(part === "\\" || part === "#") && <wbr />}
        </span>
      ))}
    </span>
  );
}

function monitorName(monitor: Monitor) {
  return monitor.name || monitor.description || monitor.gdiName;
}

function orderMonitors(monitors: Monitor[]) {
  return [...monitors].sort(
    (a, b) => (a.x ?? 0) - (b.x ?? 0) || (a.y ?? 0) - (b.y ?? 0),
  );
}

function monitorResolution(monitor: Monitor) {
  return monitor.width && monitor.height
    ? `${monitor.width}×${monitor.height}`
    : null;
}

function methodLabel(brightness: Brightness | null) {
  if (!brightness) return "";
  return brightness.method === "high_level" ? "高层 API" : "VCP 0x10";
}

function Settings() {
  const [snapshot, setSnapshot] = useState<SettingsSnapshot | null>(null);
  const [page, setPage] = useState<Page>("general");
  const [status, setStatus] = useState<SaveStatus>("idle");
  const [error, setError] = useState<string | null>(null);
  const [monitors, setMonitors] = useState<Monitor[] | null>(null);
  const [monitorsLoading, setMonitorsLoading] = useState(false);
  const [monitorsError, setMonitorsError] = useState<string | null>(null);
  const [diagnostics, setDiagnostics] = useState<DiagnosticsInfo | null>(null);
  const [diagLoading, setDiagLoading] = useState(false);
  const [diagError, setDiagError] = useState<string | null>(null);
  const [copyState, setCopyState] = useState<string | null>(null);
  const [glassDraft, setGlassDraft] = useState<number | null>(null);

  useEffect(() => {
    document.title = "LumenTray 设置";
  }, []);

  const snapshotRef = useRef<SettingsSnapshot | null>(null);
  const saveChain = useRef<Promise<void>>(Promise.resolve());
  const saveVersion = useRef(0);
  const glassDraftRef = useRef<number | null>(null);

  const load = useCallback(async () => {
    try {
      const next = await invoke<SettingsSnapshot>("get_settings");
      snapshotRef.current = next;
      setSnapshot(next);
      setGlassAlpha(next.glassOpacity);
      setError(null);
      setStatus("idle");
    } catch (loadError) {
      setStatus("error");
      setError(String(loadError));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    const subscription = listen<HotkeyStatus[]>(
      "hotkeys-changed",
      (event) => {
        const statuses = event.payload ?? [];
        if (snapshotRef.current) {
          snapshotRef.current = {
            ...snapshotRef.current,
            hotkeyStatus: statuses,
          };
        }
        setSnapshot((current) =>
          current ? { ...current, hotkeyStatus: statuses } : current,
        );
      },
    );
    return () => {
      void subscription.then((off) => off());
    };
  }, []);

  const apply = (
    patch: Partial<
      Pick<
        SettingsSnapshot,
        | "theme"
        | "glassOpacity"
        | "launchAtLogin"
        | "restoreOnWake"
        | "step"
        | "hotkeysEnabled"
        | "hotkeyIncrease"
        | "hotkeyDecrease"
        | "hotkeyStep"
      >
    >,
  ) => {
    const current = snapshotRef.current;
    if (!current) return;
    const next = { ...current, ...patch };
    snapshotRef.current = next;
    setSnapshot(next);
    setStatus("saving");
    setError(null);
    setGlassAlpha(next.glassOpacity);
    const payload = {
      theme: next.theme,
      glassOpacity: next.glassOpacity,
      launchAtLogin: next.launchAtLogin,
      restoreOnWake: next.restoreOnWake,
      step: next.step,
      hotkeysEnabled: next.hotkeysEnabled,
      hotkeyIncrease: next.hotkeyIncrease,
      hotkeyDecrease: next.hotkeyDecrease,
      hotkeyStep: next.hotkeyStep,
    };
    const mine = ++saveVersion.current;
    saveChain.current = saveChain.current
      .catch(() => {})
      .then(async () => {
        const saved = await invoke<SettingsSnapshot>("save_settings", {
          patch: payload,
        });
        if (mine !== saveVersion.current) return;
        // 后端可能规范化按键名，也可能带回最新的注册/冲突状态
        snapshotRef.current = saved;
        setSnapshot(saved);
        setStatus("saved");
      })
      .catch((saveError: unknown) => {
        if (mine !== saveVersion.current) return;
        setStatus("error");
        setError(String(saveError));
        // 保留错误提示，同时把本地状态对齐到磁盘上的设置
        void invoke<SettingsSnapshot>("get_settings")
          .then((authoritative) => {
            snapshotRef.current = authoritative;
            setSnapshot(authoritative);
          })
          .catch(() => {});
      });
  };

  const previewGlass = (value: number) => {
    glassDraftRef.current = value;
    setGlassDraft(value);
    setGlassAlpha(value);
  };

  const commitGlass = () => {
    const value = glassDraftRef.current;
    if (value === null) return;
    glassDraftRef.current = null;
    setGlassDraft(null);
    if (snapshotRef.current?.glassOpacity === value) return;
    apply({ glassOpacity: value });
  };

  const loadMonitors = useCallback(async () => {
    setMonitorsLoading(true);
    setMonitorsError(null);
    try {
      const list = await invoke<MonitorList>("list_monitors");
      setMonitors(list.monitors);
    } catch (loadError) {
      setMonitorsError(String(loadError));
    } finally {
      setMonitorsLoading(false);
    }
  }, []);

  const loadDiagnostics = useCallback(async () => {
    setDiagLoading(true);
    setDiagError(null);
    try {
      setDiagnostics(await invoke<DiagnosticsInfo>("get_diagnostics"));
    } catch (loadError) {
      setDiagError(String(loadError));
    } finally {
      setDiagLoading(false);
    }
  }, []);

  useEffect(() => {
    if ((page === "monitors" || page === "diagnostics") && monitors === null) {
      void loadMonitors();
    }
  }, [page, monitors, loadMonitors]);

  useEffect(() => {
    if (page === "diagnostics") {
      void loadDiagnostics();
    }
  }, [page, loadDiagnostics]);

  useEffect(() => {
    if (page !== "monitors" && page !== "diagnostics") return;
    const subscription = listen("monitors-changed", () => {
      void loadMonitors();
    });
    return () => {
      void subscription.then((off) => off());
    };
  }, [page, loadMonitors]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        void invoke("close_settings").catch(() => {});
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  useEffect(() => {
    let frame = requestAnimationFrame(() => {
      frame = requestAnimationFrame(() => {
        const start = () => {
          requestAnimationFrame(() => {
            document.documentElement.dataset.settings = "in";
          });
        };
        void invoke("reveal_settings").then(start, start);
      });
    });
    return () => cancelAnimationFrame(frame);
  }, []);

  const copyDiagnostics = async () => {
    const lines: string[] = [];
    lines.push("LumenTray 诊断报告");
    lines.push(`时间: ${new Date().toLocaleString()}`);
    lines.push(`版本: ${diagnostics?.appVersion ?? snapshot?.appVersion ?? "未知"}`);
    lines.push(`可执行文件: ${diagnostics?.exePath ?? "-"}`);
    lines.push(`配置文件: ${diagnostics?.settingsPath ?? snapshot?.settingsPath ?? "-"}`);
    lines.push(`日志文件: ${diagnostics?.logPath ?? "-"}`);
    if (snapshot) {
      lines.push(
        `快捷键: ${snapshot.hotkeysEnabled ? "启用" : "停用"} ${snapshot.hotkeyIncrease} / ${snapshot.hotkeyDecrease}，每次 ${snapshot.hotkeyStep}%`,
      );
      for (const hotkey of snapshot.hotkeyStatus) {
        lines.push(
          `  - ${hotkey.action}: ${hotkey.accelerator} ${
            hotkey.registered ? "已注册" : "未注册"
          }${hotkey.error ? `（${hotkey.error}）` : ""}`,
        );
      }
    }
    lines.push(`显示器记录: ${diagnostics?.monitorCount ?? snapshot?.monitorCount ?? 0} 台`);
    lines.push("");
    lines.push(`当前显示器 (${monitors?.length ?? 0}):`);
    for (const monitor of monitors ?? []) {
      const state = monitor.error
        ? `错误 0x${monitor.error.code.toString(16).toUpperCase()} ${monitor.error.context}`
        : monitor.brightness
          ? `${monitor.brightness.percent}% (raw ${monitor.brightness.raw}, ${monitor.brightness.min}-${monitor.brightness.max}, ${methodLabel(monitor.brightness)})`
          : "未知";
      lines.push(
        `- ${monitorName(monitor)} | ${monitor.id} | ${
          monitor.connection || monitor.gdiName
        } | ${state}`,
      );
    }
    lines.push("");
    lines.push("最近日志:");
    lines.push(...(diagnostics?.logTail ?? []));
    try {
      await invoke("copy_text", { text: lines.join("\n") });
      setCopyState("已复制");
    } catch (copyError) {
      setCopyState(`复制失败: ${String(copyError)}`);
    }
    window.setTimeout(() => setCopyState(null), 2400);
  };

  const statusText =
    status === "saving"
      ? "正在保存…"
      : status === "error"
        ? "保存失败"
        : status === "saved"
          ? "已保存"
          : "";

  const glassValue = glassDraft ?? snapshot?.glassOpacity ?? 95;
  const glassFill = ((glassValue - 85) / 0.15).toFixed(1);

  return (
    <main className="settings">
      <header className="settings-titlebar" data-tauri-drag-region>
        <span className="settings-brand" data-tauri-drag-region>
          <span className="brand-mark" data-tauri-drag-region>
            <BrandMark />
          </span>
          <span className="brand-name" data-tauri-drag-region>
            LumenTray
          </span>
        </span>
        <span className="titlebar-rule" data-tauri-drag-region aria-hidden="true" />
        <h1 data-tauri-drag-region>设置</h1>
        <button
          className="settings-close"
          type="button"
          title="关闭"
          aria-label="关闭设置"
          onClick={() => void invoke("close_settings").catch(() => {})}
        >
          <CloseIcon />
        </button>
      </header>

      <div className="settings-body">
        <nav className="settings-nav" aria-label="设置分类">
          {NAV_ITEMS.map((item) => (
            <button
              key={item.page}
              className={`nav-item ${page === item.page ? "active" : ""}`}
              type="button"
              aria-current={page === item.page ? "page" : undefined}
              onClick={() => setPage(item.page)}
            >
              <NavIcon name={item.icon} />
              <span>{item.label}</span>
            </button>
          ))}
          <div className="nav-footer">
            <span className={`save-state ${status}`} role="status">
              {statusText}
            </span>
            {snapshot && <span className="nav-version">v{snapshot.appVersion}</span>}
          </div>
        </nav>

        <section className="settings-content" key={page}>
          <header className="page-head">
            <div className="page-title-row">
              <h2 className="page-title">{PAGE_META[page].title}</h2>
              <div className="page-meta">
                <span className="page-eyebrow">{PAGE_META[page].eyebrow}</span>
                <span className="page-index">
                  {PAGE_META[page].index} / 05
                </span>
              </div>
            </div>
            <p className="page-hint">{PAGE_META[page].hint}</p>
          </header>

          {error && (
            <div className="settings-banner" role="alert">
              {error}
            </div>
          )}

          {!snapshot ? (
            <div className="settings-loading">
              <span className="spinner" aria-hidden="true" />
              <span>读取设置…</span>
            </div>
          ) : page === "general" ? (
            <>
              <div className="group">
                <h2>启动</h2>
                <div className="card">
                  <div className="setting-row">
                    <div className="setting-copy">
                      <span className="setting-title">开机启动</span>
                      <span className="setting-desc">
                        登录 Windows 后自动在托盘运行
                      </span>
                    </div>
                    <Toggle
                      checked={snapshot.launchAtLogin}
                      onChange={(launchAtLogin) => apply({ launchAtLogin })}
                      label="开机启动"
                    />
                  </div>
                  <div className="row-divider" />
                  <div className="setting-row">
                    <div className="setting-copy">
                      <span className="setting-title">恢复上次亮度</span>
                      <span className="setting-desc">
                        {snapshot.monitorCount > 0
                          ? `启动和唤醒时恢复每台显示器的亮度（已记录 ${snapshot.monitorCount} 台）`
                          : "启动和唤醒时恢复每台显示器的亮度"}
                      </span>
                    </div>
                    <Toggle
                      checked={snapshot.restoreOnWake}
                      onChange={(restoreOnWake) => apply({ restoreOnWake })}
                      label="恢复上次亮度"
                    />
                  </div>
                </div>
              </div>

              <div className="group">
                <h2>外观</h2>
                <div className="card">
                  <div className="setting-row">
                    <div className="setting-copy">
                      <span className="setting-title">主题</span>
                      <span className="setting-desc">
                        跟随系统或固定浅色、深色
                      </span>
                    </div>
                    <Segmented
                      value={snapshot.theme}
                      options={[
                        { value: "system", label: "跟随系统" },
                        { value: "light", label: "浅色" },
                        { value: "dark", label: "深色" },
                      ]}
                      onChange={(theme) => apply({ theme })}
                      label="主题"
                    />
                  </div>
                  <div className="row-divider" />
                  <div className="setting-row">
                    <div className="setting-copy">
                      <span className="setting-title">玻璃不透明度</span>
                      <span className="setting-desc">
                        玻璃底色浓度，越低越透
                      </span>
                    </div>
                    <div className="glass-control">
                      <input
                        className="glass-slider"
                        type="range"
                        min={85}
                        max={100}
                        step={1}
                        value={glassValue}
                        aria-label="玻璃不透明度"
                        aria-valuetext={`${glassValue}%`}
                        style={{ "--fill": `${glassFill}%` } as CSSProperties}
                        onChange={(event) =>
                          previewGlass(Number(event.target.value))
                        }
                        onPointerUp={commitGlass}
                        onKeyUp={commitGlass}
                        onBlur={commitGlass}
                      />
                      <span className="glass-readout">
                        <span className="glass-readout-value">
                          {glassValue}
                        </span>
                        <span className="glass-readout-unit">%</span>
                      </span>
                    </div>
                  </div>
                </div>
              </div>

              <div className="group">
                <h2>调节</h2>
                <div className="card">
                  <div className="setting-row">
                    <div className="setting-copy">
                      <span className="setting-title">键盘步长</span>
                      <span className="setting-desc">
                        方向键按步长调节，PgUp/PgDn 固定 10%
                      </span>
                    </div>
                    <Segmented
                      value={snapshot.step}
                      options={[
                        { value: 1, label: "1%" },
                        { value: 5, label: "5%" },
                        { value: 10, label: "10%" },
                      ]}
                      onChange={(step) => apply({ step })}
                      label="键盘步长"
                    />
                  </div>
                </div>
              </div>
            </>
          ) : page === "hotkeys" ? (
            <>
              <div className="group">
                <h2>全局快捷键</h2>
                <div className="card">
                  <div className="setting-row">
                    <div className="setting-copy">
                      <span className="setting-title">启用全局快捷键</span>
                      <span className="setting-desc">
                        在任何窗口下同时调整所有可控制显示器的亮度
                      </span>
                    </div>
                    <Toggle
                      checked={snapshot.hotkeysEnabled}
                      onChange={(hotkeysEnabled) => apply({ hotkeysEnabled })}
                      label="启用全局快捷键"
                    />
                  </div>
                  <div className="row-divider" />
                  <div className="setting-row hotkey-row">
                    <div className="setting-copy">
                      <span className="setting-title">调高亮度</span>
                      <span className="setting-desc">
                        把所有可控制显示器的亮度调高
                      </span>
                    </div>
                    <div className="hotkey-slot">
                      <HotkeyRecorder
                        value={snapshot.hotkeyIncrease}
                        disabled={!snapshot.hotkeysEnabled}
                        onChange={(hotkeyIncrease) => apply({ hotkeyIncrease })}
                      />
                      <HotkeyState
                        status={snapshot.hotkeyStatus.find(
                          (item) => item.action === "increase",
                        )}
                        enabled={snapshot.hotkeysEnabled}
                      />
                    </div>
                  </div>
                  <div className="row-divider" />
                  <div className="setting-row hotkey-row">
                    <div className="setting-copy">
                      <span className="setting-title">调低亮度</span>
                      <span className="setting-desc">
                        把所有可控制显示器的亮度调低
                      </span>
                    </div>
                    <div className="hotkey-slot">
                      <HotkeyRecorder
                        value={snapshot.hotkeyDecrease}
                        disabled={!snapshot.hotkeysEnabled}
                        onChange={(hotkeyDecrease) => apply({ hotkeyDecrease })}
                      />
                      <HotkeyState
                        status={snapshot.hotkeyStatus.find(
                          (item) => item.action === "decrease",
                        )}
                        enabled={snapshot.hotkeysEnabled}
                      />
                    </div>
                  </div>
                </div>
                <p className="group-note">
                  录制时会暂时停用现有快捷键，Esc 取消。组合键被其他程序占用时会显示原因。
                </p>
              </div>

              <div className="group">
                <h2>调整</h2>
                <div className="card">
                  <div className="setting-row">
                    <div className="setting-copy">
                      <span className="setting-title">每次调整</span>
                      <span className="setting-desc">
                        单次调整幅度，按住连发合并写入
                      </span>
                    </div>
                    <Segmented
                      value={snapshot.hotkeyStep}
                      options={[
                        { value: 5, label: "5%" },
                        { value: 10, label: "10%" },
                        { value: 15, label: "15%" },
                        { value: 20, label: "20%" },
                      ]}
                      onChange={(hotkeyStep) => apply({ hotkeyStep })}
                      label="快捷键调整幅度"
                    />
                  </div>
                </div>
                <p className="group-note">
                  快捷键只调整当前可读写亮度的显示器；无响应的显示器会被跳过并写入日志。
                </p>
              </div>
            </>
          ) : page === "monitors" ? (
            <div className="group">
              <div className="group-head">
                <h2>显示器</h2>
                <ActionButton
                  icon={<RefreshGlyph />}
                  busy={monitorsLoading}
                  disabled={monitorsLoading}
                  onClick={() => void loadMonitors()}
                >
                  {monitorsLoading ? "检测中…" : "重新检测"}
                </ActionButton>
              </div>
              {monitorsError ? (
                <div className="settings-banner" role="alert">
                  {monitorsError}
                </div>
              ) : monitors === null ? (
                <div className="card settings-empty">
                  <span className="spinner" aria-hidden="true" />
                  <span>正在检测显示器…</span>
                </div>
              ) : monitors.length === 0 ? (
                <div className="card empty-block">
                  <span className="empty-icon">
                    <MonitorIcon />
                  </span>
                  <span className="empty-title">没有检测到显示器</span>
                  <span className="empty-hint">
                    检查视频线连接，或确认显示器已开机。
                  </span>
                  <ActionButton
                    icon={<RefreshGlyph />}
                    onClick={() => void loadMonitors()}
                  >
                    重新检测
                  </ActionButton>
                </div>
              ) : (
                <div className="card">
                  {orderMonitors(monitors).map((monitor, index) => {
                    const level = monitor.brightness?.percent ?? null;
                    const last = monitor.lastPercent;
                    return (
                      <div key={monitor.id}>
                        {index > 0 && <div className="row-divider" />}
                        <div className="monitor-setting">
                          <MonitorMini
                            level={level !== null ? level / 100 : (last ?? 0) / 100}
                            stale={level === null}
                          />
                          <div className="monitor-setting-copy">
                            <span className="monitor-setting-name">
                              {monitorName(monitor)}
                              {monitor.primary && (
                                <span className="settings-primary-tag">主屏</span>
                              )}
                            </span>
                            <span className="monitor-setting-meta">
                              {[
                                monitor.connection || monitor.gdiName,
                                monitorResolution(monitor),
                              ]
                                .filter(Boolean)
                                .join(" · ")}
                            </span>
                            {monitor.error && (
                              <span className="monitor-setting-hint">
                                {ddcHint(monitor.error.code)}
                              </span>
                            )}
                          </div>
                          <div className="monitor-setting-side">
                            {level !== null ? (
                              monitor.error ? (
                                <StatusPill tone="warn" label="写入异常" />
                              ) : (
                                <StatusPill tone="ok" label="可调" />
                              )
                            ) : (
                              <StatusPill
                                tone="warn"
                                label="不可读"
                                title={
                                  monitor.error
                                    ? `${ddcHint(monitor.error.code)} · DDC/CI 0x${monitor.error.code
                                        .toString(16)
                                        .toUpperCase()}`
                                    : undefined
                                }
                              />
                            )}
                            {level !== null ? (
                              <span className="monitor-setting-value">
                                {level}
                                <span className="monitor-setting-unit">%</span>
                              </span>
                            ) : last !== null ? (
                              <span className="monitor-setting-last">
                                <span className="monitor-setting-last-tag">
                                  上次
                                </span>
                                <span className="monitor-setting-last-value">
                                  {last}
                                </span>
                                <span className="monitor-setting-last-unit">
                                  %
                                </span>
                              </span>
                            ) : null}
                          </div>
                        </div>
                      </div>
                    );
                  })}
                </div>
              )}
              <p className="group-note">
                亮度记录按显示器设备路径保存在本机，换接口后可能重新记录。
              </p>
            </div>
          ) : page === "diagnostics" ? (
            <>
              <div className="group">
                <div className="group-head">
                  <h2>环境</h2>
                  <div className="diag-actions">
                    <ActionButton
                      icon={<RefreshGlyph />}
                      busy={diagLoading}
                      disabled={diagLoading || monitorsLoading}
                      onClick={() => {
                        void loadDiagnostics();
                        void loadMonitors();
                      }}
                    >
                      {diagLoading ? "刷新中…" : "刷新"}
                    </ActionButton>
                    <ActionButton
                      icon={<CopyGlyph />}
                      onClick={() => void copyDiagnostics()}
                    >
                      {copyState ?? "复制诊断报告"}
                    </ActionButton>
                  </div>
                </div>
                {diagError && (
                  <div className="settings-banner" role="alert">
                    {diagError}
                  </div>
                )}
                <div className="card">
                  {diagnostics ? (
                    <>
                      <div className="about-row">
                        <span>版本</span>
                        <span className="about-value">v{diagnostics.appVersion}</span>
                      </div>
                      <div className="row-divider" />
                      <div className="about-row">
                        <span>可执行文件</span>
                        <PathText value={diagnostics.exePath} />
                      </div>
                      <div className="row-divider" />
                      <div className="about-row">
                        <span>配置文件</span>
                        <PathText value={diagnostics.settingsPath} />
                      </div>
                      <div className="row-divider" />
                      <div className="about-row">
                        <span>日志文件</span>
                        <PathText value={diagnostics.logPath} />
                      </div>
                    </>
                  ) : (
                    <div className="settings-loading">
                      <span className="spinner" aria-hidden="true" />
                      <span>读取诊断信息…</span>
                    </div>
                  )}
                </div>
              </div>

              <div className="group">
                <h2>快捷键</h2>
                <div className="card diag-list">
                  {diagnostics && diagnostics.hotkeys.length > 0 ? (
                    diagnostics.hotkeys.map((hotkey) => (
                      <div className="diag-line" key={hotkey.action}>
                        <span className="diag-key">
                          {hotkey.action === "increase" ? "调高亮度" : "调低亮度"}
                        </span>
                        <KeyCombo value={hotkey.accelerator} />
                        <span className="diag-state">
                          {hotkey.error ? (
                            <StatusPill
                              tone="danger"
                              label="注册失败"
                              title={hotkey.error}
                            />
                          ) : hotkey.registered ? (
                            <StatusPill tone="ok" label="已注册" />
                          ) : (
                            <StatusPill tone="muted" label="未注册" />
                          )}
                        </span>
                      </div>
                    ))
                  ) : (
                    <div className="settings-empty">快捷键未启用</div>
                  )}
                </div>
              </div>

              <div className="group">
                <div className="group-head">
                  <h2>显示器 ({monitors?.length ?? 0})</h2>
                  <ActionButton
                    icon={<RefreshGlyph />}
                    busy={monitorsLoading}
                    disabled={monitorsLoading}
                    onClick={() => void loadMonitors()}
                  >
                    {monitorsLoading ? "检测中…" : "重新检测"}
                  </ActionButton>
                </div>
                {monitorsError && (
                  <div className="settings-banner" role="alert">
                    {monitorsError}
                  </div>
                )}
                {monitors === null ? (
                  <div className="card settings-empty">
                    <span className="spinner" aria-hidden="true" />
                    <span>正在检测显示器…</span>
                  </div>
                ) : monitors.length === 0 ? (
                  <div className="card settings-empty">
                    没有检测到可控制的显示器。
                  </div>
                ) : (
                  <div className="card">
                    {orderMonitors(monitors).map((monitor, index) => {
                      const level = monitor.brightness?.percent ?? null;
                      const resolution = monitorResolution(monitor);
                      return (
                        <div key={monitor.id}>
                          {index > 0 && <div className="row-divider" />}
                          <div className="diag-monitor">
                            <div className="diag-monitor-head">
                              <span className="diag-monitor-name">
                                {monitorName(monitor)}
                                {monitor.primary && (
                                  <span className="settings-primary-tag">主屏</span>
                                )}
                              </span>
                              {monitor.error ? (
                                <StatusPill
                                  tone="warn"
                                  label="不可读"
                                  title={`DDC/CI 0x${monitor.error.code
                                    .toString(16)
                                    .toUpperCase()}`}
                                />
                              ) : (
                                <StatusPill tone="ok" label="正常" />
                              )}
                            </div>
                            <dl className="diag-facts">
                              <div>
                                <dt>设备</dt>
                                <dd>{monitor.devicePath || monitor.gdiName}</dd>
                              </div>
                              <div>
                                <dt>接口</dt>
                                <dd>
                                  {[monitor.connection || monitor.gdiName, resolution]
                                    .filter(Boolean)
                                    .join(" · ")}
                                </dd>
                              </div>
                              <div>
                                <dt>控制</dt>
                                <dd>
                                  {level !== null
                                    ? `${level}% · ${methodLabel(monitor.brightness)}`
                                    : "—"}
                                </dd>
                              </div>
                              {monitor.error && (
                                <div className="diag-error">
                                  <dt>错误</dt>
                                  <dd title={monitor.error.context}>
                                    {ddcHint(monitor.error.code)} · 0x
                                    {monitor.error.code
                                      .toString(16)
                                      .toUpperCase()}
                                  </dd>
                                </div>
                              )}
                              {monitor.lastPercent !== null && (
                                <div>
                                  <dt>上次</dt>
                                  <dd>{monitor.lastPercent}%</dd>
                                </div>
                              )}
                            </dl>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>

              <div className="group">
                <div className="group-head">
                  <h2>最近日志</h2>
                  <ActionButton
                    icon={<FolderGlyph />}
                    onClick={() =>
                      void invoke("open_log_folder").catch((openError) =>
                        setDiagError(String(openError)),
                      )
                    }
                  >
                    日志目录
                  </ActionButton>
                </div>
                <pre className="log-tail">
                  {(diagnostics?.logTail ?? []).join("\n") || "暂无日志"}
                </pre>
              </div>
            </>
          ) : (
            <>
              <div className="about-hero">
                <span className="about-mark">
                  <BrandMark />
                </span>
                <div className="about-identity">
                  <span className="about-name">LumenTray</span>
                  <span className="about-tagline">DDC/CI 本地亮度控制</span>
                </div>
                <span className="about-version">v{snapshot.appVersion}</span>
              </div>

              <div className="group">
                <div className="group-head">
                  <h2>运行</h2>
                  <ActionButton
                    icon={<NavIcon name="diagnostics" />}
                    onClick={() => setPage("diagnostics")}
                  >
                    查看诊断
                  </ActionButton>
                </div>
                <div className="card">
                  <div className="about-row">
                    <span>开机启动</span>
                    <span
                      className={`about-value ${
                        snapshot.launchAtLogin ? "on" : ""
                      }`}
                    >
                      {snapshot.launchAtLogin ? "已开启" : "已关闭"}
                    </span>
                  </div>
                  <div className="row-divider" />
                  <div className="about-row">
                    <span>全局快捷键</span>
                    <span
                      className={`about-value ${
                        snapshot.hotkeysEnabled ? "on" : ""
                      }`}
                    >
                      {snapshot.hotkeysEnabled ? "已启用" : "已停用"}
                    </span>
                  </div>
                  <div className="row-divider" />
                  <div className="about-row">
                    <span>已记录显示器</span>
                    <span>{snapshot.monitorCount} 台</span>
                  </div>
                  <div className="row-divider" />
                  <div className="about-row">
                    <span>配置文件</span>
                    <PathText value={snapshot.settingsPath} />
                  </div>
                </div>
              </div>

              <div className="privacy-card">
                <span className="privacy-icon">
                  <ShieldGlyph />
                </span>
                <div className="privacy-copy">
                  <span className="privacy-title">本地优先</span>
                  <p className="privacy-text">
                    亮度通过 DDC/CI 直接写入显示器，不读取屏幕内容，也不联网。
                  </p>
                </div>
              </div>
            </>
          )}
        </section>
      </div>
    </main>
  );
}

export default Settings;
