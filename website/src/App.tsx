import {
  AnimatePresence,
  motion,
  useAnimationFrame,
  useReducedMotion,
  type Variants,
} from "motion/react";
import { useEffect, useRef, useState } from "react";

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
        <div className="flex flex-col items-center gap-3 text-center">
          <h1>
            <Wordmark />
          </h1>
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

/**
 * Hand-drawn monoline wordmark: thin geometric caps with a crossbar-less A.
 * Glyphs live on an 80-unit cap height; strokes stay unfilled monolines.
 */
const GLYPHS: Record<string, { width: number; d: string }> = {
  S: {
    width: 52,
    d: "M47 13 C44 4 36 0 26 0 C13 0 5 6 5 16 C5 26 12 31 26 34 C41 37 49 43 49 55 C49 70 40 80 26 80 C14 80 6 75 3 67",
  },
  P: {
    width: 55,
    d: "M7 80 L7 0 L31 0 C44 0 51 8 51 21 C51 34 44 42 31 42 L7 42",
  },
  A: { width: 60, d: "M4 80 L30 0 L56 80" },
  C: {
    width: 54,
    d: "M49 14 C45 4 37 0 27 0 C11 0 5 13 5 40 C5 67 11 80 27 80 C37 80 45 76 49 66",
  },
  E: { width: 49, d: "M45 0 L9 0 L9 80 L45 80 M9 40 L35 40" },
  M: { width: 60, d: "M6 80 L6 0 L30 44 L54 0 L54 80" },
  N: { width: 56, d: "M7 80 L7 0 L49 80 L49 0" },
};

const WORDMARK_GAP = 24;

function Wordmark() {
  let x = 0;
  const placed = "SPACEMAN".split("").map((letter, index) => {
    const glyph = GLYPHS[letter];
    const entry = { key: `${letter}-${index}`, glyph, x };
    x += glyph.width + WORDMARK_GAP;
    return entry;
  });
  const total = x - WORDMARK_GAP;

  return (
    <svg
      viewBox={`-4 -5 ${total + 8} 90`}
      className="h-6"
      role="img"
      aria-label="Spaceman"
    >
      {placed.map((entry) => (
        <path
          key={entry.key}
          d={entry.glyph.d}
          transform={`translate(${entry.x},0)`}
          fill="none"
          stroke="rgba(255,255,255,0.92)"
          strokeWidth="5"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      ))}
    </svg>
  );
}

/* ------------------------------------------------------------------ */
/* Warp starfield: one shared 3D star state drawn to every registered  */
/* canvas (page background + the refracted copy inside the glass).     */
/* ------------------------------------------------------------------ */

type WarpStar = { x: number; y: number; z: number; r: number };

const WARP_STARS: WarpStar[] = Array.from({ length: 110 }, () => {
  const z = 0.12 + Math.random() * 0.88;
  return {
    x: (Math.random() * 2 - 1) * z,
    y: (Math.random() * 2 - 1) * z,
    z,
    r: 0.4 + Math.random() * 0.9,
  };
});

const WARP_SPEED = 0.035;
const starCanvases = new Set<HTMLCanvasElement>();

function registerStarCanvas(canvas: HTMLCanvasElement) {
  starCanvases.add(canvas);
  return () => {
    starCanvases.delete(canvas);
  };
}

function advanceWarp(dt: number) {
  for (const star of WARP_STARS) {
    star.z -= dt * WARP_SPEED;
    if (star.z <= 0.05 || Math.abs(star.x / star.z) > 1.2 || Math.abs(star.y / star.z) > 1.2) {
      star.x = Math.random() * 2 - 1;
      star.y = Math.random() * 2 - 1;
      star.z = 1;
      star.r = 0.4 + Math.random() * 0.9;
    }
  }
}

