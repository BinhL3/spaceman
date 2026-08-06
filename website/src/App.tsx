import { AnimatePresence, motion, type Variants } from "motion/react";
import {
  useEffect,
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
      <div aria-hidden="true" className="pointer-events-none absolute inset-0">
        <StarLayer twinkle />
      </div>

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
  const [frame, setFrame] = useState<Frame | null>(null);
  const [mapUri, setMapUri] = useState<string | null>(null);

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
    if (frame) setMapUri(buildDisplacementMap(frame));
  }, [frame]);

  return (
    <div className="relative flex w-full max-w-[520px] flex-col items-center gap-4">
      {frame && mapUri && <LiquidGlassFilter href={mapUri} width={frame.vw} height={frame.vh} />}
      <div
        aria-hidden="true"
        className="pointer-events-none absolute -inset-10 top-[-2.5rem] bg-[radial-gradient(ellipse,rgba(120,145,255,0.14),transparent_70%)] blur-2xl"
      />
      <div
        ref={frameRef}
        className="relative aspect-[16/10] w-full overflow-hidden rounded-2xl border border-white/[0.14] bg-[#0e1119] shadow-[inset_0_1px_0_rgba(255,255,255,0.12),inset_0_-1px_0_rgba(255,255,255,0.04),0_16px_48px_rgba(0,0,0,0.5),0_0_70px_rgba(125,145,255,0.14)]"
      >
        {frame && mapUri && (
          <div
            aria-hidden="true"
            className="pointer-events-none absolute"
            style={{
              left: -frame.x,
              top: -frame.y,
              width: frame.vw,
              height: frame.vh,
              filter: "url(#liquid-glass) blur(0.75px) saturate(170%) brightness(1.25)",
            }}
          >
            <StarLayer twinkle={false} />
          </div>
        )}
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
 * Viewport-sized displacement map PNG. Neutral gray everywhere except a lens
 * band baked at the frame's position: red encodes horizontal displacement,
 * green vertical, softly ramping back to neutral toward the frame's center.
 */
function buildDisplacementMap({ x, y, w, h, vw, vh }: Frame): string {
  const canvas = document.createElement("canvas");
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  canvas.width = vw * dpr;
  canvas.height = vh * dpr;
  const ctx = canvas.getContext("2d");
  if (!ctx) return "";
  ctx.scale(dpr, dpr);

  ctx.fillStyle = "rgb(128,128,0)";
  ctx.fillRect(0, 0, vw, vh);

  ctx.fillStyle = "rgb(0,0,0)";
  ctx.fillRect(x, y, w, h);
  ctx.globalCompositeOperation = "lighter";
  const gradientX = ctx.createLinearGradient(x, 0, x + w, 0);
  gradientX.addColorStop(0, "rgb(0,0,0)");
  gradientX.addColorStop(1, "rgb(255,0,0)");
  ctx.fillStyle = gradientX;
  ctx.fillRect(x, y, w, h);
  const gradientY = ctx.createLinearGradient(0, y, 0, y + h);
  gradientY.addColorStop(0, "rgb(0,0,0)");
  gradientY.addColorStop(1, "rgb(0,255,0)");
  ctx.fillStyle = gradientY;
  ctx.fillRect(x, y, w, h);
  ctx.globalCompositeOperation = "source-over";

  // Soft ramp from the lens band back to neutral, then a solid neutral center.
  const bandStart = 8;
  const bandEnd = 36;
  const steps = 16;
  ctx.fillStyle = "rgb(128,128,0)";
  for (let step = 0; step < steps; step += 1) {
    const inset = bandStart + ((bandEnd - bandStart) * step) / (steps - 1);
    ctx.globalAlpha = 0.28;
    ctx.beginPath();
    ctx.roundRect(x + inset, y + inset, w - inset * 2, h - inset * 2, 12);
    ctx.fill();
  }
  ctx.globalAlpha = 1;
  ctx.beginPath();
  ctx.roundRect(x + bandEnd, y + bandEnd, w - bandEnd * 2, h - bandEnd * 2, 10);
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

/** Cubic bezier samples along the star map route for the rocket's journey. */
const routeKeyframes = {
  left: ["10%", "24.5%", "35.9%", "47.8%", "64%"],
  top: ["83%", "72.9%", "57.4%", "40.1%", "25%"],
};

function StarMapApp() {
  return (
    <div className="relative min-h-0 flex-1 overflow-hidden">
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
          d="M 12 86 C 35 78, 40 45, 66 28"
          fill="none"
          stroke="rgba(142,160,255,0.7)"
          strokeWidth="1.2"
          strokeDasharray="4 4"
          vectorEffect="non-scaling-stroke"
        />
        <circle cx="37.9" cy="60.4" r="1.2" fill="rgba(142,160,255,0.8)" />
        <circle cx="49.8" cy="43.1" r="1.2" fill="rgba(142,160,255,0.8)" />
      </svg>
      <motion.span
        className="absolute text-sm"
        aria-hidden="true"
        animate={{
          left: routeKeyframes.left,
          top: routeKeyframes.top,
          opacity: [0, 1, 1, 1, 0],
        }}
        transition={{ duration: 7, times: [0, 0.2, 0.5, 0.8, 1], repeat: Infinity, repeatDelay: 0.8, ease: "linear" }}
      >
        🚀
      </motion.span>
      <div className="absolute left-[60%] top-[12%] size-8">
        <motion.div
          className="size-8 rounded-full bg-gradient-to-br from-[#f0b46a]/70 via-[#c084fc]/50 to-[#8ea0ff]/40"
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
      <p className="absolute left-[8%] top-[88%] font-mono text-[8px] tracking-wider text-[#8ea0ff]/50">
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
  const [tick, setTick] = useState(0);

  useEffect(() => {
    const interval = window.setInterval(() => setTick((value) => value + 1), 70);
    return () => window.clearInterval(interval);
  }, []);

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
const SONG_SECONDS = 12;

function MusicApp() {
  const [song, setSong] = useState(0);

  useEffect(() => {
    const interval = window.setInterval(
      () => setSong((value) => value + 1),
      SONG_SECONDS * 1000,
    );
    return () => window.clearInterval(interval);
  }, []);

  const track = songs[song % songs.length];

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
          key={song}
          initial={{ opacity: 0, y: 4 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.3 }}
        >
          <p className="truncate text-[10px] font-medium text-white/60">{track.title}</p>
          <p className="truncate text-[9px] text-white/30">{track.artist}</p>
        </motion.div>
        <div className="mt-1.5 flex items-center gap-2">
          <div className="h-1 min-w-0 flex-1 overflow-hidden rounded-full bg-white/10">
            <motion.div
              key={song}
              className="h-full rounded-full bg-white/40"
              initial={{ width: "0%" }}
              animate={{ width: "100%" }}
              transition={{ duration: SONG_SECONDS, ease: "linear" }}
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
  const [tick, setTick] = useState(4);

  useEffect(() => {
    const interval = window.setInterval(() => setTick((value) => value + 1), 1100);
    return () => window.clearInterval(interval);
  }, []);

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
  const [snacks, setSnacks] = useState(12);

  useEffect(() => {
    const interval = window.setInterval(
      () => setSnacks((value) => (value <= 0 ? 99 : value - 1)),
      1500,
    );
    return () => window.clearInterval(interval);
  }, []);

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

type Star = {
  id: number;
  x: number;
  y: number;
  size: number;
  opacity: number;
  duration: number;
  delay: number;
};

const STARS: Star[] = (() => {
  let seed = 20260805;
  const random = () => {
    seed = (seed * 16807) % 2147483647;
    return seed / 2147483647;
  };
  return Array.from({ length: 220 }, (_, index) => ({
    id: index,
    x: random() * 100,
    y: random() * 100,
    size: random() < 0.8 ? 1 : 2,
    opacity: 0.15 + random() * 0.5,
    duration: 3 + random() * 6,
    delay: random() * 9,
  }));
})();

function StarLayer({ twinkle }: { twinkle: boolean }) {
  return (
    <div className="absolute inset-0 overflow-hidden">
      <div className="nebula absolute inset-0" />
      {STARS.map((star) => (
        <span
          key={star.id}
          className={`absolute rounded-full bg-white ${twinkle ? "star" : ""}`}
          style={
            {
              left: `${star.x}%`,
              top: `${star.y}%`,
              width: star.size,
              height: star.size,
              opacity: star.opacity,
              boxShadow:
                star.size > 1 ? "0 0 4px rgba(200,212,255,0.6)" : undefined,
              "--star-opacity": star.opacity,
              animationDuration: twinkle ? `${star.duration}s` : undefined,
              animationDelay: twinkle ? `-${star.delay}s` : undefined,
            } as CSSProperties
          }
        />
      ))}
    </div>
  );
}
