//! Notes quotidiennes et création de notes à partir des modèles.

use std::sync::{Arc, LazyLock};

use regex::Regex;
use serde_json::Value;

use crate::error::{vault_err, Result};
use crate::index::Index;
use crate::llm::strip_fences;
use crate::util::{parse_date, today};
use crate::vault::{append_to_section, join_frontmatter, split_frontmatter, Vault};

static H1_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^#[ \t]+[^\r\n]*").unwrap());

const JOURNAL_TEMPLATE: &str =
    "---\ntype: journal\ndate: {{date}}\n---\n\n# Journal du {{date}}\n\n## Notes\n\n## Échanges\n\n## Fait aujourd'hui\n";
const REUNION_TEMPLATE: &str = "---\ntype: reunion\ndate: {{date}}\nprojects: []\nparticipants: []\ntasks: []\n\
                                ---\n\n# {{title}}\n\n## Participants\n\n## Points abordés\n\n## Décisions\n\n## Actions\n";
const NOTE_TEMPLATE: &str = "---\ntype: {{type}}\ndate: {{date}}\ntags: []\n---\n\n# {{title}}\n";

/// type de note -> dossier ; les réunions sont rangées par année
const NOTE_FOLDERS: [(&str, &str); 8] = [
    ("reunion", "reunions/{year}"),
    ("sujet", "sujets"),
    ("projet", "projets"),
    ("personne", "personnes"),
    ("ressource", "ressources"),
    ("formation", "formations"),
    ("synthese", "syntheses"),
    ("note", "sujets"),
];

/// Types de notes que l'on peut créer (hors journal).
pub const NOTE_KINDS: [&str; 8] = ["note", "sujet", "reunion", "projet", "personne", "ressource", "formation", "synthese"];

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

    pub fn create_note(&self, kind: &str, title: &str, date: Option<&str>, projects: &[String]) -> Result<String> {
        self.create_note_with_body(kind, title, date, None, projects)
    }

    /// Comme [`Self::create_note`] ; `body` remplace ce qui suit le titre dans le modèle et
    /// `projects` (identifiants de projets) renseigne la liste `projects` du frontmatter.
    pub fn create_note_with_body(
        &self,
        kind: &str,
        title: &str,
        date: Option<&str>,
        body: Option<&str>,
        projects: &[String],
    ) -> Result<String> {
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
        let name = file_name(title);
        let stem = if kind == "reunion" { format!("{date} {name}") } else { name.clone() };
        let rel = self.vault.unique_path(&folder, &stem)?;
        // le suffixe anti-doublon (« -2 ») fait partie du titre, pour rester identique au fichier
        let file_stem = rel.rsplit('/').next().unwrap_or(&rel).trim_end_matches(".md");
        let title = if kind == "reunion" { name.as_str() } else { file_stem };
        let template = self.template(if kind == "reunion" { "reunion" } else { "note" })?;
        let text = render(&template, &[("date", &date), ("title", title), ("type", kind)]);
        let text = with_title(&text, title);
        // le LLM enveloppe parfois tout le contenu dans une clôture ```markdown
        let body = body.map(strip_fences);
        let text = match body.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
            Some(body) => with_body(&text, body),
            None => text,
        };
        let text = with_projects(&text, projects);
        self.vault.write(&rel, &text, None, true)?;
        self.index.upsert(&rel)?;
        Ok(rel)
    }
}

/// Nom de fichier tiré du titre : caractères interdits par Windows ou par les
/// `[[wiki-links]]` remplacés par des espaces, espaces fusionnées, 100 caractères au plus.
pub fn file_name(title: &str) -> String {
    let cleaned: String =
        title.chars().map(|c| if c.is_control() || r#"\/:*?"<>|#^[]"#.contains(c) { ' ' } else { c }).collect();
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = joined.chars().take(100).collect();
    let cut = cut.trim_end_matches(['.', ' ']);
    if cut.is_empty() { "note".into() } else { cut.into() }
}

/// Force le `title` du frontmatter et le premier titre de niveau 1, quel que soit le modèle.
fn with_title(text: &str, title: &str) -> String {
    let (mut meta, body) = split_frontmatter(text);
    meta.insert("title".into(), Value::String(title.into()));
    let heading = format!("# {title}");
    let body = match H1_RE.find(body) {
        Some(m) => format!("{}{heading}{}", &body[..m.start()], &body[m.end()..]),
        None => format!("\n{heading}\n{}", body.trim_start_matches(['\r', '\n'])),
    };
    join_frontmatter(&meta, &body)
}

/// Renseigne la liste `projects` du frontmatter (à sa place si le modèle la prévoit déjà) ;
/// sans projet, la note reste telle que le modèle la donne.
fn with_projects(text: &str, projects: &[String]) -> String {
    let mut ids: Vec<&str> = Vec::new();
    for id in projects.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return text.to_string();
    }
    let (mut meta, body) = split_frontmatter(text);
    meta.insert("projects".into(), Value::from(ids));
    join_frontmatter(&meta, body)
}

/// Remplace ce qui suit le titre de niveau 1 (posé par [`with_title`]) par `body`.
fn with_body(text: &str, body: &str) -> String {
    let (meta, current) = split_frontmatter(text);
    let kept = H1_RE.find(current).map_or("", |m| &current[..m.end()]);
    join_frontmatter(&meta, &format!("{kept}\n\n{body}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_garde_le_titre() {
        assert_eq!(file_name("Réunion d'équipe"), "Réunion d'équipe");
        assert_eq!(file_name("  SAP / FI : clôture?  "), "SAP FI clôture");
        assert_eq!(file_name("[[x]]."), "x");
        assert_eq!(file_name("///"), "note");
    }

    #[test]
    fn with_title_remplace_le_titre_du_modele() {
        let text = "---\ntype: note\ntitle: Modèle de note\n---\n\n# Modèle de note\n\n## Suite\n";
        assert_eq!(with_title(text, "Ma note"), "---\ntype: note\ntitle: Ma note\n---\n\n# Ma note\n\n## Suite\n");
        let sans_titre = "---\ntype: note\n---\n\nTexte\n";
        assert_eq!(with_title(sans_titre, "Ma note"), "---\ntype: note\ntitle: Ma note\n---\n\n# Ma note\nTexte\n");
    }

    #[test]
    fn with_projects_renseigne_le_frontmatter() {
        let text = "---\ntype: reunion\nprojects: []\ntitle: Point\n---\n\n# Point\n";
        let ids = vec!["sap".to_string(), " sap ".to_string(), "crm".to_string()];
        assert_eq!(with_projects(text, &ids), "---\ntype: reunion\nprojects: [sap, crm]\ntitle: Point\n---\n\n# Point\n");
        let note = "---\ntype: note\ntitle: X\n---\n\n# X\n";
        assert_eq!(with_projects(note, &["sap".into()]), "---\ntype: note\ntitle: X\nprojects: [sap]\n---\n\n# X\n");
        assert_eq!(with_projects(note, &[]), note);
    }

    #[test]
    fn with_body_remplace_le_corps_du_modele() {
        let text = "---\ntype: reunion\ntitle: Point\n---\n\n# Point\n\n## Participants\n";
        assert_eq!(with_body(text, "- a"), "---\ntype: reunion\ntitle: Point\n---\n\n# Point\n\n- a\n");
    }

    #[test]
    fn corps_enveloppe_dans_une_cloture_markdown() {
        assert_eq!(strip_fences("```markdown\n## A\n- a\n```"), "## A\n- a");
    }
}