function drawWarp() {
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const dpr = Math.min(window.devicePixelRatio || 1, 3);
  for (const canvas of starCanvases) {
    const bounds = canvas.getBoundingClientRect();
    if (bounds.width === 0 || bounds.height === 0) continue;
    const width = Math.round(bounds.width * dpr);
    const height = Math.round(bounds.height * dpr);
    if (canvas.width !== width || canvas.height !== height) {
      canvas.width = width;
      canvas.height = height;
    }
    const ctx = canvas.getContext("2d");
    if (!ctx) continue;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, bounds.width, bounds.height);
    const cx = vw / 2 - bounds.left;
    const cy = vh / 2 - bounds.top;
    for (const star of WARP_STARS) {
      const k = 1 / star.z;
      const px = cx + star.x * (vw / 2) * k * 0.9;
      const py = cy + star.y * (vh / 2) * k * 0.9;
      const depth = 1 - star.z;
      const size = star.r * (0.3 + depth * 1.3);
      const alpha = Math.min(1, depth * 1.5) * 0.65 + 0.05;
      if (size > 0.9) {
        // Bright stars get a soft bloom instead of a hard-edged disc.
        const bloom = size * 3;
        const glow = ctx.createRadialGradient(px, py, 0, px, py, bloom);
        glow.addColorStop(0, `rgba(226,234,255,${alpha})`);
        glow.addColorStop(0.4, `rgba(205,218,255,${alpha * 0.3})`);
        glow.addColorStop(1, "rgba(205,218,255,0)");
        ctx.fillStyle = glow;
        ctx.beginPath();
        ctx.arc(px, py, bloom, 0, Math.PI * 2);
        ctx.fill();
      } else {
        ctx.beginPath();
        ctx.arc(px, py, size, 0, Math.PI * 2);
        ctx.fillStyle = `rgba(218,228,255,${alpha * 0.8})`;
        ctx.fill();
      }
    }
  }
}

function Starfield() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const reduced = useReducedMotion();

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    return registerStarCanvas(canvas);
  }, []);

  useAnimationFrame((_, delta) => {
    if (!reduced) advanceWarp(Math.min(delta, 64) / 1000);
    drawWarp();
  });

  return (
    <div aria-hidden="true" className="pointer-events-none absolute inset-0">
      <div className="nebula absolute inset-0" />
      <canvas ref={canvasRef} className="absolute inset-0 size-full" />
    </div>
  );
}

/* ------------------------------------------------------------------ */
/* Wall-clock ticker: app state derives from performance.now(), so     */
/* every app resumes exactly where it left off after workspace slides. */
/* ------------------------------------------------------------------ */

function useTicker(stepMs: number) {
  const [, force] = useState(0);
  useEffect(() => {
    const interval = window.setInterval(() => force((value) => value + 1), stepMs);
    return () => window.clearInterval(interval);
  }, [stepMs]);
  return Math.floor(performance.now() / stepMs);
}

/* ------------------------------------------------------------------ */
/* Workspace demo frame with liquid-glass refraction                   */
/* ------------------------------------------------------------------ */

const panelVariants: Variants = {
  enter: (direction: number) => ({ x: `${direction * 106}%` }),
  center: { x: "0%" },
  exit: (direction: number) => ({ x: `${direction * -106}%` }),
};

