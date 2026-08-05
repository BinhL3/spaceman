import { AnimatePresence, motion } from "motion/react";
import { useEffect, useState } from "react";

const DOWNLOAD_URL =
  "https://pub-6f85d683ffe0419cad0a71508904053a.r2.dev/releases/Spaceman-v0.1.1-macos-arm64.zip";

type Tile = {
  id: string;
  name: string;
  detail: string;
  className: string;
  tone: string;
};

const workspaces: Array<{ label: string; tiles: Tile[] }> = [
  {
    label: "Build",
    tiles: [
      {
        id: "editor",
        name: "Zed",
        detail: "src/app.rs",
        className: "left-[2%] top-[3%] h-[94%] w-[61%]",
        tone: "bg-[#151922]",
      },
      {
        id: "terminal",
        name: "Ghostty",
        detail: "cargo test",
        className: "left-[65%] top-[3%] h-[45%] w-[33%]",
        tone: "bg-[#141817]",
      },
      {
        id: "browser",
        name: "Browser",
        detail: "localhost:5173",
        className: "left-[65%] top-[51%] h-[46%] w-[33%]",
        tone: "bg-[#1a1815]",
      },
    ],
  },
  {
    label: "Research",
    tiles: [
      {
        id: "browser",
        name: "Browser",
        detail: "Documentation",
        className: "left-[2%] top-[3%] h-[94%] w-[67%]",
        tone: "bg-[#1a1815]",
      },
      {
        id: "notes",
        name: "Notes",
        detail: "Window model",
        className: "left-[71%] top-[3%] h-[94%] w-[27%]",
        tone: "bg-[#19191d]",
      },
    ],
  },
  {
    label: "Comms",
    tiles: [
      {
        id: "messages",
        name: "Messages",
        detail: "Project room",
        className: "left-[2%] top-[3%] h-[94%] w-[42%]",
        tone: "bg-[#141a19]",
      },
      {
        id: "calendar",
        name: "Calendar",
        detail: "Wednesday, August 5",
        className: "left-[46%] top-[3%] h-[94%] w-[52%]",
        tone: "bg-[#19171b]",
      },
    ],
  },
];

const features = [
  ["Fast by default", "Switch workspaces without the flash, shuffle, or pause."],
  ["Display aware", "Ten workspaces with independent lanes for every display."],
  ["Remembers windows", "Tabs can change underneath. Your layout stays intact."],
];

