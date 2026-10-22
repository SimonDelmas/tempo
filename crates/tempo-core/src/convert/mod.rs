//! Conversion des documents de la bibliothèque en Markdown (format pivot).
//!
//! Un convertisseur par famille de formats : pandoc (docx, odt, rtf, html, epub), lecture du
//! zip (pptx), calamine (tableurs), pdfium (PDF), mail-parser et cfb (mails), cfb (.doc et .ppt
//! d'Office 97-2003). Seul le texte est gardé (titres, listes, tableaux) : images et schémas
//! sont perdus. Ni Python ni Office ne sont nécessaires.

mod legacy;
mod mail;
mod office;
mod pdf;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;

use regex::Regex;

use crate::config::project_root;
use crate::error::{vault_err, Error, Result};
use crate::vault::{meta_str, split_frontmatter};

pub use mail::Mail;

/// Extensions acceptées, dans l'ordre d'affichage.
pub const FORMATS: [&str; 19] = [
    "pdf", "docx", "doc", "pptx", "ppt", "xlsx", "xlsm", "xls", "ods", "odt", "rtf", "html", "htm", "epub", "md",
    "markdown", "txt", "eml", "msg",
];

static IMAGE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!\[([^\]]*)\]\([^)]*\)").unwrap());
static ANCHOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<(?:span|a) id="[^"]*"(?: class="[^"]*")?>\s*</(?:span|a)>"#).unwrap());
static BLANK_LINES_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n[ \t]*\n(?:[ \t]*\n)+").unwrap());

/// Format d'un fichier d'après son extension (`htm` → `html`, `markdown` → `md`), s'il est pris en charge.
pub fn format_of(name: &str) -> Option<&'static str> {
    let ext = Path::new(name).extension()?.to_string_lossy().to_lowercase();
    match ext.as_str() {
        "htm" => Some("html"),
        "markdown" => Some("md"),
        other => FORMATS.iter().find(|f| **f == other).copied(),
    }
}

/// Outils externes trouvés sur le poste.
#[derive(Debug, Clone, Default)]
pub struct Tools {
    pub pandoc: Option<PathBuf>,
    pub pdfium: Option<PathBuf>,
}

impl Tools {
    /// Cherche pandoc et pdfium : chemin réglé (`TEMPO_PANDOC`, `TEMPO_PDFIUM`), puis à côté de
    /// l'exécutable (ressources livrées par l'installeur), puis dans `src-tauri/resources/` du
    /// dépôt (développement), puis dans le PATH.
    pub fn discover(pandoc: Option<&Path>, pdfium: Option<&Path>) -> Tools {
        Tools { pandoc: find_tool(pandoc, "pandoc.exe"), pdfium: find_tool(pdfium, "pdfium.dll") }
    }
}

