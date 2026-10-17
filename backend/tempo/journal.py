"""Notes quotidiennes et création de notes à partir des modèles."""

import datetime as dt

from .index import Index
from .vault import Vault, VaultError, append_to_section, slugify

DEFAULT_TEMPLATES = {
    "journal": "---\ntype: journal\ndate: {{date}}\n---\n\n# Journal du {{date}}\n\n"
               "## Notes\n\n## Échanges\n\n## Fait aujourd'hui\n",
    "reunion": "---\ntype: reunion\ndate: {{date}}\nprojects: []\nparticipants: []\ntasks: []\n"
               "---\n\n# {{title}}\n\n## Participants\n\n## Points abordés\n\n## Décisions\n\n"
               "## Actions\n",
    "note": "---\ntype: {{type}}\ndate: {{date}}\ntags: []\n---\n\n# {{title}}\n",
}

# type de note -> dossier ; les réunions sont rangées par année
NOTE_FOLDERS = {"reunion": "reunions/{year}", "sujet": "sujets", "projet": "projets",
                "personne": "personnes", "note": "sujets"}


class JournalService:
    def __init__(self, vault: Vault, index: Index):
        self.vault = vault
        self.index = index

    def template(self, name: str) -> str:
        rel = f"_templates/{name}.md"
        if self.vault.exists(rel):
            return self.vault.read_text(rel)
        return DEFAULT_TEMPLATES.get(name, DEFAULT_TEMPLATES["note"])

    @staticmethod
    def render(template: str, **values: str) -> str:
        for key, value in values.items():
            template = template.replace("{{" + key + "}}", value)
        return template

    @staticmethod
    def path_for(date: str) -> str:
        return f"journal/{date[:4]}/{date}.md"

    def ensure(self, date: str | None = None) -> str:
        date = date or dt.date.today().isoformat()
        dt.date.fromisoformat(date)
        rel = self.path_for(date)
        if not self.vault.exists(rel):
            self.vault.write(rel, self.render(self.template("journal"), date=date))
            self.index.upsert(rel)
        return rel

    def append(self, date: str, heading: str, line: str) -> str:
        rel = self.ensure(date)
        text = self.vault.read_text(rel)
        self.vault.write(rel, append_to_section(text, heading, line))
        self.index.upsert(rel)
        return rel

    def create_note(self, type: str, title: str, date: str | None = None) -> str:
        if type == "journal":
            return self.ensure(date)
        if type not in NOTE_FOLDERS:
            raise VaultError(f"Type de note inconnu : {type}")
        title = title.strip()
        if not title:
            raise VaultError("Le titre est obligatoire")
        date = date or dt.date.today().isoformat()
        folder = NOTE_FOLDERS[type].format(year=date[:4])
        stem = f"{date}-{slugify(title)}" if type == "reunion" else slugify(title)
        rel = self.vault.unique_path(folder, stem)
        template = self.template("reunion" if type == "reunion" else "note")
        self.vault.write(rel, self.render(template, date=date, title=title, type=type),
                         create_only=True)
        self.index.upsert(rel)
        return rel
