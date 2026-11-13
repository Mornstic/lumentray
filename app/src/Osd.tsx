import { useEffect, useState } from "react";
import type { CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./Osd.css";

type OsdState = {
  name: string;
  index: number;
  total: number;
  percent: number | null;
  primary: boolean;
  synced: number;
  reachable: number;
};

type Phase = "in" | "out";

function syncLabel(state: OsdState) {
  if (state.percent === null) {
    return state.synced > 0
      ? `此屏无响应 · ${state.synced} 台已同步`
      : "此屏暂无响应";
  }
  if (state.reachable <= 1) {
    return state.synced > 0 ? "已应用" : "显示器未响应";
  }
  if (state.synced >= state.reachable) {
    return `全部 ${state.reachable} 台同步`;
  }
  if (state.synced === 0) {
    return "显示器未响应";
  }
  return `${state.synced}/${state.reachable} 台已同步`;
}

function errorState(state: OsdState) {
  return state.percent === null || state.synced === 0;
}

export default function Osd() {
  const [state, setState] = useState<OsdState | null>(null);
  const [phase, setPhase] = useState<Phase>("in");

  useEffect(() => {
    let disposed = false;
    void invoke<OsdState | null>("get_osd_state")
      .then((initial) => {
        if (!disposed && initial) setState(initial);
      })
      .catch(() => {});
    const stateListener = listen<OsdState>("osd-state", (event) => {
      if (disposed) return;
      setState(event.payload);
      setPhase("in");
    });
    const hideListener = listen("osd-hide", () => {
      if (!disposed) setPhase("out");
    });
    return () => {
      disposed = true;
      void stateListener.then((unlisten) => unlisten());
      void hideListener.then((unlisten) => unlisten());
    };
  }, []);

  const percent = state?.percent ?? null;

  return (
    <div className={`osd${phase === "out" ? " is-leaving" : ""}`}>
      {state && (
        <div className="osd-pill">
          <span
            className="osd-swatch"
            style={{ "--level": (percent ?? 0) / 100 } as CSSProperties}
            aria-hidden="true"
          >
            <span className="osd-swatch-fill" />
            <span className="osd-swatch-sheen" />
          </span>
          <div className="osd-meta">
            <div className="osd-title">
              <span className="osd-index">
                {String(state.index).padStart(2, "0")}
              </span>
              <span className="osd-name">{state.name}</span>
              {state.primary && <span className="osd-primary">主屏</span>}
            </div>
            <div className={`osd-sub${errorState(state) ? " is-error" : ""}`}>
              {syncLabel(state)}
            </div>
          </div>
          <div className="osd-readout">
            <div className="osd-value">
              <span className="osd-number">
                {percent === null ? "—" : percent}
              </span>
              {percent !== null && <span className="osd-unit">%</span>}
            </div>
            <div
              className={`osd-bar${percent === null ? " is-unknown" : ""}`}
              aria-hidden="true"
            >
              <span
                className="osd-bar-fill"
                style={{ width: `${percent ?? 0}%` }}
              />
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
