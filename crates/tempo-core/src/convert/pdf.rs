//! PDF : texte page par page extrait par pdfium (`pdfium.dll`, chargée une fois par processus).

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use pdfium_render::prelude::*;
use regex::Regex;

use crate::error::{vault_err, Result};

/// Instance de pdfium : la bibliothèque ne peut être liée qu'une fois par processus ; l'échec
/// est mémorisé avec le chemin essayé, pour retenter si le réglage change.
static PDFIUM: Mutex<Option<(PathBuf, std::result::Result<Pdfium, String>)>> = Mutex::new(None);

static SENTENCE_END_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[.:;!?…»)\]]$").unwrap());

/// Lie pdfium (si besoin) puis exécute `f` ; les conversions PDF sont ainsi sérialisées.
fn with_pdfium<T>(path: &Path, f: impl FnOnce(&Pdfium) -> Result<T>) -> Result<T> {
    let mut guard = PDFIUM.lock().unwrap_or_else(|e| e.into_inner());
    let retry = match guard.as_ref() {
        None => true,
        Some((tried, Err(_))) => tried != path,
        Some((_, Ok(_))) => false,
    };
    if retry {
        let bound = Pdfium::bind_to_library(path)
            .map(Pdfium::new)
            .map_err(|e| format!("Chargement de pdfium impossible ({}) : {e:?}", path.display()));
        *guard = Some((path.to_path_buf(), bound));
    }
    match guard.as_ref() {
        Some((_, Ok(pdfium))) => f(pdfium),
        Some((_, Err(message))) => Err(vault_err(message.clone())),
        None => Err(vault_err("pdfium indisponible")),
    }
}

/// Recolle les lignes d'une page en paragraphes : une ligne vide sépare deux paragraphes, une
/// ligne qui ne termine pas une phrase se poursuit sur la suivante (césure retirée).
pub fn page_paragraphs(text: &str) -> Vec<String> {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut lines_in_current = 0;
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            if !current.is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
            continue;
        }
        if current.is_empty() {
            current = line.to_string();
            lines_in_current = 1;
            continue;
        }
        let lower_next = line.chars().next().is_some_and(char::is_lowercase);
        // une ligne courte isolée suivie d'une majuscule est sans doute un titre
        let title_like = lines_in_current == 1 && current.chars().count() <= 60 && !lower_next;
        let continues = !title_like && !SENTENCE_END_RE.is_match(&current) && !line.starts_with(['•', '-', '–', '▪', '●']);
        if !continues {
            paragraphs.push(std::mem::replace(&mut current, line.to_string()));
            lines_in_current = 1;
            continue;
        }
        lines_in_current += 1;
        let hyphenated = current.ends_with('-') && current.chars().rev().nth(1).is_some_and(char::is_alphabetic);
        if hyphenated && lower_next {
            current.pop();
            current.push_str(line);
        } else {
            current.push(' ');
            current.push_str(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }
    paragraphs
}

/// PDF en Markdown : un marqueur `<!-- page N -->` avant le texte de chaque page ; renvoie
/// aussi le nombre de pages et le titre des métadonnées.
pub fn pdf_to_markdown(bytes: &[u8], pdfium_path: &Path) -> Result<(String, usize, Option<String>)> {
    with_pdfium(pdfium_path, |pdfium| {
        let document = pdfium.load_pdf_from_byte_slice(bytes, None).map_err(|e| {
            let detail = format!("{e:?}");
            if detail.contains("Password") {
                vault_err("PDF protégé par mot de passe")
            } else {
                vault_err(format!("PDF illisible : {detail}"))
            }
        })?;
        let title = document
            .metadata()
            .get(PdfDocumentMetadataTagType::Title)
            .map(|t| t.value().trim().to_string())
            .filter(|t| !t.is_empty());
        let mut sections = Vec::new();
        let mut count = 0;
        for (i, page) in document.pages().iter().enumerate() {
            count += 1;
            let text = page.text().map(|t| t.all()).unwrap_or_default();
            let paragraphs = page_paragraphs(&text.replace(['\u{2}', '\u{fffe}'], ""));
            let mut section = format!("<!-- page {} -->", i + 1);
            for paragraph in paragraphs {
                section.push_str(&format!("\n\n{paragraph}"));
            }
            sections.push(section);
        }
        let has_text = sections.iter().any(|s| s.contains("\n\n"));
        Ok((if has_text { sections.join("\n\n") + "\n" } else { String::new() }, count, title))
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// PDF minimal d'une page par page de `pages` (police Helvetica, une ligne de texte par
    /// élément), avec une table des références correcte.
    pub fn sample_pdf(pages: &[&[&str]], title: Option<&str>) -> Vec<u8> {
        let mut objects: Vec<String> = Vec::new();
        let n = pages.len();
        // 1 : catalogue, 2 : arbre des pages, 3 : police, 4 : infos, puis (page, contenu) par page
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 5 + 2 * i)).collect();
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        objects.push(format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")));
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into());
        objects.push(format!("<< /Title ({}) >>", title.unwrap_or("")));
        for (i, lines) in pages.iter().enumerate() {
            let mut stream = String::from("BT /F1 12 Tf 72 720 Td 14 TL\n");
            for line in *lines {
                stream.push_str(&format!("({}) Tj T*\n", line.replace('(', "\\(").replace(')', "\\)")));
            }
            stream.push_str("ET");
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                6 + 2 * i
            ));
            objects.push(format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()));
        }
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            // texte en Windows-1252, comme le déclare la police
            let body = encoding_rs::WINDOWS_1252.encode(object).0;
            pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            pdf.extend_from_slice(&body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        let mut table = format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
        for offset in offsets {
            table.push_str(&format!("{offset:010} 00000 n \n"));
        }
        table.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1));
        pdf.extend_from_slice(table.as_bytes());
        pdf
    }

    /// pdfium.dll du poste, si elle a été téléchargée (sinon le test est sauté).
    pub fn pdfium() -> Option<PathBuf> {
        crate::convert::Tools::discover(None, None).pdfium
    }

    #[test]
    fn paragraphes_recolles() {
        let text = "Introduction\nLe projet de migra-\ntion avance bien et\nil reste deux lots\nà livrer.\n\n• Lot 1\n• Lot 2\nTitre court:\nSuite";
        assert_eq!(
            page_paragraphs(text),
            vec!["Introduction", "Le projet de migration avance bien et il reste deux lots à livrer.", "• Lot 1", "• Lot 2", "Titre court:", "Suite"]
        );
    }

    #[test]
    fn texte_page_par_page() {
        let Some(path) = pdfium() else {
            eprintln!("pdfium.dll absente : test sauté (scripts/telecharger-outils.ps1)");
            return;
        };
        let pdf = sample_pdf(&[&["Rapport annuel", "Chiffre d'affaires en hausse."], &["Perspectives 2027."]], Some("Rapport 2026"));
        let (markdown, pages, title) = pdf_to_markdown(&pdf, &path).unwrap();
        assert_eq!(pages, 2);
        assert_eq!(title.as_deref(), Some("Rapport 2026"));
        assert!(markdown.starts_with("<!-- page 1 -->\n\nRapport annuel"), "{markdown}");
        assert!(markdown.contains("Chiffre d'affaires en hausse."), "{markdown}");
        assert!(markdown.contains("<!-- page 2 -->\n\nPerspectives 2027."), "{markdown}");
        assert!(pdf_to_markdown(b"pas un pdf", &path).is_err());
        let (empty, _, _) = pdf_to_markdown(&sample_pdf(&[&[]], None), &path).unwrap();
        assert_eq!(empty, "");
    }
}