fn find_tool(configured: Option<&Path>, name: &str) -> Option<PathBuf> {
    if let Some(path) = configured {
        return Some(if path.is_dir() { path.join(name) } else { path.to_path_buf() });
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        dirs.push(exe_dir.join("resources"));
        dirs.push(exe_dir);
    }
    if let Some(root) = project_root() {
        dirs.push(root.join("src-tauri").join("resources"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Résultat d'une conversion.
#[derive(Debug, Clone, Default)]
pub struct Converted {
    pub markdown: String,
    /// titre trouvé dans le document (propriétés, sujet d'un mail…)
    pub title: Option<String>,
    /// pages d'un PDF, diapositives d'une présentation
    pub pages: Option<usize>,
    /// pièces jointes d'un mail : (nom de fichier, contenu)
    pub attachments: Vec<(String, Vec<u8>)>,
}

/// Convertit le contenu d'un fichier de format `format` (voir [`format_of`]).
pub fn convert(bytes: &[u8], format: &str, tools: &Tools) -> Result<Converted> {
    let mut converted = match format {
        "md" | "txt" => {
            let text = decode_text(bytes);
            let (meta, body) = split_frontmatter(&text);
            Converted { title: meta_str(&meta, "title"), markdown: body.to_string(), ..Default::default() }
        }
        "docx" | "odt" | "rtf" | "html" | "epub" => {
            let title = if format == "docx" { office::office_title(bytes) } else { None };
            Converted { markdown: pandoc(bytes, format, tools)?, title, ..Default::default() }
        }
        "pptx" => {
            let (markdown, slides, title) = office::pptx_to_markdown(bytes)?;
            Converted { markdown, pages: Some(slides), title, ..Default::default() }
        }
        "xlsx" | "xlsm" | "xls" | "ods" => Converted { markdown: office::spreadsheet_to_markdown(bytes)?, ..Default::default() },
        "pdf" => {
            let path = tools.pdfium.as_deref().ok_or_else(|| {
                vault_err("pdfium.dll introuvable : lancez scripts/telecharger-outils.ps1 ou renseignez TEMPO_PDFIUM")
            })?;
            let (markdown, pages, title) = pdf::pdf_to_markdown(bytes, path)?;
            Converted { markdown, pages: Some(pages), title, ..Default::default() }
        }
        "eml" | "msg" => {
            let mail = if format == "eml" { mail::parse_eml(bytes)? } else { mail::parse_msg(bytes)? };
            let markdown = mail::mail_to_markdown(&mail, &|html: &str| pandoc(html.as_bytes(), "html", tools));
            let title = Some(mail.subject.trim().to_string()).filter(|s| !s.is_empty());
            Converted { markdown, title, attachments: mail.attachments, ..Default::default() }
        }
        "doc" => Converted { markdown: legacy::doc_to_markdown(bytes)?, ..Default::default() },
        "ppt" => {
            let (markdown, slides) = legacy::ppt_to_markdown(bytes)?;
            Converted { markdown, pages: Some(slides), ..Default::default() }
        }
        other => return Err(vault_err(format!("Format non pris en charge : {other}"))),
    };
    converted.markdown = tidy(&converted.markdown);
    if converted.markdown.trim().is_empty() {
        return Err(vault_err(match format {
            "pdf" => "Aucun texte dans ce PDF (document scanné ?) : la reconnaissance de caractères n'est pas gérée",
            _ => "Aucun texte n'a pu être extrait de ce document",
        }));
    }
    Ok(converted)
}

/// Texte UTF-8 (BOM retiré), sinon Windows-1252.
pub fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

/// Fins de ligne `\n`, espaces de fin de ligne et lignes vides multiples retirés.
fn tidy(markdown: &str) -> String {
    let text = markdown.replace("\r\n", "\n").replace('\r', "\n");
    let text: String = text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
    let text = BLANK_LINES_RE.replace_all(&text, "\n\n");
    format!("{}\n", text.trim_matches('\n'))
}

/// Conversion par pandoc (Markdown GitHub, sans retour à la ligne forcé) ; les images
/// deviennent leur texte alternatif et les ancres vides de Word disparaissent.
fn pandoc(bytes: &[u8], format: &str, tools: &Tools) -> Result<String> {
    let exe = tools.pandoc.as_deref().ok_or_else(|| {
        vault_err("pandoc introuvable : lancez scripts/telecharger-outils.ps1 ou renseignez TEMPO_PANDOC")
    })?;
    // pandoc lit les formats binaires (docx, odt, epub) depuis un fichier, pas depuis l'entrée standard
    let input = std::env::temp_dir().join(format!("tempo-{}.{format}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&input, bytes)?;
    let mut command = Command::new(exe);
    command
        .arg(&input)
        .args(["-f", format, "-t", "gfm", "--wrap=none"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut command);
    let output = command.output();
    let _ = std::fs::remove_file(&input);
    let output = output.map_err(|e| Error::Internal(format!("Lancement de pandoc impossible ({}) : {e}", exe.display())))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(vault_err(format!("Conversion par pandoc impossible : {}", detail.trim())));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let text = IMAGE_RE.replace_all(&text, "$1");
    Ok(ANCHOR_RE.replace_all(&text, "").into_owned())
}

/// Sous Windows, une application sans console qui lance un programme en ligne de commande
/// ouvrirait une fenêtre noire : on la masque.
fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_par_extension() {
        assert_eq!(format_of("Rapport annuel.PDF"), Some("pdf"));
        assert_eq!(format_of("page.htm"), Some("html"));
        assert_eq!(format_of("notes.markdown"), Some("md"));
        assert_eq!(format_of("archive.zip"), None);
        assert_eq!(format_of("sans-extension"), None);
    }

    #[test]
    fn markdown_et_texte() {
        let tools = Tools::default();
        let md = convert("---\ntitle: Guide\n---\n\n# Guide\r\n\r\n\r\n\r\nTexte  \n".as_bytes(), "md", &tools).unwrap();
        assert_eq!(md.title.as_deref(), Some("Guide"));
        assert_eq!(md.markdown, "# Guide\n\nTexte\n");
        // Windows-1252 à défaut d'UTF-8
        let txt = convert(b"Ann\xe9e fiscale", "txt", &tools).unwrap();
        assert_eq!(txt.markdown, "Année fiscale\n");
        assert!(convert(b"  \n", "txt", &tools).is_err());
    }

    #[test]
    fn outils_absents() {
        let tools = Tools::default();
        let err = convert(b"%PDF-1.4", "pdf", &tools).unwrap_err().to_string();
        assert!(err.contains("pdfium.dll introuvable"), "{err}");
        let err = convert(b"PK", "docx", &tools).unwrap_err().to_string();
        assert!(err.contains("pandoc introuvable"), "{err}");
    }

    #[test]
    fn docx_et_html_par_pandoc() {
        let tools = Tools::discover(None, None);
        let Some(exe) = tools.pandoc.clone() else {
            eprintln!("pandoc absent : test sauté (scripts/telecharger-outils.ps1)");
            return;
        };
        // docx produit par pandoc lui-même à partir d'un Markdown
        let docx = std::env::temp_dir().join(format!("tempo-test-{}.docx", uuid::Uuid::new_v4().simple()));
        let source = "---
title: Cahier
---

# Cahier des charges\n\nTexte avec ![logo](logo.png) une image.\n\n| Lot | Date |\n|-----|------|\n| 1 | mars |\n";
        let mut child = Command::new(&exe)
            .args(["-f", "markdown", "-o"])
            .arg(&docx)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), source.as_bytes()).unwrap();
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        let bytes = std::fs::read(&docx).unwrap();
        let _ = std::fs::remove_file(&docx);
        let converted = convert(&bytes, "docx", &tools).unwrap();
        assert!(converted.markdown.starts_with("# Cahier des charges\n"), "{}", converted.markdown);
        assert!(converted.markdown.contains("| Lot | Date |"), "{}", converted.markdown);
        assert!(!converted.markdown.contains("!["), "{}", converted.markdown);

        let html = convert("<html><body><h2>Accès</h2><p>Voir <b>la procédure</b>.</p><script>x()</script></body></html>".as_bytes(), "html", &tools)
            .unwrap();
        assert_eq!(html.markdown, "## Accès\n\nVoir **la procédure**.\n");
    }

    #[test]
    fn nettoyage_de_pandoc() {
        let text = "Avant ![Logo de la société](media/image1.png) après <span id=\"_Toc1\" class=\"anchor\"></span>fin";
        let text = IMAGE_RE.replace_all(text, "$1");
        assert_eq!(ANCHOR_RE.replace_all(&text, ""), "Avant Logo de la société après fin");
    }
}