type Frame = { x: number; y: number; w: number; h: number; vw: number; vh: number };

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
  const copyCanvasRef = useRef<HTMLCanvasElement>(null);
  const [frame, setFrame] = useState<Frame | null>(null);
  const [mapUri, setMapUri] = useState<string | null>(null);

  useEffect(() => {
    const canvas = copyCanvasRef.current;
    if (!canvas) return;
    return registerStarCanvas(canvas);
  }, []);

  useEffect(() => {
    const element = frameRef.current;
    if (!element) return;
    const update = () => {
      const bounds = element.getBoundingClientRect();
      if (bounds.width === 0) return;
      setFrame({
        x: Math.round(bounds.left),
        y: Math.round(bounds.top),
        w: Math.round(bounds.width),
        h: Math.round(bounds.height),
        vw: window.innerWidth,
        vh: window.innerHeight,
      });
    };
    update();
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, { passive: true });
    const observer = new ResizeObserver(update);
    observer.observe(element);
    return () => {
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update);
      observer.disconnect();
    };
  }, []);

  useEffect(() => {
    if (frame) setMapUri(buildDisplacementMap(frame.w, frame.h));
  }, [frame]);

  return (
    <div className="relative flex w-full max-w-[520px] flex-col items-center gap-4">
      {frame && mapUri && <LiquidGlassFilter href={mapUri} width={frame.w} height={frame.h} />}
      <div
        aria-hidden="true"
        className="pointer-events-none absolute -inset-10 top-[-2.5rem] bg-[radial-gradient(ellipse,rgba(120,145,255,0.14),transparent_70%)] blur-2xl"
      />
      <div
        ref={frameRef}
        className="relative aspect-[16/10] w-full overflow-hidden rounded-2xl border border-white/[0.14] bg-[#0e1119] shadow-[inset_0_1px_0_rgba(255,255,255,0.12),inset_0_-1px_0_rgba(255,255,255,0.04),0_16px_48px_rgba(0,0,0,0.5),0_0_70px_rgba(125,145,255,0.14)]"
      >
        <div
          aria-hidden="true"
          className="pointer-events-none absolute inset-0"
          style={
            mapUri
              ? { filter: "url(#liquid-glass) blur(0.75px) saturate(170%) brightness(1.25)" }
              : undefined
          }
        >
          <div
            className="nebula absolute"
            style={{
              left: frame ? -frame.x : 0,
              top: frame ? -frame.y : 0,
              width: frame?.vw ?? "100%",
              height: frame?.vh ?? "100%",
            }}
          />
          <canvas ref={copyCanvasRef} className="absolute inset-0 size-full" />
        </div>
        <div
          aria-hidden="true"
          className="pointer-events-none absolute inset-0 bg-[#7288ff]/[0.05]"
        />

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
 * Frame-sized displacement map: red encodes horizontal displacement, green
 * vertical, with a soft neutral center so only a lens band at the edge warps.
 */
function buildDisplacementMap(w: number, h: number): string {
  const canvas = document.createElement("canvas");
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  canvas.width = w * dpr;
  canvas.height = h * dpr;
  const ctx = canvas.getContext("2d");
  if (!ctx) return "";
  ctx.scale(dpr, dpr);

  ctx.fillStyle = "rgb(0,0,0)";
  ctx.fillRect(0, 0, w, h);
  ctx.globalCompositeOperation = "lighter";
  const gradientX = ctx.createLinearGradient(0, 0, w, 0);
  gradientX.addColorStop(0, "rgb(0,0,0)");
  gradientX.addColorStop(1, "rgb(255,0,0)");
  ctx.fillStyle = gradientX;
  ctx.fillRect(0, 0, w, h);
  const gradientY = ctx.createLinearGradient(0, 0, 0, h);
  gradientY.addColorStop(0, "rgb(0,0,0)");
  gradientY.addColorStop(1, "rgb(0,255,0)");
  ctx.fillStyle = gradientY;
  ctx.fillRect(0, 0, w, h);
  ctx.globalCompositeOperation = "source-over";

  const bandStart = 8;
  const bandEnd = 36;
  const steps = 16;
  ctx.fillStyle = "rgb(128,128,0)";
  for (let step = 0; step < steps; step += 1) {
    const inset = bandStart + ((bandEnd - bandStart) * step) / (steps - 1);
    ctx.globalAlpha = 0.28;
    ctx.beginPath();
    ctx.roundRect(inset, inset, w - inset * 2, h - inset * 2, 12);
    ctx.fill();
  }
  ctx.globalAlpha = 1;
  ctx.beginPath();
  ctx.roundRect(bandEnd, bandEnd, w - bandEnd * 2, h - bandEnd * 2, 10);
  ctx.fill();

  return canvas.toDataURL("image/png");
}

function LiquidGlassFilter({
  href,
  width,
  height,
}: {
  href: string;
  width: number;
  height: number;
}) {
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
        <feImage
          href={href}
          x="0"
          y="0"
          width={width}
          height={height}
          preserveAspectRatio="none"
          result="map"
        />
        <feDisplacementMap
          in="SourceGraphic"
          in2="map"
          scale={-72}
          xChannelSelector="R"
          yChannelSelector="G"
        />
      </filter>
    </svg>
  );
}

/* ------------------------------------------------------------------ */
/* Fake windows                                                        */
/* ------------------------------------------------------------------ */

