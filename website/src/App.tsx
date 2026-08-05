import { AnimatePresence, motion } from "motion/react";
import { useEffect, useState } from "react";

const DOWNLOAD_URL =
  "https://pub-6f85d683ffe0419cad0a71508904053a.r2.dev/releases/Spaceman-v0.1.1-macos-arm64.zip";

type Tile = {
  id: string;
  className: string;
};

const workspaces: Tile[][] = [
  [
    { id: "a", className: "left-[2.5%] top-[3.5%] h-[93%] w-[60%]" },
    { id: "b", className: "left-[64.5%] top-[3.5%] h-[45%] w-[33%]" },
    { id: "c", className: "left-[64.5%] top-[51.5%] h-[45%] w-[33%]" },
  ],
  [
    { id: "b", className: "left-[2.5%] top-[3.5%] h-[93%] w-[66%]" },
    { id: "d", className: "left-[70.5%] top-[3.5%] h-[93%] w-[27%]" },
  ],
  [
    { id: "e", className: "left-[2.5%] top-[3.5%] h-[93%] w-[46.75%]" },
    { id: "a", className: "left-[51.25%] top-[3.5%] h-[93%] w-[46.75%]" },
  ],
];

export function App() {
  const [workspace, setWorkspace] = useState(0);

  useEffect(() => {
    const interval = window.setInterval(() => {
      setWorkspace((current) => (current + 1) % workspaces.length);
    }, 2800);
    return () => window.clearInterval(interval);
  }, []);

  return (
    <main className="flex min-h-dvh flex-col items-center justify-center gap-10 bg-[#0b0c0e] px-6 py-16 text-[#f2f2f0] antialiased">
      <header className="flex flex-col items-center gap-5">
        <img src="/mark.svg" alt="" className="size-14 rounded-[14px] shadow-lg" />
        <div className="flex flex-col items-center gap-2 text-center">
          <h1 className="text-2xl font-medium tracking-tight">Spaceman</h1>
          <p className="text-base text-[#87888e]">A tiling window manager for macOS.</p>
        </div>
      </header>

      <WorkspaceDemo workspace={workspace} onSelect={setWorkspace} />

      <footer className="flex flex-col items-center gap-3">
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
  return (
    <div className="flex w-full max-w-[520px] flex-col items-center gap-4">
      <div className="relative aspect-[16/10] w-full overflow-hidden rounded-xl border border-white/10 bg-[#101114] shadow-[0_20px_60px_rgba(0,0,0,0.45)]">
        <AnimatePresence initial={false}>
          {workspaces[workspace].map((tile) => (
            <motion.div
              layout
              key={tile.id}
              initial={{ opacity: 0, scale: 0.98 }}
              animate={{ opacity: 1, scale: 1 }}
              exit={{ opacity: 0, scale: 0.98 }}
              transition={{
                layout: { type: "spring", visualDuration: 0.4, bounce: 0.12 },
                opacity: { duration: 0.15 },
              }}
              className={`absolute rounded-md bg-white/[0.07] ${tile.className}`}
            />
          ))}
        </AnimatePresence>
      </div>

      <div className="flex items-center gap-1" role="tablist" aria-label="Workspaces">
        {workspaces.map((_, index) => (
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