export function App() {
  const [workspace, setWorkspace] = useState(0);

  useEffect(() => {
    const interval = window.setInterval(() => {
      setWorkspace((current) => (current + 1) % workspaces.length);
    }, 3200);
    return () => window.clearInterval(interval);
  }, []);

  const active = workspaces[workspace];

  return (
    <main className="isolate min-h-dvh overflow-hidden bg-[#0c0d0f] text-[#f2f2f0] antialiased">
      <div className="page-grid min-h-dvh">
        <nav className="mx-auto flex w-full max-w-6xl items-center justify-between px-5 py-6 sm:px-8 lg:px-10">
          <a href="#top" className="flex items-center gap-3" aria-label="Spaceman home">
            <img src="/mark.svg" alt="" className="size-8 rounded-[9px]" />
            <span className="text-base font-medium tracking-[-0.01em]">Spaceman</span>
          </a>
          <a
            href="#download"
            className="text-base text-[#9d9da3] transition-colors duration-150 hover:text-white sm:text-sm"
          >
            Download
          </a>
        </nav>

        <section
          id="top"
          className="mx-auto grid w-full max-w-6xl items-center gap-14 px-5 pb-20 pt-14 sm:px-8 sm:pt-20 lg:grid-cols-[9fr_11fr] lg:gap-16 lg:px-10 lg:pb-28 lg:pt-24"
        >
          <div className="flex flex-col items-start gap-7">
            <div className="flex items-center gap-2 font-mono text-sm tracking-wide text-[#8f96ad]">
              <span className="size-1.5 rounded-full bg-[#8ea0ff] shadow-[0_0_12px_#8ea0ff]" />
              NATIVE MACOS WINDOW MANAGER
            </div>
            <div className="flex flex-col gap-5">
              <h1 className="max-w-[12ch] text-balance text-5xl font-medium tracking-tight sm:text-6xl lg:text-[4.5rem]">
                Windows in their place.
              </h1>
              <p className="max-w-[48ch] text-pretty text-lg text-[#a3a3a8] sm:text-base/7">
                A fast tiling window manager that keeps ten workspaces organized across your Mac’s displays.
              </p>
            </div>
            <div id="download" className="flex flex-col items-start gap-3">
              <a
                href={DOWNLOAD_URL}
                className="group flex items-center gap-2.5 rounded-lg bg-[#f0f0ee] py-3 pl-3 pr-4 text-base font-medium text-[#101114] shadow-[0_10px_35px_rgba(0,0,0,0.28)] transition-transform duration-150 hover:-translate-y-0.5 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[#8ea0ff] sm:text-sm"
              >
                <span className="grid size-7 place-items-center rounded-md bg-[#15171b] text-white">
                  ↓
                </span>
                Download for Apple silicon
              </a>
              <p className="text-base text-[#73747b] sm:text-sm">
                v0.1.1 · macOS 14+ · notarized by Apple
              </p>
            </div>
          </div>

          <WorkspaceDemo active={active} workspace={workspace} onSelect={setWorkspace} />
        </section>

        <section className="border-y border-white/8 bg-white/[0.018]">
          <div className="mx-auto grid w-full max-w-6xl gap-px px-5 sm:px-8 md:grid-cols-3 lg:px-10" role="list">
            {features.map(([title, description], index) => (
              <article
                key={title}
                role="listitem"
                className="flex flex-col gap-2 border-white/8 py-8 md:border-l md:px-7 md:first:border-l-0 md:first:pl-0 md:last:pr-0"
              >
                <div className="font-mono text-sm tabular-nums text-[#666a77]">0{index + 1}</div>
                <h2 className="text-lg font-medium">{title}</h2>
                <p className="max-w-[34ch] text-pretty text-base text-[#85868d] sm:text-sm/6">
                  {description}
                </p>
              </article>
            ))}
          </div>
        </section>

        <footer className="mx-auto flex w-full max-w-6xl flex-col gap-4 px-5 py-9 text-base text-[#66676e] sm:flex-row sm:items-center sm:justify-between sm:px-8 sm:text-sm lg:px-10">
          <p>Built for the Mac. Nothing leaves it.</p>
          <a href={DOWNLOAD_URL} className="transition-colors duration-150 hover:text-white">
            Download Spaceman ↓
          </a>
        </footer>
      </div>
    </main>
  );
}