const appNames: Record<string, string> = {
  a: "Star Map",
  b: "Mission Control",
  c: "Space Tunes",
  d: "Alien Chat",
  e: "Ship Status",
};

function FakeWindow({ id, rect }: { id: string; rect: Rect }) {
  return (
    <div
      className="absolute flex flex-col overflow-hidden rounded-lg border border-white/[0.16] bg-gradient-to-b from-[#1a1d25]/95 to-[#14161c]/95 shadow-[0_0_0_0.5px_rgba(0,0,0,0.6),inset_0_0.5px_0_rgba(255,255,255,0.14),0_10px_30px_rgba(0,0,0,0.5)]"
      style={{
        left: `${rect.x}%`,
        top: `${rect.y}%`,
        width: `${rect.w}%`,
        height: `${rect.h}%`,
      }}
    >
      <div className="relative flex h-[26px] shrink-0 items-center border-b border-black/40 bg-white/[0.045] px-2.5 shadow-[inset_0_-0.5px_0_rgba(255,255,255,0.05)]">
        <div className="flex gap-1.5">
          <span className="size-[8px] rounded-full bg-[#ff5f57] shadow-[inset_0_0_1px_rgba(0,0,0,0.4)]" />
          <span className="size-[8px] rounded-full bg-[#febc2e] shadow-[inset_0_0_1px_rgba(0,0,0,0.4)]" />
          <span className="size-[8px] rounded-full bg-[#28c840] shadow-[inset_0_0_1px_rgba(0,0,0,0.4)]" />
        </div>
        <span className="pointer-events-none absolute inset-x-0 truncate px-14 text-center text-[10px] font-medium text-white/40">
          {appNames[id]}
        </span>
      </div>
      <AppBody id={id} />
    </div>
  );
}

const chartStars = [
  [12, 18], [28, 8], [44, 22], [62, 12], [80, 20], [90, 42],
  [18, 42], [36, 55], [8, 68], [52, 72], [74, 60], [88, 82],
  [24, 84], [60, 88], [40, 38], [70, 78], [15, 30], [55, 40],
  [83, 68], [33, 25], [47, 60], [92, 10], [5, 50], [65, 50],
] as const;

function AppBody({ id }: { id: string }) {
  if (id === "a") return <StarMapApp />;
  if (id === "b") return <TerminalApp />;
  if (id === "c") return <MusicApp />;
  if (id === "d") return <ChatApp />;
  return <StatusApp />;
}

/**
 * The star map route is one cubic bezier in percentage space; the SVG path,
 * waypoint dots, and per-frame rocket position all derive from it exactly.
 */
const ROUTE = {
  p0: { x: 12, y: 86 },
  p1: { x: 35, y: 78 },
  p2: { x: 40, y: 45 },
  p3: { x: 66, y: 28 },
};
const ROUTE_PATH = `M ${ROUTE.p0.x} ${ROUTE.p0.y} C ${ROUTE.p1.x} ${ROUTE.p1.y}, ${ROUTE.p2.x} ${ROUTE.p2.y}, ${ROUTE.p3.x} ${ROUTE.p3.y}`;

function routePoint(t: number) {
  const u = 1 - t;
  const { p0, p1, p2, p3 } = ROUTE;
  return {
    x: u ** 3 * p0.x + 3 * u * u * t * p1.x + 3 * u * t * t * p2.x + t ** 3 * p3.x,
    y: u ** 3 * p0.y + 3 * u * u * t * p1.y + 3 * u * t * t * p2.y + t ** 3 * p3.y,
  };
}

function routeTangent(t: number) {
  const u = 1 - t;
  const { p0, p1, p2, p3 } = ROUTE;
  return {
    x: 3 * u * u * (p1.x - p0.x) + 6 * u * t * (p2.x - p1.x) + 3 * t * t * (p3.x - p2.x),
    y: 3 * u * u * (p1.y - p0.y) + 6 * u * t * (p2.y - p1.y) + 3 * t * t * (p3.y - p2.y),
  };
}

function easeInOutCubic(t: number) {
  return t < 0.5 ? 4 * t ** 3 : 1 - (-2 * t + 2) ** 3 / 2;
}

const ROCKET_PERIOD_MS = 9000;

