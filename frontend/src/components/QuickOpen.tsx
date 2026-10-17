import { useMemo, useState } from "react";
import { NoteSummary, TYPE_LABELS } from "../api";

type Props = { files: NoteSummary[]; onOpen: (path: string) => void; onClose: () => void };

export default function QuickOpen({ files, onOpen, onClose }: Props) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);

  const matches = useMemo(() => {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    return files
      .filter((f) => f.type !== "modele")
      .filter((f) => words.every((w) => `${f.alias} ${f.title} ${f.path}`.toLowerCase().includes(w)))
      .sort((a, b) => (b.date || "").localeCompare(a.date || ""))
      .slice(0, 30);
  }, [files, query]);

  const choose = (file?: NoteSummary) => {
    if (!file) return;
    onOpen(file.path);
    onClose();
  };

  return (
    <div className="modal-backdrop" onMouseDown={onClose}>
      <div className="modal quick-open" onMouseDown={(e) => e.stopPropagation()}>
        <input autoFocus placeholder="Ouvrir une note…" value={query}
          onChange={(e) => { setQuery(e.target.value); setSelected(0); }}
          onKeyDown={(e) => {
            if (e.key === "Escape") onClose();
            if (e.key === "ArrowDown") { e.preventDefault(); setSelected((s) => Math.min(s + 1, matches.length - 1)); }
            if (e.key === "ArrowUp") { e.preventDefault(); setSelected((s) => Math.max(s - 1, 0)); }
            if (e.key === "Enter") choose(matches[selected]);
          }} />
        <ul>
          {matches.map((f, i) => (
            <li key={f.path} className={i === selected ? "selected" : ""} onMouseEnter={() => setSelected(i)}
              onClick={() => choose(f)}>
              <span>{f.alias ? `${f.alias} · ` : ""}{f.title}</span>
              <span className="muted small">{TYPE_LABELS[f.type] ?? f.type} · {f.path}</span>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
