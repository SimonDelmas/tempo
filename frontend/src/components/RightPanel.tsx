import { useEffect, useState } from "react";
import { api, NoteSummary, SemanticHit, TYPE_LABELS } from "../api";

type Props = {
  path: string;
  revision: number;
  /** recherche sémantique disponible : suggestions de liens */
  semantic: boolean;
  onOpen: (path: string) => void;
  /** ajoute un lien [[name]] à la note ouverte */
  onLink: (name: string) => void;
};

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

function Suggestions({ notes, onOpen, onLink }: {
  notes: SemanticHit[]; onOpen: (p: string) => void; onLink: (name: string) => void;
}) {
  return (
    <section>
      <h4 title="Notes proches par le sens et pas encore liées">Suggestions de liens</h4>
      {notes.length === 0 && <p className="muted small">Aucune</p>}
      <ul className="link-list">
        {notes.map((n) => (
          <li key={n.path} className="suggestion">
            <div className="suggestion-head">
              <button className="link" onClick={() => onOpen(n.path)} title={n.snippet}>
                {n.alias ? `${n.alias} · ` : ""}{n.title}
              </button>
              <button className="ghost mini" title="Ajouter le lien à la fin de la note"
                onClick={() => onLink(n.alias || n.stem)}>＋</button>
            </div>
            <span className="muted small">
              {TYPE_LABELS[n.type] ?? n.type} · proximité {Math.round(n.score * 100)} %
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}

export default function RightPanel({ path, revision, semantic, onOpen, onLink }: Props) {
  const [links, setLinks] = useState<{ backlinks: NoteSummary[]; outlinks: NoteSummary[] }>({
    backlinks: [], outlinks: [],
  });
  const [related, setRelated] = useState<SemanticHit[]>([]);

  useEffect(() => {
    api.links(path).then(setLinks).catch(() => setLinks({ backlinks: [], outlinks: [] }));
  }, [path, revision]);

  useEffect(() => {
    if (!semantic) return;
    api.semanticRelated(path).then((hits) => setRelated(hits.slice(0, 6))).catch(() => setRelated([]));
  }, [path, revision, semantic]);

  const tasks = links.outlinks.filter((n) => n.type === "tache");
  const others = links.outlinks.filter((n) => n.type !== "tache");
  return (
    <aside className="right-panel">
      <NoteList title="Tâches liées" notes={tasks} onOpen={onOpen} />
      <NoteList title="Mentionné dans" notes={links.backlinks} onOpen={onOpen} />
      <NoteList title="Liens sortants" notes={others} onOpen={onOpen} />
      {semantic && <Suggestions notes={related} onOpen={onOpen} onLink={onLink} />}
    </aside>
  );
}