function StarMapApp() {
  const containerRef = useRef<HTMLDivElement>(null);
  const rocketRef = useRef<HTMLSpanElement>(null);
  const dimensions = useRef({ w: 1, h: 1 });
  const reduced = useReducedMotion();

  useEffect(() => {
    const element = containerRef.current;
    if (!element) return;
    const observer = new ResizeObserver(([entry]) => {
      const { width, height } = entry.contentRect;
      if (width > 0 && height > 0) dimensions.current = { w: width, h: height };
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  useAnimationFrame(() => {
    const rocket = rocketRef.current;
    if (!rocket) return;
    const phase = reduced
      ? 0.5
      : (performance.now() % ROCKET_PERIOD_MS) / ROCKET_PERIOD_MS;
    const t = easeInOutCubic(phase);
    const point = routePoint(t);
    const tangent = routeTangent(t);
    const { w, h } = dimensions.current;
    const heading = (Math.atan2(tangent.y * h, tangent.x * w) * 180) / Math.PI;
    const opacity = reduced
      ? 1
      : Math.max(0, Math.min(1, phase / 0.06, (1 - phase) / 0.06));
    rocket.style.left = `${point.x}%`;
    rocket.style.top = `${point.y}%`;
    rocket.style.opacity = String(opacity);
    rocket.style.transform = `translate(-50%, -50%) rotate(${heading + 45}deg)`;
  });

  return (
    <div ref={containerRef} className="relative min-h-0 flex-1 overflow-hidden">
      {chartStars.map(([sx, sy], index) => (
        <span
          key={index}
          className="absolute size-px rounded-full bg-white/40"
          style={{ left: `${sx}%`, top: `${sy}%` }}
        />
      ))}
      <svg
        className="absolute inset-0 size-full"
        viewBox="0 0 100 100"
        preserveAspectRatio="none"
        aria-hidden="true"
      >
        <path
          d="M 25 0 V 100 M 50 0 V 100 M 75 0 V 100 M 0 25 H 100 M 0 50 H 100 M 0 75 H 100"
          stroke="rgba(255,255,255,0.045)"
          strokeWidth="0.5"
          vectorEffect="non-scaling-stroke"
        />
        <path
          className="route-dash"
          d={ROUTE_PATH}
          fill="none"
          stroke="rgba(142,160,255,0.7)"
          strokeWidth="1.2"
          strokeDasharray="4 4"
          vectorEffect="non-scaling-stroke"
        />
      </svg>
      {[0.38, 0.7].map((t, index) => {
        const point = routePoint(t);
        return (
          <motion.span
            key={t}
            className="absolute size-[5px] rounded-full bg-[#8ea0ff]/80"
            style={{
              left: `${point.x}%`,
              top: `${point.y}%`,
              translateX: "-50%",
              translateY: "-50%",
            }}
            animate={{ scale: [1, 1.5, 1], opacity: [0.7, 1, 0.7] }}
            transition={{
              duration: 2.4,
              repeat: Infinity,
              delay: index * 1.2,
              ease: "easeInOut",
            }}
          />
        );
      })}
      <span
        ref={rocketRef}
        className="absolute text-sm will-change-transform"
        style={{ left: "12%", top: "86%", opacity: 0 }}
        aria-hidden="true"
      >
        🚀
      </span>
      <div className="absolute left-[60%] top-[12%] size-8">
        <motion.div
          className="size-8 rounded-full bg-[radial-gradient(circle_at_35%_30%,rgba(255,220,180,0.9),rgba(192,132,252,0.6)_45%,rgba(90,100,210,0.5)_80%)] shadow-[inset_-3px_-3px_6px_rgba(0,0,0,0.45),0_0_14px_rgba(192,132,252,0.3)]"
          animate={{ scale: [1, 1.07, 1] }}
          transition={{ duration: 3.4, repeat: Infinity, ease: "easeInOut" }}
        />
        <div className="absolute left-1/2 top-1/2 h-3.5 w-12 -translate-x-1/2 -translate-y-1/2 -rotate-[18deg] rounded-[50%] border border-[#f0b46a]/50" />
        <motion.div
          className="absolute -inset-2 rounded-full border border-dashed border-[#8ea0ff]/40"
          animate={{ rotate: 360 }}
          transition={{ duration: 24, repeat: Infinity, ease: "linear" }}
        />
      </div>
      <p className="absolute left-[8%] top-[89%] font-mono text-[8px] tracking-wider text-[#8ea0ff]/50">
        YOU ARE HERE
      </p>
      <p className="absolute bottom-2 right-2.5 font-mono text-[9px] text-white/30">
        4.2 ly to Kepler-442b · ETA: eventually
      </p>
    </div>
  );
}

const termScript = [
  { cmd: "spaceman ctl warp 9", out: "warp drive engaged 🚀" },
  { cmd: "open pod-bay-doors", out: "I'm afraid I can't do that" },
  { cmd: "spaceman ctl focus mars", out: "focused: Mars (display 4)" },
  { cmd: "sudo make-me-a-sandwich", out: "🥪 replicating..." },
];
const CMD_PAUSE = 8;
const OUT_PAUSE = 20;
const termDurations = termScript.map((item) => item.cmd.length + CMD_PAUSE + OUT_PAUSE);
const termTotal = termDurations.reduce((sum, ticks) => sum + ticks, 0);

function TerminalApp() {
  const tick = useTicker(70);

  let remaining = tick % termTotal;
  let index = 0;
  while (remaining >= termDurations[index]) {
    remaining -= termDurations[index];
    index += 1;
  }
  const current = termScript[index];
  const typed = current.cmd.slice(0, Math.min(remaining, current.cmd.length));
  const showOut = remaining >= current.cmd.length + CMD_PAUSE;
  const history = termScript.slice(Math.max(0, index - 2), index);

  return (
    <div className="flex-1 space-y-1 overflow-hidden p-2.5 font-mono text-[10px] leading-relaxed">
      {history.map((item) => (
        <div key={item.cmd} className="space-y-1">
          <p>
            <span className="text-[#7dd3a8]/80">❯</span>{" "}
            <span className="text-white/55">{item.cmd}</span>
          </p>
          <p className="text-white/25">{item.out}</p>
        </div>
      ))}
      <p>
        <span className="text-[#7dd3a8]/80">❯</span>{" "}
        <span className="text-white/55">{typed}</span>
        <span className="terminal-cursor ml-px inline-block h-2.5 w-[7px] translate-y-px bg-white/50" />
      </p>
      {showOut && <p className="text-white/25">{current.out}</p>}
    </div>
  );
}

const songs = [
  { title: "Space Oddity", artist: "David Bowie" },
  { title: "Rocket Man", artist: "Elton John" },
  { title: "Starman", artist: "David Bowie" },
];
const SONG_MS = 12000;

function MusicApp() {
  const tick = useTicker(250);
  const nowMs = tick * 250;
  const songIndex = Math.floor(nowMs / SONG_MS);
  const track = songs[songIndex % songs.length];
  const progress = ((nowMs % SONG_MS) / SONG_MS) * 100;

  return (
    <div className="flex flex-1 items-center gap-3 overflow-hidden p-3">
      <div className="relative size-12 shrink-0">
        <motion.div
          className="size-12 rounded-full bg-gradient-to-br from-[#f0b46a]/70 via-[#c084fc]/50 to-[#8ea0ff]/50"
          animate={{ rotate: 360 }}
          transition={{ duration: 20, repeat: Infinity, ease: "linear" }}
        />
        <div className="absolute left-1/2 top-1/2 h-5 w-[70px] -translate-x-1/2 -translate-y-1/2 -rotate-[20deg] rounded-[50%] border-2 border-[#f0b46a]/40" />
      </div>
      <div className="min-w-0 flex-1 space-y-1">
        <motion.div
          key={songIndex}
          initial={{ opacity: 0, y: 4 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.3 }}
        >
          <p className="truncate text-[10px] font-medium text-white/60">{track.title}</p>
          <p className="truncate text-[9px] text-white/30">{track.artist}</p>
        </motion.div>
        <div className="mt-1.5 flex items-center gap-2">
          <div className="h-1 min-w-0 flex-1 overflow-hidden rounded-full bg-white/10">
            <div
              className="h-full rounded-full bg-white/40 transition-[width] duration-300 ease-linear"
              style={{ width: `${progress}%` }}
            />
          </div>
          <div className="flex h-3 shrink-0 items-end gap-[2px]" aria-hidden="true">
            {[0.5, 0.9, 0.65, 1, 0.75].map((peak, bar) => (
              <motion.span
                key={bar}
                className="w-[2px] origin-bottom rounded-sm bg-[#8ea0ff]/60"
                style={{ height: 12 }}
                animate={{ scaleY: [0.25, peak, 0.4, peak * 0.8, 0.25] }}
                transition={{
                  duration: 0.9 + bar * 0.17,
                  repeat: Infinity,
                  ease: "easeInOut",
                }}
              />
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}

const chatScript = [
  { self: false, text: "take me to your leader 👽" },
  { self: true, text: "can't. tiling windows 🧑‍🚀" },
  { self: false, text: "respect 👽" },
  { self: false, text: "is that liquid glass? 👽" },
  { self: true, text: "obviously 🧑‍🚀" },
  { self: false, text: "humanity has peaked 👽" },
];

function ChatApp() {
  const tick = useTicker(1100);
  const delivered = Math.floor(tick / 2);
  const typing = tick % 2 === 1;
  const next = chatScript[delivered % chatScript.length];

  const visible = [];
  for (let index = Math.max(0, delivered - 3); index < delivered; index += 1) {
    visible.push({ key: index, ...chatScript[index % chatScript.length] });
  }

  return (
    <div className="flex flex-1 flex-col justify-end gap-1.5 overflow-hidden p-2.5 text-[10px] leading-tight">
      <AnimatePresence initial={false}>
        {visible.map((message) => (
          <motion.p
            layout
            key={message.key}
            initial={{ opacity: 0, y: 10, scale: 0.9 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, transition: { duration: 0.12 } }}
            transition={{ type: "spring", visualDuration: 0.3, bounce: 0.2 }}
            className={`max-w-[85%] rounded-lg px-2 py-1 ${
              message.self
                ? "self-end rounded-br-sm bg-[#4a7dff]/40 text-white/70"
                : "self-start rounded-bl-sm bg-white/[0.08] text-white/55"
            }`}
          >
            {message.text}
          </motion.p>
        ))}
        {typing && (
          <motion.div
            layout
            key={`typing-${delivered}`}
            initial={{ opacity: 0, y: 10, scale: 0.9 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, transition: { duration: 0.1 } }}
            transition={{ type: "spring", visualDuration: 0.3, bounce: 0.2 }}
            className={`flex gap-[3px] rounded-lg px-2 py-1.5 ${
              next.self
                ? "self-end rounded-br-sm bg-[#4a7dff]/40"
                : "self-start rounded-bl-sm bg-white/[0.08]"
            }`}
          >
            {[0, 1, 2].map((dot) => (
              <span
                key={dot}
                className="size-1 animate-pulse rounded-full bg-white/50"
                style={{ animationDelay: `${dot * 0.2}s` }}
              />
            ))}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

function StatusApp() {
  const tick = useTicker(1500);
  const snacks = 99 - (tick % 100);

  const gauges = [
    ["OXYGEN", 98, "bg-[#7dd3a8]/60"],
    ["FUEL", 61, "bg-[#f0b46a]/60"],
    ["GRAVITY", 100, "bg-[#8ea0ff]/60"],
    ["SNACKS", snacks, snacks < 15 ? "bg-[#ff5f57]/70 animate-pulse" : "bg-[#ff5f57]/70"],
  ] as const;

  return (
    <div className="flex min-h-0 flex-1 flex-col justify-center gap-2 overflow-hidden p-3 font-mono text-[9px]">
      {gauges.map(([label, value, tone]) => (
        <div key={label} className="space-y-1">
          <div className="flex justify-between text-white/35">
            <span>{label}</span>
            <span>{value}%</span>
          </div>
          <div className="h-1.5 rounded-full bg-white/[0.07]">
            <div
              className={`h-full rounded-full transition-[width] duration-1000 ease-linear ${tone}`}
              style={{ width: `${value}%` }}
            />
          </div>
        </div>
      ))}
    </div>
  );
}
