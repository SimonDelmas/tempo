//! Présentations PowerPoint (pptx, lues dans le zip) et tableurs (calamine).

use std::io::{Cursor, Read};

use calamine::{open_workbook_auto_from_rs, Data, Reader as _};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use quick_xml::XmlVersion;

use crate::error::{vault_err, Result};

/// Lignes gardées par feuille d'un tableur.
const MAX_ROWS: usize = 1000;
/// Colonnes gardées par feuille d'un tableur.
const MAX_COLUMNS: usize = 40;

type Zip = zip::ZipArchive<Cursor<Vec<u8>>>;

fn open_zip(bytes: &[u8], kind: &str) -> Result<Zip> {
    zip::ZipArchive::new(Cursor::new(bytes.to_vec())).map_err(|e| vault_err(format!("Fichier {kind} illisible : {e}")))
}

fn read_entry(zip: &mut Zip, name: &str) -> Option<String> {
    let mut entry = zip.by_name(name).ok()?;
    let mut text = String::new();
    entry.read_to_string(&mut text).ok()?;
    Some(text)
}

/// Chemin d'une cible de relation (`slides/slide1.xml`, `../notesSlides/n1.xml`) depuis le dossier `base`.
fn resolve_target(base: &str, target: &str) -> String {
    let mut parts: Vec<&str> =
        if let Some(absolute) = target.strip_prefix('/') { return absolute.to_string() } else { base.split('/').filter(|p| !p.is_empty()).collect() };
    for part in target.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// Relations d'une partie : (identifiant, type, cible résolue).
fn relationships(zip: &mut Zip, part: &str) -> Vec<(String, String, String)> {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    let Some(xml) = read_entry(zip, &format!("{dir}/_rels/{file}.rels")) else { return Vec::new() };
    let mut reader = Reader::from_str(&xml);
    let mut rels = Vec::new();
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "Relationship" => {
                let attr = |name: &str| {
                    e.try_get_attribute(name).ok().flatten().and_then(|a| a.normalized_value(XmlVersion::Implicit1_0).ok()).map(|v| v.into_owned())
                };
                if let (Some(id), Some(target)) = (attr("Id"), attr("Target")) {
                    rels.push((id, attr("Type").unwrap_or_default(), resolve_target(dir, &target)));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    rels
}

/// Texte d'un nœud texte ou d'une référence d'entité (`&amp;`, `&#233;`).
fn event_text(event: &Event) -> Option<String> {
    match event {
        Event::Text(t) => Some(t.xml10_content().into_owned()),
        Event::CData(t) => Some(t.xml10_content().into_owned()),
        Event::GeneralRef(r) => {
            if let Ok(Some(c)) = r.resolve_char_ref() {
                return Some(c.to_string());
            }
            let name = r.xml10_content();
            Some(match name.as_ref() {
                "amp" => "&",
                "lt" => "<",
                "gt" => ">",
                "quot" => "\"",
                "apos" => "'",
                _ => "",
            }
            .to_string())
        }
        _ => None,
    }
}

/// Contenu d'une diapositive (ou d'une page de notes) : titre, blocs de corps dans l'ordre.
#[derive(Default)]
struct SlideText {
    title: String,
    blocks: Vec<String>,
}

/// Lit les formes d'une diapositive : zones de texte (titre repéré par son espace réservé) et
/// tableaux. `placeholders` restreint aux espaces réservés de ces types (notes : `body`).
fn slide_text(xml: &str, placeholders: Option<&[&str]>) -> SlideText {
    let mut slide = SlideText::default();
    let mut reader = Reader::from_str(xml);
    // forme en cours : type d'espace réservé, paragraphes
    let mut shape: Option<(Option<String>, Vec<String>)> = None;
    let mut paragraph: Option<String> = None;
    let mut in_text = false;
    // tableau en cours : lignes de cellules, cellule en cours
    let mut table: Option<Vec<Vec<String>>> = None;
    let mut cell: Option<Vec<String>> = None;
    loop {
        let Ok(event) = reader.read_event() else { break };
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(event, Event::Empty(_));
                match e.local_name().as_ref() {
                    "sp" if !empty => shape = Some((None, Vec::new())),
                    "ph" => {
                        if let Some((kind, _)) = shape.as_mut() {
                            let value = e.try_get_attribute("type").ok().flatten().and_then(|a| a.normalized_value(XmlVersion::Implicit1_0).ok());
                            *kind = Some(value.map(|v| v.into_owned()).unwrap_or_else(|| "body".into()));
                        }
                    }
                    "tbl" if !empty => table = Some(Vec::new()),
                    "tr" if !empty => {
                        if let Some(rows) = table.as_mut() {
                            rows.push(Vec::new());
                        }
                    }
                    "tc" if !empty && table.is_some() => cell = Some(Vec::new()),
                    "p" if !empty => paragraph = Some(String::new()),
                    "t" if !empty => in_text = true,
                    "br" => {
                        if let Some(p) = paragraph.as_mut() {
                            p.push('\n');
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                match e.local_name().as_ref() {
                    "t" => in_text = false,
                    "p" => {
                        let text = paragraph.take().unwrap_or_default();
                        let text = text.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
                        if text.is_empty() {
                        } else if let Some(c) = cell.as_mut() {
                            c.push(text);
                        } else if let Some((_, paragraphs)) = shape.as_mut() {
                            paragraphs.push(text);
                        }
                    }
                    "tc" => {
                        if let (Some(c), Some(row)) = (cell.take(), table.as_mut().and_then(|rows| rows.last_mut())) {
                            row.push(c.join(" "));
                        }
                    }
                    "tbl" => {
                        if let Some(rows) = table.take() {
                            let markdown = markdown_table(&rows);
                            if !markdown.is_empty() {
                                slide.blocks.push(markdown);
                            }
                        }
                    }
                    "sp" => {
                        if let Some((kind, paragraphs)) = shape.take() {
                            let wanted = match placeholders {
                                Some(types) => kind.as_deref().is_some_and(|k| types.contains(&k)),
                                None => true,
                            };
                            let is_title = matches!(kind.as_deref(), Some("title" | "ctrTitle"));
                            if !wanted || paragraphs.is_empty() {
                            } else if is_title && slide.title.is_empty() && placeholders.is_none() {
                                slide.title = paragraphs.join(" ").replace('\n', " ");
                            } else {
                                slide.blocks.push(paragraphs.join("\n"));
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            other => {
                if in_text
                    && let (Some(p), Some(text)) = (paragraph.as_mut(), event_text(other))
                {
                    p.push_str(&text);
                }
            }
        }
    }
    slide
}

/// Tableau Markdown ; la première ligne sert d'en-tête, les `|` des cellules sont protégés.
fn markdown_table(rows: &[Vec<String>]) -> String {
    let rows: Vec<&Vec<String>> = rows.iter().filter(|r| r.iter().any(|c| !c.trim().is_empty())).collect();
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if width == 0 {
        return String::new();
    }
    let line = |cells: &[String]| {
        let mut cells: Vec<String> = cells.iter().map(|c| c.replace('|', "\\|").replace('\n', " ").trim().to_string()).collect();
        cells.resize(width, String::new());
        format!("| {} |", cells.join(" | "))
    };
    let mut lines = vec![line(rows[0]), format!("|{}", " --- |".repeat(width))];
    lines.extend(rows[1..].iter().map(|r| line(r)));
    lines.join("\n")
}

/// Titre des propriétés d'un document Office (docx, xlsx…), s'il est renseigné.
pub fn office_title(bytes: &[u8]) -> Option<String> {
    core_title(&mut open_zip(bytes, "Office").ok()?)
}

/// Titre des propriétés du document (`docProps/core.xml`).
fn core_title(zip: &mut Zip) -> Option<String> {
    let xml = read_entry(zip, "docProps/core.xml")?;
    let mut reader = Reader::from_str(&xml);
    let mut in_title = false;
    let mut title = String::new();
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) if e.local_name().as_ref() == "title" => in_title = true,
            Event::End(e) if e.local_name().as_ref() == "title" => break,
            Event::Eof => break,
            other if in_title => title.push_str(&event_text(&other).unwrap_or_default()),
            _ => {}
        }
    }
    Some(title.trim().to_string()).filter(|t| !t.is_empty())
}

/// Présentation pptx en Markdown : `## Diapositive N — titre`, corps, tableaux et notes de
/// l'orateur ; renvoie aussi le nombre de diapositives et le titre des propriétés.
pub fn pptx_to_markdown(bytes: &[u8]) -> Result<(String, usize, Option<String>)> {
    let mut zip = open_zip(bytes, "pptx")?;
    let presentation =
        read_entry(&mut zip, "ppt/presentation.xml").ok_or_else(|| vault_err("Fichier pptx illisible : ppt/presentation.xml absent"))?;
    let rels = relationships(&mut zip, "ppt/presentation.xml");
    // ordre des diapositives : liste p:sldIdLst de la présentation
    let mut order: Vec<String> = Vec::new();
    let mut reader = Reader::from_str(&presentation);
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "sldId" => {
                let rid = e.attributes().flatten().find(|a| a.key.local_name().as_ref() == "id" && a.key.as_ref() != "id");
                if let Some(target) = rid
                    .and_then(|a| a.normalized_value(XmlVersion::Implicit1_0).ok().map(|v| v.into_owned()))
                    .and_then(|rid| rels.iter().find(|(id, _, _)| *id == rid).map(|(_, _, t)| t.clone()))
                {
                    order.push(target);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut sections = Vec::new();
    for (i, part) in order.iter().enumerate() {
        let Some(xml) = read_entry(&mut zip, part) else { continue };
        let slide = slide_text(&xml, None);
        let notes = relationships(&mut zip, part)
            .into_iter()
            .find(|(_, kind, _)| kind.ends_with("/notesSlide"))
            .and_then(|(_, _, target)| read_entry(&mut zip, &target))
            .map(|xml| slide_text(&xml, Some(&["body"])).blocks.join(" ").replace('\n', " "))
            .filter(|n| !n.trim().is_empty());
        let mut section = match slide.title.trim() {
            "" => format!("## Diapositive {}", i + 1),
            title => format!("## Diapositive {} — {title}", i + 1),
        };
        for block in slide.blocks {
            section.push_str(&format!("\n\n{block}"));
        }
        if let Some(notes) = notes {
            section.push_str(&format!("\n\n> Notes : {}", notes.trim()));
        }
        sections.push(section);
    }
    let title = core_title(&mut zip);
    Ok((sections.join("\n\n") + "\n", order.len(), title))
}

/// Texte d'une cellule de tableur (dates au format AAAA-MM-JJ, nombres sans décimale inutile).
fn cell_text(cell: &Data) -> String {
    match cell {
        Data::Empty | Data::Error(_) => String::new(),
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
        Data::DateTime(d) if d.is_datetime() => {
            let (y, m, day, h, min, _, _) = d.to_ymd_hms_milli();
            if h == 0 && min == 0 { format!("{y:04}-{m:02}-{day:02}") } else { format!("{y:04}-{m:02}-{day:02} {h:02}:{min:02}") }
        }
        other => other.to_string(),
    }
}

/// Tableur en Markdown : une section `## Feuille : nom` par feuille non vide, avec un tableau
/// (au plus [`MAX_ROWS`] lignes et [`MAX_COLUMNS`] colonnes, la troncature est signalée).
pub fn spreadsheet_to_markdown(bytes: &[u8]) -> Result<String> {
    let mut workbook =
        open_workbook_auto_from_rs(Cursor::new(bytes.to_vec())).map_err(|e| vault_err(format!("Tableur illisible : {e}")))?;
    let mut sections = Vec::new();
    for name in workbook.sheet_names() {
        let Ok(range) = workbook.worksheet_range(&name) else { continue };
        let rows: Vec<Vec<String>> = range
            .rows()
            .map(|row| row.iter().take(MAX_COLUMNS).map(cell_text).collect::<Vec<_>>())
            .filter(|row| row.iter().any(|c| !c.trim().is_empty()))
            .collect();
        if rows.is_empty() {
            continue;
        }
        let mut section = format!("## Feuille : {name}\n\n{}", markdown_table(&rows[..rows.len().min(MAX_ROWS + 1)]));
        if rows.len() > MAX_ROWS + 1 {
            section.push_str(&format!("\n\n[…{} lignes de plus non reprises]", rows.len() - MAX_ROWS - 1));
        }
        if range.width() > MAX_COLUMNS {
            section.push_str(&format!("\n\n[…{} colonnes de plus non reprises]", range.width() - MAX_COLUMNS));
        }
        sections.push(section);
    }
    Ok(sections.join("\n\n") + "\n")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// Zip en mémoire à partir de (chemin, contenu).
    pub fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in files {
            writer.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    const P: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    fn sp(ph: Option<&str>, paragraphs: &[&str]) -> String {
        let ph = ph.map(|t| format!(r#"<p:nvPr><p:ph type="{t}"/></p:nvPr>"#)).unwrap_or_else(|| "<p:nvPr/>".into());
        let body: String = paragraphs.iter().map(|p| format!("<a:p><a:r><a:t>{p}</a:t></a:r></a:p>")).collect();
        format!("<p:sp><p:nvSpPr>{ph}</p:nvSpPr><p:txBody>{body}</p:txBody></p:sp>")
    }

    pub fn sample_pptx() -> Vec<u8> {
        let rel = |id: &str, kind: &str, target: &str| {
            format!(r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}"/>"#)
        };
        let rels = |items: String| format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{items}</Relationships>"#);
        let presentation = format!(r#"<p:presentation {P}><p:sldIdLst><p:sldId id="257" r:id="rId3"/><p:sldId id="256" r:id="rId2"/></p:sldIdLst></p:presentation>"#);
        let slide1 = format!(
            r#"<p:sld {P}><p:cSld><p:spTree>{}{}</p:spTree></p:cSld></p:sld>"#,
            sp(Some("ctrTitle"), &["Contexte &amp; enjeux"]),
            sp(None, &["Bascule S/4 en mars", "Reprise des articles"])
        );
        let table = "<p:graphicFrame><a:graphic><a:graphicData><a:tbl>\
            <a:tr><a:tc><a:txBody><a:p><a:r><a:t>Lot</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p><a:r><a:t>Date</a:t></a:r></a:p></a:txBody></a:tc></a:tr>\
            <a:tr><a:tc><a:txBody><a:p><a:r><a:t>1</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p><a:r><a:t>mars | avril</a:t></a:r></a:p></a:txBody></a:tc></a:tr>\
            </a:tbl></a:graphicData></a:graphic></p:graphicFrame>";
        let slide2 = format!(r#"<p:sld {P}><p:cSld><p:spTree>{table}</p:spTree></p:cSld></p:sld>"#);
        let notes = format!(
            r#"<p:notes {P}><p:cSld><p:spTree>{}{}</p:spTree></p:cSld></p:notes>"#,
            sp(Some("sldImg"), &[]),
            sp(Some("body"), &["Insister sur le calendrier"])
        );
        zip_of(&[
            ("ppt/presentation.xml", &presentation),
            ("ppt/_rels/presentation.xml.rels", &rels(rel("rId2", "slide", "slides/slide2.xml") + &rel("rId3", "slide", "slides/slide1.xml"))),
            ("ppt/slides/slide1.xml", &slide1),
            ("ppt/slides/_rels/slide1.xml.rels", &rels(rel("rId1", "notesSlide", "../notesSlides/notesSlide1.xml"))),
            ("ppt/slides/slide2.xml", &slide2),
            ("ppt/notesSlides/notesSlide1.xml", &notes),
            ("docProps/core.xml", r#"<cp:coreProperties xmlns:cp="c" xmlns:dc="d"><dc:title>Revue de projet</dc:title></cp:coreProperties>"#),
        ])
    }

    #[test]
    fn diapositives_dans_l_ordre_de_la_presentation() {
        let (markdown, slides, title) = pptx_to_markdown(&sample_pptx()).unwrap();
        assert_eq!(slides, 2);
        assert_eq!(title.as_deref(), Some("Revue de projet"));
        assert_eq!(
            markdown,
            "## Diapositive 1 — Contexte & enjeux\n\nBascule S/4 en mars\nReprise des articles\n\n\
             > Notes : Insister sur le calendrier\n\n\
             ## Diapositive 2\n\n| Lot | Date |\n| --- | --- |\n| 1 | mars \\| avril |\n"
        );
    }

    #[test]
    fn pptx_invalide() {
        assert!(pptx_to_markdown(b"pas un zip").is_err());
        assert!(pptx_to_markdown(&zip_of(&[("autre.xml", "<x/>")])).is_err());
    }

    #[test]
    fn cibles_relatives() {
        assert_eq!(resolve_target("ppt", "slides/slide1.xml"), "ppt/slides/slide1.xml");
        assert_eq!(resolve_target("ppt/slides", "../notesSlides/n1.xml"), "ppt/notesSlides/n1.xml");
        assert_eq!(resolve_target("ppt/slides", "/ppt/media/i.png"), "ppt/media/i.png");
    }

    #[test]
    fn tableau_markdown() {
        let rows = vec![vec!["A".to_string(), "B".to_string()], vec!["1".to_string()], vec![String::new(), " ".to_string()]];
        assert_eq!(markdown_table(&rows), "| A | B |\n| --- | --- |\n| 1 |  |");
        assert_eq!(markdown_table(&[]), "");
    }

    #[test]
    fn tableur_invalide() {
        assert!(spreadsheet_to_markdown(b"pas un classeur").is_err());
    }
}