function WorkspaceDemo({
  active,
  workspace,
  onSelect,
}: {
  active: (typeof workspaces)[number];
  workspace: number;
  onSelect: (workspace: number) => void;
}) {
  return (
    <div className="relative mx-auto w-full max-w-[620px]">
      <div className="absolute -inset-10 bg-[radial-gradient(circle,rgba(95,113,201,0.17),transparent_66%)] blur-2xl" />
      <div className="relative overflow-hidden rounded-[18px] border border-white/12 bg-[#121317] shadow-[0_30px_90px_rgba(0,0,0,0.5)]">
        <div className="flex h-11 items-center justify-between border-b border-white/8 px-4">
          <div className="flex gap-1.5" aria-hidden="true">
            <span className="size-2.5 rounded-full bg-white/15" />
            <span className="size-2.5 rounded-full bg-white/10" />
            <span className="size-2.5 rounded-full bg-white/10" />
          </div>
          <AnimatePresence mode="wait" initial={false}>
            <motion.div
              key={active.label}
              initial={{ opacity: 0, y: 4 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -4 }}
              transition={{ duration: 0.16, ease: "easeOut" }}
              className="font-mono text-sm text-[#777a85]"
            >
              Workspace {workspace + 1} · {active.label}
            </motion.div>
          </AnimatePresence>
          <div className="w-14" />
        </div>

        <div className="relative aspect-[16/10] overflow-hidden bg-[#0a0b0d]">
          <AnimatePresence initial={false}>
            {active.tiles.map((tile) => (
              <motion.div
                layout
                key={tile.id}
                initial={{ opacity: 0, scale: 0.985 }}
                animate={{ opacity: 1, scale: 1 }}
                exit={{ opacity: 0, scale: 0.985 }}
                transition={{
                  layout: { type: "spring", visualDuration: 0.42, bounce: 0.12 },
                  opacity: { duration: 0.18 },
                }}
                className={`absolute overflow-hidden rounded-[7px] border border-white/10 shadow-xl ${tile.className} ${tile.tone}`}
              >
                <div className="flex h-9 items-center gap-2 border-b border-white/7 px-3">
                  <span className="size-1.5 rounded-full bg-[#8ea0ff]" />
                  <span className="text-sm font-medium text-[#d8d8da]">{tile.name}</span>
                  <span className="truncate font-mono text-sm text-[#64666e]">{tile.detail}</span>
                </div>
                <WindowTexture id={tile.id} />
              </motion.div>
            ))}
          </AnimatePresence>
        </div>

        <div className="flex items-center justify-center gap-1.5 border-t border-white/8 px-4 py-3">
          {workspaces.map((item, index) => (
            <button
              key={item.label}
              type="button"
              onClick={() => onSelect(index)}
              className={`relative grid size-8 place-items-center rounded-md font-mono text-sm tabular-nums transition-colors duration-150 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[#8ea0ff] ${
                index === workspace
                  ? "text-white"
                  : "text-[#61636c] hover:bg-white/5 hover:text-[#a8a9af]"
              }`}
              aria-label={`Show workspace ${index + 1}: ${item.label}`}
              aria-pressed={index === workspace}
            >
              {index === workspace && (
                <motion.span
                  layoutId="active-workspace"
                  className="absolute inset-0 rounded-md bg-[#31384f]"
                  transition={{ type: "spring", visualDuration: 0.3, bounce: 0.1 }}
                />
              )}
              <span className="relative">{index + 1}</span>
              <span className="absolute left-1/2 top-1/2 size-[max(100%,3rem)] -translate-1/2 pointer-fine:hidden" aria-hidden="true" />
            </button>
          ))}
          <span className="mx-1 h-4 w-px bg-white/10" />
          <span className="font-mono text-sm text-[#4f515a]">⌥ 1–0</span>
        </div>
      </div>
    </div>
  );
}

function WindowTexture({ id }: { id: string }) {
  if (id === "terminal") {
    return (
      <div className="flex flex-col gap-2 p-3 font-mono text-sm text-[#6d9c7c]">
        <span>$ cargo test</span>
        <span className="text-[#5e6463]">141 passed; 0 failed</span>
      </div>
    );
  }

  if (id === "calendar") {
    return (
      <div className="grid grid-cols-4 gap-2 p-3">
        {[0, 1, 2, 3, 4, 5, 6, 7].map((item) => (
          <span key={item} className={`h-8 rounded ${item === 5 ? "bg-[#59689e]/60" : "bg-white/[0.035]"}`} />
        ))}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-2.5 p-3">
      <span className="h-2 w-3/5 rounded-full bg-white/10" />
      <span className="h-2 w-4/5 rounded-full bg-white/[0.055]" />
      <span className="h-2 w-2/5 rounded-full bg-white/[0.055]" />
      <span className="h-2 w-3/4 rounded-full bg-white/[0.055]" />
    </div>
  );
}
