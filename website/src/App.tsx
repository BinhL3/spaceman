import { motion } from "motion/react";
import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";

const DOWNLOAD_URL =
  "https://pub-6f85d683ffe0419cad0a71508904053a.r2.dev/releases/Spaceman-v0.1.1-macos-arm64.zip";

type Rect = { x: number; y: number; w: number; h: number };

const layouts: Array<Record<string, Rect>> = [
  {
    a: { x: 2.5, y: 4, w: 60, h: 92 },
    b: { x: 64.5, y: 4, w: 33, h: 44 },
    c: { x: 64.5, y: 52, w: 33, h: 44 },
  },
  {
    b: { x: 2.5, y: 4, w: 66, h: 92 },
    d: { x: 70.5, y: 4, w: 27, h: 92 },
  },
  {
    e: { x: 2.5, y: 4, w: 46.75, h: 92 },
    a: { x: 51.25, y: 4, w: 46.75, h: 92 },
  },
];

const windowIds = [...new Set(layouts.flatMap((layout) => Object.keys(layout)))];

function firstRect(id: string): Rect {
  for (const layout of layouts) {
    const rect = layout[id];
    if (rect) return rect;
  }
  throw new Error(`No rect for window ${id}`);
}

export function App() {
  const [workspace, setWorkspace] = useState(0);

  useEffect(() => {
    const interval = window.setInterval(() => {
      setWorkspace((current) => (current + 1) % layouts.length);
    }, 3200);
    return () => window.clearInterval(interval);
  }, [workspace]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      const digit = Number(event.key);
      if (Number.isInteger(digit) && digit >= 1 && digit <= layouts.length) {
        setWorkspace(digit - 1);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  return (
    <main className="relative flex min-h-dvh flex-col items-center justify-center gap-10 overflow-hidden bg-[#0b0c0e] px-6 py-16 text-[#f2f2f0] antialiased">
      <Starfield />

      <header className="relative flex flex-col items-center gap-5">
        <img src="/mark.svg" alt="" className="size-14 rounded-[14px] shadow-lg" />
        <div className="flex flex-col items-center gap-2 text-center">
          <h1 className="text-2xl font-medium tracking-tight">Spaceman</h1>
          <p className="text-base text-[#87888e]">
            Another fucking tiling window manager for macOS.
          </p>
        </div>
      </header>

      <WorkspaceDemo workspace={workspace} onSelect={setWorkspace} />

      <footer className="relative flex flex-col items-center gap-3">
        <a
          href={DOWNLOAD_URL}
          className="rounded-full bg-[#f0f0ee] px-6 py-2.5 text-sm font-medium text-[#101114] transition-opacity duration-150 hover:opacity-85 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[#8ea0ff]"
        >
          Download
        </a>
        <p className="text-[13px] text-[#5c5d63]">v0.1.1 · macOS 14+ · Apple silicon</p>
      </footer>
    </main>
  );
}

function WorkspaceDemo({
  workspace,
  onSelect,
}: {
  workspace: number;
  onSelect: (workspace: number) => void;
}) {
  const lastRects = useRef(new Map(windowIds.map((id) => [id, firstRect(id)])));

  return (
    <div className="relative flex w-full max-w-[520px] flex-col items-center gap-4">
      <div className="relative aspect-[16/10] w-full overflow-hidden rounded-xl border border-white/10 bg-[#101114]/90 shadow-[0_20px_60px_rgba(0,0,0,0.45)] backdrop-blur-sm">
        {windowIds.map((id) => {
          const rect = layouts[workspace][id];
          if (rect) lastRects.current.set(id, rect);
          const shown = rect ?? lastRects.current.get(id)!;
          return (
            <motion.div
              key={id}
              initial={false}
              animate={{
                left: `${shown.x}%`,
                top: `${shown.y}%`,
                width: `${shown.w}%`,
                height: `${shown.h}%`,
                opacity: rect ? 1 : 0,
                scale: rect ? 1 : 0.94,
              }}
              transition={{
                default: { type: "spring", visualDuration: 0.55, bounce: 0.16 },
                opacity: { duration: 0.3, ease: "easeOut" },
              }}
              style={{ zIndex: rect ? 1 : 0 }}
              className="absolute rounded-md bg-white/[0.07]"
            />
          );
        })}
      </div>

      <div className="flex items-center gap-1" role="tablist" aria-label="Workspaces">
        {layouts.map((_, index) => (
          <button
            key={index}
            type="button"
            role="tab"
            aria-selected={index === workspace}
            aria-label={`Workspace ${index + 1}`}
            onClick={() => onSelect(index)}
            className="relative grid size-8 place-items-center rounded-md font-mono text-[13px] tabular-nums transition-colors duration-150 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[#8ea0ff]"
          >
            {index === workspace && (
              <motion.span
                layoutId="active-workspace"
                className="absolute inset-0 rounded-md bg-white/10"
                transition={{ type: "spring", visualDuration: 0.3, bounce: 0.1 }}
              />
            )}
            <span
              className={`relative ${index === workspace ? "text-white" : "text-[#54555c]"}`}
            >
              {index + 1}
            </span>
          </button>
        ))}
      </div>
    </div>
  );
}

function Starfield() {
  const stars = useMemo(() => {
    let seed = 20260805;
    const random = () => {
      seed = (seed * 16807) % 2147483647;
      return seed / 2147483647;
    };
    return Array.from({ length: 140 }, (_, index) => ({
      id: index,
      x: random() * 100,
      y: random() * 100,
      size: random() < 0.85 ? 1 : 2,
      opacity: 0.12 + random() * 0.45,
      duration: 3 + random() * 6,
      delay: random() * 9,
    }));
  }, []);

  return (
    <div aria-hidden="true" className="pointer-events-none absolute inset-0">
      <div className="nebula absolute inset-0" />
      {stars.map((star) => (
        <span
          key={star.id}
          className="star absolute rounded-full bg-white"
          style={
            {
              left: `${star.x}%`,
              top: `${star.y}%`,
              width: star.size,
              height: star.size,
              "--star-opacity": star.opacity,
              animationDuration: `${star.duration}s`,
              animationDelay: `-${star.delay}s`,
            } as CSSProperties
          }
        />
      ))}
    </div>
  );
}
