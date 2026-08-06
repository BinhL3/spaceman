import { AnimatePresence, motion, type Variants } from "motion/react";
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
} from "react";

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

export function App() {
  const [[workspace, direction], setState] = useState<[number, number]>([0, 1]);

  const select = (next: number) =>
    setState(([current]) =>
      next === current ? [current, 1] : [next, next > current ? 1 : -1],
    );

  useEffect(() => {
    const interval = window.setInterval(() => {
      setState(([current]) => [(current + 1) % layouts.length, 1]);
    }, 3200);
    return () => window.clearInterval(interval);
  }, [workspace]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      const digit = Number(event.key);
      if (Number.isInteger(digit) && digit >= 1 && digit <= layouts.length) {
        select(digit - 1);
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

      <WorkspaceDemo workspace={workspace} direction={direction} onSelect={select} />

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

const panelVariants: Variants = {
  enter: (direction: number) => ({ x: `${direction * 106}%` }),
  center: { x: "0%" },
  exit: (direction: number) => ({ x: `${direction * -106}%` }),
};

function WorkspaceDemo({
  workspace,
  direction,
  onSelect,
}: {
  workspace: number;
  direction: number;
  onSelect: (workspace: number) => void;
}) {
  const frameRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 520, h: 325 });

  useEffect(() => {
    const element = frameRef.current;
    if (!element) return;
    const observer = new ResizeObserver(([entry]) => {
      const { width, height } = entry.contentRect;
      if (width > 0 && height > 0) {
        setSize({ w: Math.round(width), h: Math.round(height) });
      }
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  return (
    <div className="relative flex w-full max-w-[520px] flex-col items-center gap-4">
      <LiquidGlassFilter width={size.w} height={size.h} />
      <div
        ref={frameRef}
        className="liquid-glass relative aspect-[16/10] w-full overflow-hidden rounded-2xl border border-white/[0.12] bg-white/[0.04] shadow-[inset_0_1px_0_rgba(255,255,255,0.12),inset_0_-1px_0_rgba(255,255,255,0.04),0_16px_48px_rgba(0,0,0,0.5)]"
      >
        <AnimatePresence initial={false} custom={direction}>
          <motion.div
            key={workspace}
            custom={direction}
            variants={panelVariants}
            initial="enter"
            animate="center"
            exit="exit"
            transition={{ type: "spring", visualDuration: 0.3, bounce: 0.05 }}
            className="absolute inset-0"
          >
            {Object.entries(layouts[workspace]).map(([id, rect]) => (
              <FakeWindow key={id} id={id} rect={rect} />
            ))}
          </motion.div>
        </AnimatePresence>
        <div
          aria-hidden="true"
          className="pointer-events-none absolute inset-0 bg-[linear-gradient(115deg,rgba(255,255,255,0.05),transparent_40%)]"
        />
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
            className="relative grid size-8 place-items-center rounded-md font-mono text-[13px] tabular-nums focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[#8ea0ff]"
          >
            {index === workspace && (
              <motion.span
                layoutId="active-workspace"
                className="absolute inset-0 rounded-md bg-white/10"
                transition={{ type: "spring", visualDuration: 0.3, bounce: 0.1 }}
              />
            )}
            <motion.span
              animate={{ opacity: index === workspace ? 1 : 0.35 }}
              transition={{ duration: 0.25, ease: "easeOut" }}
              className="relative text-white"
            >
              {index + 1}
            </motion.span>
          </button>
        ))}
      </div>
    </div>
  );
}

/**
 * Edge-refraction displacement map: red encodes horizontal displacement,
 * green encodes vertical. The blurred neutral-gray rounded rect in the middle
 * limits the warp to a lens-like band around the frame's edge.
 */
function displacementMapUri(width: number, height: number) {
  const edge = 18;
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}">` +
    `<defs>` +
    `<linearGradient id="x" x1="0%" y1="0%" x2="100%" y2="0%">` +
    `<stop offset="0%" stop-color="#000"/><stop offset="100%" stop-color="#f00"/>` +
    `</linearGradient>` +
    `<linearGradient id="y" x1="0%" y1="0%" x2="0%" y2="100%">` +
    `<stop offset="0%" stop-color="#000"/><stop offset="100%" stop-color="#0f0"/>` +
    `</linearGradient>` +
    `</defs>` +
    `<rect width="${width}" height="${height}" fill="#000"/>` +
    `<rect width="${width}" height="${height}" fill="url(#x)"/>` +
    `<rect width="${width}" height="${height}" fill="url(#y)" style="mix-blend-mode:screen"/>` +
    `<rect x="${edge}" y="${edge}" width="${width - edge * 2}" height="${height - edge * 2}" rx="14" fill="rgb(128,128,128)" style="filter:blur(${Math.round(edge * 0.65)}px)"/>` +
    `</svg>`;
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}

function LiquidGlassFilter({ width, height }: { width: number; height: number }) {
  const href = useMemo(() => displacementMapUri(width, height), [width, height]);
  return (
    <svg width="0" height="0" aria-hidden="true" className="absolute">
      <filter
        id="liquid-glass"
        x="0"
        y="0"
        width={width}
        height={height}
        filterUnits="userSpaceOnUse"
        primitiveUnits="userSpaceOnUse"
        colorInterpolationFilters="sRGB"
      >
        <feImage href={href} x="0" y="0" width={width} height={height} result="map" />
        <feDisplacementMap
          in="SourceGraphic"
          in2="map"
          scale={-48}
          xChannelSelector="R"
          yChannelSelector="G"
        />
      </filter>
    </svg>
  );
}

const appNames: Record<string, string> = {
  a: "Editor",
  b: "Terminal",
  c: "Music",
  d: "Chat",
  e: "Web",
};

function FakeWindow({ id, rect }: { id: string; rect: Rect }) {
  return (
    <div
      className="absolute flex flex-col overflow-hidden rounded-md bg-[#14161b]/90 shadow-[inset_0_1px_0_rgba(255,255,255,0.07),0_4px_16px_rgba(0,0,0,0.35)]"
      style={{
        left: `${rect.x}%`,
        top: `${rect.y}%`,
        width: `${rect.w}%`,
        height: `${rect.h}%`,
      }}
    >
      <div className="flex h-6 shrink-0 items-center gap-1.5 border-b border-white/[0.05] px-2.5">
        <span className="size-[7px] rounded-full bg-[#ff5f57]/60" />
        <span className="size-[7px] rounded-full bg-[#febc2e]/60" />
        <span className="size-[7px] rounded-full bg-[#28c840]/60" />
        <span className="ml-1.5 text-[10px] font-medium text-white/30">{appNames[id]}</span>
      </div>
      <AppBody id={id} />
    </div>
  );
}

const codeLines: Array<[number, Array<[string, number]>]> = [
  [0, [["bg-[#c084fc]/50", 24], ["bg-white/20", 40]]],
  [1, [["bg-[#8ea0ff]/50", 32], ["bg-[#7dd3a8]/40", 52], ["bg-white/15", 20]]],
  [1, [["bg-white/15", 44], ["bg-[#f0b46a]/45", 28]]],
  [2, [["bg-[#8ea0ff]/50", 24], ["bg-white/15", 60]]],
  [2, [["bg-[#7dd3a8]/40", 36], ["bg-white/15", 32]]],
  [1, [["bg-white/15", 24]]],
  [0, [["bg-[#c084fc]/50", 20], ["bg-white/20", 48]]],
  [1, [["bg-[#8ea0ff]/50", 40], ["bg-[#f0b46a]/45", 24], ["bg-white/15", 36]]],
  [1, [["bg-[#7dd3a8]/40", 28], ["bg-white/15", 44]]],
  [0, [["bg-white/15", 16]]],
];

function AppBody({ id }: { id: string }) {
  if (id === "a") {
    return (
      <div className="flex min-h-0 flex-1">
        <div className="w-14 shrink-0 space-y-1.5 border-r border-white/[0.05] p-2">
          {[32, 24, 36, 20, 28].map((width, index) => (
            <div
              key={index}
              className="h-1.5 rounded-full bg-white/10"
              style={{ width }}
            />
          ))}
        </div>
        <div className="min-w-0 flex-1 space-y-[7px] overflow-hidden p-2.5">
          {codeLines.map(([indent, segments], index) => (
            <div key={index} className="flex gap-1.5" style={{ paddingLeft: indent * 12 }}>
              {segments.map(([tone, width], segment) => (
                <span
                  key={segment}
                  className={`h-1.5 shrink-0 rounded-full ${tone}`}
                  style={{ width }}
                />
              ))}
            </div>
          ))}
        </div>
      </div>
    );
  }

  if (id === "b") {
    return (
      <div className="flex-1 space-y-1 overflow-hidden p-2.5 font-mono text-[10px] leading-relaxed">
        <p>
          <span className="text-[#7dd3a8]/80">❯</span>{" "}
          <span className="text-white/55">spaceman ctl focus right</span>
        </p>
        <p className="text-white/25">focused Editor</p>
        <p>
          <span className="text-[#7dd3a8]/80">❯</span>{" "}
          <span className="text-white/55">cargo build --release</span>
        </p>
        <p className="text-white/25">Finished in 4.2s</p>
        <p>
          <span className="text-[#7dd3a8]/80">❯</span>{" "}
          <span className="terminal-cursor inline-block h-2.5 w-[7px] translate-y-px bg-white/50" />
        </p>
      </div>
    );
  }

  if (id === "c") {
    return (
      <div className="flex flex-1 items-center gap-3 overflow-hidden p-3">
        <div className="size-12 shrink-0 rounded-md bg-gradient-to-br from-[#8ea0ff]/60 via-[#c084fc]/40 to-[#f0b46a]/30" />
        <div className="min-w-0 flex-1 space-y-2">
          <div className="h-1.5 w-3/5 rounded-full bg-white/25" />
          <div className="h-1.5 w-2/5 rounded-full bg-white/10" />
          <div className="h-1 w-full rounded-full bg-white/10">
            <div className="h-full w-1/3 rounded-full bg-white/40" />
          </div>
        </div>
      </div>
    );
  }

  if (id === "d") {
    return (
      <div className="flex flex-1 flex-col justify-end gap-1.5 overflow-hidden p-2.5">
        <div className="h-5 w-4/5 self-start rounded-lg rounded-bl-sm bg-white/[0.08]" />
        <div className="h-5 w-3/5 self-end rounded-lg rounded-br-sm bg-[#4a7dff]/40" />
        <div className="h-8 w-5/6 self-start rounded-lg rounded-bl-sm bg-white/[0.08]" />
        <div className="h-5 w-1/2 self-end rounded-lg rounded-br-sm bg-[#4a7dff]/40" />
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-hidden p-2.5">
      <div className="flex h-5 shrink-0 items-center rounded-full bg-white/[0.07] px-2.5">
        <div className="h-1 w-24 rounded-full bg-white/15" />
      </div>
      <div className="h-16 shrink-0 rounded bg-gradient-to-br from-white/[0.08] to-white/[0.02]" />
      <div className="space-y-1.5">
        <div className="h-1.5 w-11/12 rounded-full bg-white/12" />
        <div className="h-1.5 w-4/5 rounded-full bg-white/[0.07]" />
        <div className="h-1.5 w-3/5 rounded-full bg-white/[0.07]" />
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
