//! Notes quotidiennes et création de notes à partir des modèles.

use std::sync::Arc;

use crate::error::{vault_err, Result};
use crate::index::Index;
use crate::util::{parse_date, today};
use crate::vault::{append_to_section, slugify, Vault};

const JOURNAL_TEMPLATE: &str =
    "---\ntype: journal\ndate: {{date}}\n---\n\n# Journal du {{date}}\n\n## Notes\n\n## Échanges\n\n## Fait aujourd'hui\n";
const REUNION_TEMPLATE: &str = "---\ntype: reunion\ndate: {{date}}\nprojects: []\nparticipants: []\ntasks: []\n\
                                ---\n\n# {{title}}\n\n## Participants\n\n## Points abordés\n\n## Décisions\n\n## Actions\n";
const NOTE_TEMPLATE: &str = "---\ntype: {{type}}\ndate: {{date}}\ntags: []\n---\n\n# {{title}}\n";

/// type de note -> dossier ; les réunions sont rangées par année
const NOTE_FOLDERS: [(&str, &str); 7] = [
    ("reunion", "reunions/{year}"),
    ("sujet", "sujets"),
    ("projet", "projets"),
    ("personne", "personnes"),
    ("ressource", "ressources"),
    ("formation", "formations"),
    ("note", "sujets"),
];

pub fn render(template: &str, values: &[(&str, &str)]) -> String {
    values.iter().fold(template.to_string(), |text, (key, value)| text.replace(&format!("{{{{{key}}}}}"), value))
}

pub fn path_for(date: &str) -> String {
    format!("journal/{}/{date}.md", &date[..4])
}

pub struct JournalService {
    vault: Arc<Vault>,
    index: Arc<Index>,
}

impl JournalService {
    pub fn new(vault: Arc<Vault>, index: Arc<Index>) -> Self {
        JournalService { vault, index }
    }

    /// Modèle du vault (`_templates/<nom>.md`) ou modèle par défaut.
    pub fn template(&self, name: &str) -> Result<String> {
        let rel = format!("_templates/{name}.md");
        if self.vault.exists(&rel) {
            return self.vault.read_text(&rel);
        }
        Ok(match name {
            "journal" => JOURNAL_TEMPLATE,
            "reunion" => REUNION_TEMPLATE,
            _ => NOTE_TEMPLATE,
        }
        .to_string())
    }

    /// Crée si besoin la note du jour ; renvoie son chemin.
    pub fn ensure(&self, date: Option<&str>) -> Result<String> {
        let date = match date.filter(|d| !d.is_empty()) {
            Some(d) => parse_date(d)?.to_string(),
            None => today().to_string(),
        };
        let rel = path_for(&date);
        if !self.vault.exists(&rel) {
            self.vault.write(&rel, &render(&self.template("journal")?, &[("date", &date)]), None, false)?;
            self.index.upsert(&rel)?;
        }
        Ok(rel)
    }

    pub fn append(&self, date: &str, heading: &str, line: &str) -> Result<String> {
        let rel = self.ensure(Some(date))?;
        let text = self.vault.read_text(&rel)?;
        self.vault.write(&rel, &append_to_section(&text, heading, line), None, false)?;
        self.index.upsert(&rel)?;
        Ok(rel)
    }

    pub fn create_note(&self, kind: &str, title: &str, date: Option<&str>) -> Result<String> {
        if kind == "journal" {
            return self.ensure(date);
        }
        let Some((_, folder)) = NOTE_FOLDERS.iter().find(|(k, _)| *k == kind) else {
            return Err(vault_err(format!("Type de note inconnu : {kind}")));
        };
        let title = title.trim();
        if title.is_empty() {
            return Err(vault_err("Le titre est obligatoire"));
        }
        let date = match date.filter(|d| !d.is_empty()) {
            Some(d) => parse_date(d)?.to_string(),
            None => today().to_string(),
        };
        let folder = folder.replace("{year}", &date[..4]);
        let stem = if kind == "reunion" { format!("{date}-{}", slugify(title, 50)) } else { slugify(title, 50) };
        let rel = self.vault.unique_path(&folder, &stem)?;
        let template = self.template(if kind == "reunion" { "reunion" } else { "note" })?;
        self.vault.write(&rel, &render(&template, &[("date", &date), ("title", title), ("type", kind)]), None, true)?;
        self.index.upsert(&rel)?;
        Ok(rel)
    }
}
