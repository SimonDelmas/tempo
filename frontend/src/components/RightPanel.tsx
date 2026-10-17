import { useEffect, useState } from "react";
import { api, NoteSummary, TYPE_LABELS } from "../api";

type Props = { path: string; revision: number; onOpen: (path: string) => void };

function NoteList({ title, notes, onOpen }: { title: string; notes: NoteSummary[]; onOpen: (p: string) => void }) {
  return (
    <section>
      <h4>{title} <span className="count">{notes.length}</span></h4>
      {notes.length === 0 && <p className="muted small">Aucun</p>}
      <ul className="link-list">
        {notes.map((n) => (
          <li key={n.path}>
            <button className="link" onClick={() => onOpen(n.path)} title={n.path}>
              {n.alias ? `${n.alias} · ` : ""}{n.title}
            </button>
            <span className="muted small">{TYPE_LABELS[n.type] ?? n.type}{n.date ? ` · ${n.date}` : ""}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

export default function RightPanel({ path, revision, onOpen }: Props) {
  const [links, setLinks] = useState<{ backlinks: NoteSummary[]; outlinks: NoteSummary[] }>({
    backlinks: [], outlinks: [],
  });

  useEffect(() => {
    api.links(path).then(setLinks).catch(() => setLinks({ backlinks: [], outlinks: [] }));
  }, [path, revision]);

  const tasks = links.outlinks.filter((n) => n.type === "tache");
  const others = links.outlinks.filter((n) => n.type !== "tache");
  return (
    <aside className="right-panel">
      <NoteList title="Tâches liées" notes={tasks} onOpen={onOpen} />
      <NoteList title="Mentionné dans" notes={links.backlinks} onOpen={onOpen} />
      <NoteList title="Liens sortants" notes={others} onOpen={onOpen} />
    </aside>
  );
}
