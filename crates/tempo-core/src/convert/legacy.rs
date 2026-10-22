//! Formats Office 97-2003 sans Office : texte d'un document Word (.doc, [MS-DOC]) et d'une
//! présentation PowerPoint (.ppt, [MS-PPT]), lus dans le fichier composé OLE avec `cfb`.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};

use encoding_rs::WINDOWS_1252;

use crate::error::{vault_err, Result};

const DOC: &str = ".doc";
const PPT: &str = ".ppt";
/// Garde-fous contre les fichiers malveillants : enregistrements PowerPoint parcourus,
/// profondeur d'imbrication des conteneurs, éditions successives suivies.
const MAX_RECORDS: usize = 2_000_000;
const MAX_DEPTH: usize = 32;
const MAX_EDITS: usize = 1024;

type Compound<'a> = cfb::CompoundFile<Cursor<&'a [u8]>>;

// --- lecture du fichier composé et des valeurs binaires ------------------------------

fn open_compound<'a>(bytes: &'a [u8], kind: &str) -> Result<Compound<'a>> {
    cfb::CompoundFile::open(Cursor::new(bytes))
        .map_err(|_| vault_err(format!("Fichier {kind} illisible : ce n'est pas un fichier Office 97-2003 (OLE)")))
}

/// Contenu d'un flux, `None` s'il n'existe pas.
fn read_stream(comp: &mut Compound, path: &str, kind: &str) -> Result<Option<Vec<u8>>> {
    if !comp.is_stream(path) {
        return Ok(None);
    }
    let corrupt = |_| vault_err(format!("Fichier {kind} illisible : flux {path} corrompu"));
    let mut data = Vec::new();
    comp.open_stream(path).map_err(corrupt)?.read_to_end(&mut data).map_err(corrupt)?;
    Ok(Some(data))
}

/// Valeur lue dans le fichier, ou erreur « Fichier .xxx illisible : détail ».
fn need<T>(value: Option<T>, kind: &str, detail: &str) -> Result<T> {
    value.ok_or_else(|| vault_err(format!("Fichier {kind} illisible : {detail}")))
}

fn slice_at(data: &[u8], offset: usize, len: usize) -> Option<&[u8]> {
    data.get(offset..offset.checked_add(len)?)
}

fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(slice_at(data, offset, 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(slice_at(data, offset, 4)?.try_into().ok()?))
}

fn utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).filter_map(|c| c.try_into().ok()).map(u16::from_le_bytes).collect();
    String::from_utf16_lossy(&units)
}

fn cp1252(bytes: &[u8]) -> String {
    WINDOWS_1252.decode_without_bom_handling(bytes).0.into_owned()
}

/// Espaces successifs réduits à un seul, sans espace en début ni en fin.
fn squeeze(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// --- Word 97-2003 -----------------------------------------------------------------

/// Champs utiles de la FIB (File Information Block) en tête du flux WordDocument.
struct Fib {
    table_stream: &'static str,
    ccp_text: u32,
    fc_clx: u32,
    lcb_clx: u32,
}

fn parse_fib(word: &[u8]) -> Result<Fib> {
    let truncated = "en-tête FIB tronqué";
    if need(u16_at(word, 0x00), DOC, truncated)? != 0xA5EC {
        return Err(vault_err("Fichier .doc illisible : ce n'est pas un document Word 97-2003"));
    }
    if need(u16_at(word, 0x02), DOC, truncated)? < 0x0069 {
        return Err(vault_err("Fichier .doc illisible : format antérieur à Word 97 non pris en charge"));
    }
    // FibBase : fEncrypted (0x0100) et fWhichTblStm (0x0200) dans le mot à l'offset 0x0A.
    let flags = need(u16_at(word, 0x0A), DOC, truncated)?;
    if flags & 0x0100 != 0 {
        return Err(vault_err("Fichier .doc illisible : document protégé par mot de passe"));
    }
    let table_stream = if flags & 0x0200 != 0 { "1Table" } else { "0Table" };
    // FibBase (32 octets), puis csw + FibRgW97, cslw + FibRgLw97, cbRgFcLcb + FibRgFcLcb97.
    let csw = need(u16_at(word, 0x20), DOC, truncated)? as usize;
    let cslw_at = 0x22 + csw * 2;
    let cslw = need(u16_at(word, cslw_at), DOC, truncated)? as usize;
    let lw_at = cslw_at + 2;
    // ccpText : 4e entier de FibRgLw97 (offset 0x0C).
    if cslw < 4 {
        return Err(vault_err("Fichier .doc illisible : FIB incomplète (FibRgLw97)"));
    }
    let ccp_text = need(u32_at(word, lw_at + 0x0C), DOC, truncated)?;
    let cb_at = lw_at + cslw * 4;
    let cb_rg_fc_lcb = need(u16_at(word, cb_at), DOC, truncated)? as usize;
    // fcClx / lcbClx : 34e paire (indice 33) de FibRgFcLcb97, soit l'offset 0x1A2 d'une FIB standard.
    if cb_rg_fc_lcb < 34 {
        return Err(vault_err("Fichier .doc illisible : FIB incomplète (FibRgFcLcb97)"));
    }
    let clx_at = cb_at + 2 + 33 * 8;
    Ok(Fib {
        table_stream,
        ccp_text,
        fc_clx: need(u32_at(word, clx_at), DOC, truncated)?,
        lcb_clx: need(u32_at(word, clx_at + 4), DOC, truncated)?,
    })
}

/// PlcPcd du Clx : les Prc (0x01) qui le précèdent sont sautés.
fn find_plc_pcd(clx: &[u8]) -> Result<&[u8]> {
    let mut pos = 0usize;
    loop {
        match clx.get(pos) {
            Some(0x01) => {
                let cb = need(u16_at(clx, pos + 1), DOC, "Clx tronqué")? as i16;
                if cb < 0 {
                    return Err(vault_err("Fichier .doc illisible : Clx invalide"));
                }
                pos += 3 + cb as usize;
            }
            Some(0x02) => {
                let lcb = need(u32_at(clx, pos + 1), DOC, "Clx tronqué")? as usize;
                return need(slice_at(clx, pos + 5, lcb), DOC, "table des pièces tronquée");
            }
            _ => return Err(vault_err("Fichier .doc illisible : table des pièces (Clx) absente")),
        }
    }
}

struct Piece {
    cp_start: u32,
    cp_end: u32,
    fc: u32,
    compressed: bool,
}

/// PlcPcd : n+1 positions de caractères (CP) puis n descripteurs de pièce de 8 octets.
fn parse_pieces(plc: &[u8]) -> Result<Vec<Piece>> {
    let count = plc.len().saturating_sub(4) / 12;
    if count == 0 {
        return Err(vault_err("Fichier .doc illisible : table des pièces vide"));
    }
    let mut pieces = Vec::with_capacity(count);
    for i in 0..count {
        let cp_start = need(u32_at(plc, i * 4), DOC, "table des pièces tronquée")?;
        let cp_end = need(u32_at(plc, (i + 1) * 4), DOC, "table des pièces tronquée")?;
        if cp_end < cp_start {
            return Err(vault_err("Fichier .doc illisible : table des pièces incohérente"));
        }
        // Pcd : 2 octets d'options, puis FcCompressed (fc sur 30 bits, fCompressed 0x40000000).
        let fc = need(u32_at(plc, (count + 1) * 4 + i * 8 + 2), DOC, "table des pièces tronquée")?;
        pieces.push(Piece { cp_start, cp_end, fc: fc & 0x3FFF_FFFF, compressed: fc & 0x4000_0000 != 0 });
    }
    Ok(pieces)
}

/// Texte principal du document : les `ccp_text` premiers caractères, sans notes ni en-têtes.
fn main_text(word: &[u8], pieces: &[Piece], ccp_text: u32) -> Result<String> {
    // Un caractère occupe au moins un octet du flux : borne contre les pièces répétées.
    let mut budget = word.len();
    let mut text = String::new();
    for piece in pieces {
        let end = piece.cp_end.min(ccp_text);
        if end <= piece.cp_start {
            continue;
        }
        let count = ((end - piece.cp_start) as usize).min(budget);
        budget -= count;
        let missing = "texte hors du flux WordDocument";
        if piece.compressed {
            text.push_str(&cp1252(need(slice_at(word, piece.fc as usize / 2, count), DOC, missing)?));
        } else {
            let len = need(count.checked_mul(2), DOC, missing)?;
            text.push_str(&utf16le(need(slice_at(word, piece.fc as usize, len), DOC, missing)?));
        }
        if budget == 0 {
            break;
        }
    }
    Ok(text)
}

fn push_paragraph(paragraphs: &mut Vec<String>, text: &str) {
    let text = squeeze(text);
    if !text.is_empty() {
        paragraphs.push(text);
    }
}

/// Ligne de tableau : cellules séparées par « | », ignorée si toutes sont vides.
fn push_row(paragraphs: &mut Vec<String>, row: &mut Vec<String>) {
    if row.iter().any(|cell| !cell.is_empty()) {
        push_paragraph(paragraphs, &row.join(" | "));
    }
    row.clear();
}

/// Markdown du texte Word : paragraphes, cellules de tableau sur une ligne, résultats de champs.
fn doc_text_to_markdown(text: &str) -> String {
    let mut paragraphs = Vec::new();
    let mut current = String::new();
    let mut row: Vec<String> = Vec::new();
    // Champs ouverts (imbrication possible) : vrai tant qu'on est dans le code, avant 0x14.
    let mut fields: Vec<bool> = Vec::new();
    let mut after_cell = false;
    for ch in text.chars() {
        match ch {
            '\u{13}' => fields.push(true),
            '\u{14}' => {
                if let Some(code) = fields.last_mut() {
                    *code = false;
                }
            }
            '\u{15}' => {
                fields.pop();
            }
            _ if fields.iter().any(|&code| code) => {}
            // 0x07 : fin de cellule ; deux marques consécutives terminent la ligne du tableau.
            '\u{07}' if after_cell && current.trim().is_empty() => {
                push_row(&mut paragraphs, &mut row);
                after_cell = false;
            }
            '\u{07}' => {
                row.push(squeeze(&current));
                current.clear();
                after_cell = true;
            }
            _ => {
                after_cell = false;
                match ch {
                    // Fin de paragraphe, saut de ligne, saut de page.
                    '\r' | '\u{0B}' | '\u{0C}' if row.is_empty() => {
                        push_paragraph(&mut paragraphs, &current);
                        current.clear();
                    }
                    '\r' | '\u{0B}' | '\u{0C}' | '\t' => current.push(' '),
                    // Trait d'union insécable ; les autres caractères de contrôle (objets 0x01 /
                    // 0x08, trait d'union conditionnel 0x1F…) sont retirés.
                    '\u{1E}' => current.push('-'),
                    c if c.is_control() => {}
                    c => current.push(c),
                }
            }
        }
    }
    if row.is_empty() {
        push_paragraph(&mut paragraphs, &current);
    } else {
        row.push(squeeze(&current));
        push_row(&mut paragraphs, &mut row);
    }
    if paragraphs.is_empty() { String::new() } else { paragraphs.join("\n\n") + "\n" }
}

/// Texte d'un document Word 97-2003 en Markdown (paragraphes séparés par une ligne vide).
pub fn doc_to_markdown(bytes: &[u8]) -> Result<String> {
    let mut comp = open_compound(bytes, DOC)?;
    let word = read_stream(&mut comp, "WordDocument", DOC)?
        .ok_or_else(|| vault_err("Fichier .doc illisible : flux WordDocument absent"))?;
    let fib = parse_fib(&word)?;
    let table = read_stream(&mut comp, fib.table_stream, DOC)?
        .ok_or_else(|| vault_err(format!("Fichier .doc illisible : flux {} absent", fib.table_stream)))?;
    let clx = need(
        slice_at(&table, fib.fc_clx as usize, fib.lcb_clx as usize),
        DOC,
        "table des pièces hors du flux de table",
    )?;
    let pieces = parse_pieces(find_plc_pcd(clx)?)?;
    Ok(doc_text_to_markdown(&main_text(&word, &pieces, fib.ccp_text)?))
}

// --- PowerPoint 97-2003 -----------------------------------------------------------

// Types d'enregistrements [MS-PPT].
const RT_DOCUMENT: u16 = 0x03E8;
const RT_SLIDE: u16 = 0x03EE;
const RT_NOTES: u16 = 0x03F0;
const RT_NOTES_ATOM: u16 = 0x03F1;
const RT_SLIDE_PERSIST_ATOM: u16 = 0x03F3;
const RT_OUTLINE_TEXT_REF_ATOM: u16 = 0x0F9E;
const RT_TEXT_HEADER_ATOM: u16 = 0x0F9F;
const RT_TEXT_CHARS_ATOM: u16 = 0x0FA0;
const RT_TEXT_BYTES_ATOM: u16 = 0x0FA8;
const RT_SLIDE_LIST_WITH_TEXT: u16 = 0x0FF0;
const RT_USER_EDIT_ATOM: u16 = 0x0FF5;
const RT_CURRENT_USER_ATOM: u16 = 0x0FF6;
const RT_PERSIST_DIRECTORY_ATOM: u16 = 0x1772;
const RT_CRYPT_SESSION: u16 = 0x2F14;

// Types de texte du TextHeaderAtom : titre, notes, titre centré ; « inconnu » sans en-tête.
const TEXT_TITLE: u32 = 0;
const TEXT_NOTES: u32 = 2;
const TEXT_CENTER_TITLE: u32 = 6;
const TEXT_UNKNOWN: u32 = u32::MAX;

fn ppt_protected<T>() -> Result<T> {
    Err(vault_err("Fichier .ppt illisible : présentation protégée par mot de passe"))
}

/// Enregistrement : en-tête de 8 octets (recVer 4 bits + recInstance 12 bits, recType, recLen).
struct Record<'a> {
    version: u16,
    instance: u16,
    kind: u16,
    data: &'a [u8],
}

impl Record<'_> {
    fn is_container(&self) -> bool {
        self.version == 0x0F
    }
}

fn record_at(stream: &[u8], offset: usize) -> Result<Record<'_>> {
    let truncated = "enregistrement tronqué";
    let header = need(slice_at(stream, offset, 8), PPT, truncated)?;
    let (Some(head), Some(kind), Some(len)) = (u16_at(header, 0), u16_at(header, 2), u32_at(header, 4)) else {
        return Err(vault_err("Fichier .ppt illisible : enregistrement tronqué"));
    };
    let data = need(slice_at(stream, offset + 8, len as usize), PPT, truncated)?;
    Ok(Record { version: head & 0x0F, instance: head >> 4, kind, data })
}

/// Enregistrements successifs d'un flux ou du contenu d'un conteneur.
fn records<'a>(data: &'a [u8], budget: &mut usize) -> Result<Vec<Record<'a>>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        *budget = need(budget.checked_sub(1), PPT, "trop d'enregistrements")?;
        let record = record_at(data, pos)?;
        pos += 8 + record.data.len();
        out.push(record);
    }
    Ok(out)
}

#[derive(Clone)]
struct TextItem {
    kind: u32,
    text: String,
}

fn atom_text(record: &Record) -> String {
    if record.kind == RT_TEXT_CHARS_ATOM { utf16le(record.data) } else { cp1252(record.data) }
}

/// Ouvre un texte typé par un TextHeaderAtom ; le TextChars/TextBytes suivant le remplira.
fn open_text(items: &mut Vec<TextItem>, pending: &mut Option<usize>, record: &Record) {
    items.push(TextItem { kind: u32_at(record.data, 0).unwrap_or(TEXT_UNKNOWN), text: String::new() });
    *pending = Some(items.len() - 1);
}

/// Rattache un texte au TextHeaderAtom qui le précède, ou l'ajoute sans type.
fn attach_text(items: &mut Vec<TextItem>, pending: &mut Option<usize>, text: String) {
    match pending.take().and_then(|i| items.get_mut(i)) {
        Some(item) => item.text = text,
        None => items.push(TextItem { kind: TEXT_UNKNOWN, text }),
    }
}

/// Entrée d'une SlideListWithText : SlidePersistAtom puis textes des espaces réservés.
struct ListEntry {
    persist_ref: u32,
    slide_id: u32,
    texts: Vec<TextItem>,
}

fn list_entries(list: &Record, budget: &mut usize) -> Result<Vec<ListEntry>> {
    let mut entries: Vec<ListEntry> = Vec::new();
    let mut pending = None;
    for record in records(list.data, budget)? {
        // SlidePersistAtom : persistIdRef à l'offset 0, slideId à l'offset 12.
        if record.kind == RT_SLIDE_PERSIST_ATOM {
            entries.push(ListEntry {
                persist_ref: u32_at(record.data, 0).unwrap_or(0),
                slide_id: u32_at(record.data, 12).unwrap_or(0),
                texts: Vec::new(),
            });
            pending = None;
            continue;
        }
        let Some(entry) = entries.last_mut() else { continue };
        match record.kind {
            RT_TEXT_HEADER_ATOM => open_text(&mut entry.texts, &mut pending, &record),
            RT_TEXT_CHARS_ATOM | RT_TEXT_BYTES_ATOM => attach_text(&mut entry.texts, &mut pending, atom_text(&record)),
            _ => {}
        }
    }
    Ok(entries)
}

/// Textes d'un conteneur (diapositive, notes), dans l'ordre des formes du dessin ; un
/// OutlineTextRefAtom renvoie au i-ème texte de l'entrée de SlideListWithText.
fn collect_texts(
    data: &[u8],
    list: &[TextItem],
    used: &mut [bool],
    out: &mut Vec<TextItem>,
    depth: usize,
    budget: &mut usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(vault_err("Fichier .ppt illisible : imbrication trop profonde"));
    }
    let mut pending = None;
    for record in records(data, budget)? {
        match record.kind {
            RT_TEXT_HEADER_ATOM => open_text(out, &mut pending, &record),
            RT_TEXT_CHARS_ATOM | RT_TEXT_BYTES_ATOM => attach_text(out, &mut pending, atom_text(&record)),
            RT_OUTLINE_TEXT_REF_ATOM => {
                let index = u32_at(record.data, 0).unwrap_or(u32::MAX) as usize;
                if let Some(item) = list.get(index) {
                    out.push(item.clone());
                    if let Some(flag) = used.get_mut(index) {
                        *flag = true;
                    }
                }
            }
            _ if record.is_container() => collect_texts(record.data, list, used, out, depth + 1, budget)?,
            _ => {}
        }
    }
    Ok(())
}

/// Lignes d'un texte PowerPoint : 0x0D sépare les paragraphes, 0x0B est un saut de ligne.
fn ppt_lines(text: &str) -> Vec<String> {
    text.split(['\r', '\u{0B}'])
        .map(|line| squeeze(&line.replace('\t', " ").replace(|c: char| c.is_control(), "")))
        .filter(|line| !line.is_empty())
        .collect()
}

/// Textes d'une diapositive ou d'une page de notes : ceux de la liste que le dessin ne
/// référence pas (espaces réservés), puis ceux du dessin, sans doublon ni texte vide.
fn merge_texts(container: Option<&Record>, list: &[TextItem], budget: &mut usize) -> Result<Vec<(u32, Vec<String>)>> {
    let mut used = vec![false; list.len()];
    let mut drawn = Vec::new();
    if let Some(container) = container {
        collect_texts(container.data, list, &mut used, &mut drawn, 0, budget)?;
    }
    let unused = list.iter().zip(&used).filter(|(_, used)| !**used).map(|(item, _)| item.clone());
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for item in unused.chain(drawn) {
        let lines = ppt_lines(&item.text);
        if !lines.is_empty() && seen.insert(lines.join("\n")) {
            merged.push((item.kind, lines));
        }
    }
    Ok(merged)
}

/// Table persistId → position dans le flux, reconstituée en suivant les éditions depuis la
/// dernière (flux « Current User ») ; `None` si elle est inexploitable.
struct Persist {
    offsets: HashMap<u32, usize>,
    doc_ref: u32,
    crypt_ref: Option<u32>,
}

fn persist_directory(stream: &[u8], current_user: &[u8]) -> Result<Option<Persist>> {
    // CurrentUserAtom : headerToken à l'offset 12, offsetToCurrentEdit à l'offset 16.
    if u16_at(current_user, 2) != Some(RT_CURRENT_USER_ATOM) {
        return Ok(None);
    }
    if u32_at(current_user, 12) == Some(0xF3D1_C4DF) {
        return ppt_protected();
    }
    let Some(mut edit_at) = u32_at(current_user, 16) else { return Ok(None) };
    let mut offsets = HashMap::new();
    let mut seen = HashSet::new();
    let mut newest: Option<(u32, Option<u32>)> = None;
    loop {
        if !seen.insert(edit_at) || seen.len() > MAX_EDITS {
            return Ok(None);
        }
        let Ok(edit) = record_at(stream, edit_at as usize) else { return Ok(None) };
        if edit.kind != RT_USER_EDIT_ATOM {
            return Ok(None);
        }
        // UserEditAtom : offsetLastEdit (8), offsetPersistDirectory (12), docPersistIdRef (16),
        // encryptSessionPersistIdRef (28, présent seulement si la présentation est chiffrée).
        if newest.is_none() {
            let Some(doc_ref) = u32_at(edit.data, 16) else { return Ok(None) };
            newest = Some((doc_ref, u32_at(edit.data, 28)));
        }
        let Some(dir_at) = u32_at(edit.data, 12) else { return Ok(None) };
        let Ok(dir) = record_at(stream, dir_at as usize) else { return Ok(None) };
        if dir.kind != RT_PERSIST_DIRECTORY_ATOM {
            return Ok(None);
        }
        // PersistDirectoryEntry : persistId (20 bits) + cPersist (12 bits), puis cPersist positions.
        let mut pos = 0;
        while let Some(head) = u32_at(dir.data, pos) {
            let (first, count) = (head & 0x000F_FFFF, (head >> 20) as usize);
            for k in 0..count {
                let Some(offset) = u32_at(dir.data, pos + 4 + k * 4) else { return Ok(None) };
                // L'édition la plus récente l'emporte : on parcourt de la dernière à la première.
                offsets.entry(first + k as u32).or_insert(offset as usize);
            }
            pos += 4 + count * 4;
        }
        match u32_at(edit.data, 8) {
            Some(0) | None => break,
            Some(previous) => edit_at = previous,
        }
    }
    Ok(newest.map(|(doc_ref, crypt_ref)| Persist { offsets, doc_ref, crypt_ref }))
}

fn resolve<'a>(stream: &'a [u8], persist: &Persist, id: u32, kind: u16) -> Option<Record<'a>> {
    let offset = *persist.offsets.get(&id)?;
    record_at(stream, offset).ok().filter(|record| record.kind == kind)
}

/// Diapositive ou page de notes : conteneur Slide / Notes et entrée de SlideListWithText,
/// si trouvés.
struct SlideSource<'a> {
    container: Option<Record<'a>>,
    entry: Option<ListEntry>,
}

impl SlideSource<'_> {
    fn list_texts(&self) -> &[TextItem] {
        self.entry.as_ref().map_or(&[], |entry| entry.texts.as_slice())
    }
}

/// Diapositives et pages de notes.
type Deck<'a> = (Vec<SlideSource<'a>>, Vec<SlideSource<'a>>);

/// Diapositives et pages de notes, via la table de persistance (ordre de la présentation).
fn deck_from_persist<'a>(stream: &'a [u8], persist: &Persist, budget: &mut usize) -> Result<Option<Deck<'a>>> {
    let Some(document) = resolve(stream, persist, persist.doc_ref, RT_DOCUMENT) else { return Ok(None) };
    let (slides, notes) = document_lists(&document, budget)?;
    if slides.is_empty() {
        return Ok(None);
    }
    let sources = |entries: Vec<ListEntry>, kind: u16| -> Vec<SlideSource<'a>> {
        entries
            .into_iter()
            .map(|entry| SlideSource {
                container: resolve(stream, persist, entry.persist_ref, kind),
                entry: Some(entry),
            })
            .collect()
    };
    Ok(Some((sources(slides, RT_SLIDE), sources(notes, RT_NOTES))))
}

/// Diapositives et pages de notes dans l'ordre d'apparition au premier niveau du flux ; la
/// i-ème entrée de SlideListWithText est associée au i-ème conteneur Slide.
fn deck_from_scan<'a>(stream: &'a [u8], budget: &mut usize) -> Result<Deck<'a>> {
    let top = records(stream, budget)?;
    if top.iter().any(|record| record.kind == RT_CRYPT_SESSION) {
        return ppt_protected();
    }
    let (entries, _) = match top.iter().find(|record| record.kind == RT_DOCUMENT) {
        Some(document) => document_lists(document, budget)?,
        None => (Vec::new(), Vec::new()),
    };
    let mut containers = Vec::new();
    let mut notes = Vec::new();
    for record in top {
        match record.kind {
            RT_SLIDE => containers.push(record),
            RT_NOTES => notes.push(SlideSource { container: Some(record), entry: None }),
            _ => {}
        }
    }
    let count = if entries.is_empty() { containers.len() } else { entries.len() };
    let mut containers = containers.into_iter();
    let mut entries = entries.into_iter();
    let slides = (0..count).map(|_| SlideSource { container: containers.next(), entry: entries.next() }).collect();
    Ok((slides, notes))
}

/// Entrées des SlideListWithText du Document : diapositives (instance 0) et notes (instance 2).
fn document_lists(document: &Record, budget: &mut usize) -> Result<(Vec<ListEntry>, Vec<ListEntry>)> {
    let (mut slides, mut notes) = (Vec::new(), Vec::new());
    for record in records(document.data, budget)? {
        if record.kind == RT_SLIDE_LIST_WITH_TEXT {
            match record.instance {
                0 => slides.extend(list_entries(&record, budget)?),
                2 => notes.extend(list_entries(&record, budget)?),
                _ => {}
            }
        }
    }
    Ok((slides, notes))
}

/// Texte d'une présentation PowerPoint 97-2003 en Markdown, et nombre de diapositives.
pub fn ppt_to_markdown(bytes: &[u8]) -> Result<(String, usize)> {
    let mut comp = open_compound(bytes, PPT)?;
    if comp.is_stream("EncryptedSummary") {
        return ppt_protected();
    }
    let stream = read_stream(&mut comp, "PowerPoint Document", PPT)?
        .ok_or_else(|| vault_err("Fichier .ppt illisible : flux PowerPoint Document absent"))?;
    let current_user = read_stream(&mut comp, "Current User", PPT)?;
    let mut budget = MAX_RECORDS;
    let persist = match &current_user {
        Some(current_user) => persist_directory(&stream, current_user)?,
        None => None,
    };
    let mut deck = None;
    if let Some(persist) = &persist {
        if let Some(id) = persist.crypt_ref
            && resolve(&stream, persist, id, RT_CRYPT_SESSION).is_some()
        {
            return ppt_protected();
        }
        deck = deck_from_persist(&stream, persist, &mut budget)?;
    }
    let (slides, notes) = match deck {
        Some(deck) => deck,
        None => deck_from_scan(&stream, &mut budget)?,
    };

    // Notes de l'orateur : rattachées à la diapositive par NotesAtom.slideIdRef.
    let slide_ids: HashMap<u32, usize> = slides
        .iter()
        .enumerate()
        .filter_map(|(i, slide)| slide.entry.as_ref().map(|entry| (entry.slide_id, i)))
        .collect();
    let mut slide_notes: HashMap<usize, Vec<String>> = HashMap::new();
    for page in &notes {
        let Some(container) = &page.container else { continue };
        let children = records(container.data, &mut budget)?;
        let Some(atom) = children.iter().find(|record| record.kind == RT_NOTES_ATOM) else { continue };
        let Some(&index) = u32_at(atom.data, 0).and_then(|id| slide_ids.get(&id)) else { continue };
        let lines: Vec<String> = merge_texts(Some(container), page.list_texts(), &mut budget)?
            .into_iter()
            .filter(|(kind, _)| *kind == TEXT_NOTES || *kind == TEXT_UNKNOWN)
            .flat_map(|(_, lines)| lines)
            .collect();
        if !lines.is_empty() {
            slide_notes.insert(index, lines);
        }
    }

    // Même convention que le convertisseur pptx : un bloc par forme (une ligne par paragraphe),
    // notes sur une seule ligne.
    let mut blocks = Vec::new();
    for (i, slide) in slides.iter().enumerate() {
        let mut title = None;
        let mut body = Vec::new();
        for (kind, lines) in merge_texts(slide.container.as_ref(), slide.list_texts(), &mut budget)? {
            if title.is_none() && (kind == TEXT_TITLE || kind == TEXT_CENTER_TITLE) {
                title = Some(lines.join(" "));
            } else {
                body.push(lines.join("\n"));
            }
        }
        blocks.push(match title {
            Some(title) => format!("## Diapositive {} — {title}", i + 1),
            None => format!("## Diapositive {}", i + 1),
        });
        blocks.extend(body);
        if let Some(lines) = slide_notes.get(&i) {
            blocks.push(format!("> Notes : {}", lines.join(" ")));
        }
    }
    let markdown = if blocks.is_empty() { String::new() } else { blocks.join("\n\n") + "\n" };
    Ok((markdown, slides.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Fichier composé OLE construit en mémoire.
    fn compound(streams: &[(&str, &[u8])]) -> Vec<u8> {
        let mut comp = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        for (path, data) in streams {
            comp.create_stream(path).unwrap().write_all(data).unwrap();
        }
        comp.flush().unwrap();
        comp.into_inner().into_inner()
    }

    fn put_u16(buf: &mut [u8], offset: usize, value: u16) {
        buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(buf: &mut [u8], offset: usize, value: u32) {
        buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    struct DocSpec<'a> {
        /// Pièces de texte : (texte, compressée en cp1252 ou UTF-16).
        pieces: &'a [(&'a str, bool)],
        /// Nombre de caractères du texte principal (par défaut : tout le texte).
        ccp_text: Option<u32>,
        flags: u16,
        with_prc: bool,
    }

    impl Default for DocSpec<'_> {
        fn default() -> Self {
            DocSpec { pieces: &[], ccp_text: None, flags: 0x0200, with_prc: false }
        }
    }

    /// Flux WordDocument et flux de table d'un .doc minimal : FIB standard (csw = 14,
    /// cslw = 22, cbRgFcLcb = 93), texte à partir de 0x400, Clx en tête du flux de table.
    fn doc_streams(spec: &DocSpec) -> (Vec<u8>, Vec<u8>) {
        let mut word = vec![0u8; 0x400];
        put_u16(&mut word, 0x00, 0xA5EC);
        put_u16(&mut word, 0x02, 0x00C1);
        put_u16(&mut word, 0x0A, spec.flags);
        put_u16(&mut word, 0x20, 14);
        put_u16(&mut word, 0x3E, 22);
        put_u16(&mut word, 0x98, 93);
        let (mut cps, mut pcds) = (vec![0u32], Vec::new());
        for (text, compressed) in spec.pieces {
            let fc = word.len() as u32;
            let chars = if *compressed {
                let bytes = WINDOWS_1252.encode(text).0;
                word.extend_from_slice(&bytes);
                pcds.push((fc * 2) | 0x4000_0000);
                bytes.len()
            } else {
                let units: Vec<u16> = text.encode_utf16().collect();
                units.iter().for_each(|u| word.extend_from_slice(&u.to_le_bytes()));
                pcds.push(fc);
                units.len()
            };
            cps.push(cps.last().unwrap() + chars as u32);
        }
        let total = *cps.last().unwrap();
        put_u32(&mut word, 0x40 + 0x0C, spec.ccp_text.unwrap_or(total));
        let mut table = Vec::new();
        if spec.with_prc {
            table.extend_from_slice(&[0x01, 0x04, 0x00, 0xAA, 0xBB, 0xCC, 0xDD]);
        }
        let mut plc = Vec::new();
        cps.iter().for_each(|cp| plc.extend_from_slice(&cp.to_le_bytes()));
        for fc in pcds {
            plc.extend_from_slice(&[0, 0]);
            plc.extend_from_slice(&fc.to_le_bytes());
            plc.extend_from_slice(&[0, 0]);
        }
        let fc_clx = table.len() as u32;
        table.push(0x02);
        table.extend_from_slice(&(plc.len() as u32).to_le_bytes());
        table.extend_from_slice(&plc);
        put_u32(&mut word, 0x1A2, fc_clx);
        put_u32(&mut word, 0x1A6, table.len() as u32 - fc_clx);
        (word, table)
    }

    fn build_doc(spec: &DocSpec) -> Vec<u8> {
        let (word, table) = doc_streams(spec);
        let table_name = if spec.flags & 0x0200 != 0 { "1Table" } else { "0Table" };
        compound(&[("WordDocument", &word), (table_name, &table)])
    }

    fn doc(pieces: &[(&str, bool)]) -> String {
        doc_to_markdown(&build_doc(&DocSpec { pieces, ..Default::default() })).unwrap()
    }

    #[test]
    fn extrait_le_texte_et_les_paragraphes_d_un_doc() {
        let text = "Premier paragraphe.\rDeuxième   paragraphe\tavec tabulation.\r\r  \rTroisième\u{0B}ligne\u{0C}\
                    Un porte\u{1E}manteau anti\u{1F}constitutionnel\u{01}\u{08}.\r";
        assert_eq!(
            doc(&[(text, true)]),
            "Premier paragraphe.\n\nDeuxième paragraphe avec tabulation.\n\nTroisième\n\nligne\n\n\
             Un porte-manteau anticonstitutionnel.\n"
        );
    }

    #[test]
    fn retire_le_code_des_champs_et_garde_le_resultat() {
        let text = "Voir \u{13} HYPERLINK \"https://exemple.fr\" \u{14}le site\u{15} et la page \u{13}PAGE\u{14}3\u{15}.\r\
                    Réponse : \u{13} IF \u{13} DATE \u{14}12/10\u{15} = \"12/10\" \u{14}Oui\u{15}\r\
                    Sans résultat\u{13} TOC \u{15}.\r";
        assert_eq!(doc(&[(text, true)]), "Voir le site et la page 3.\n\nRéponse : Oui\n\nSans résultat.\n");
    }

    #[test]
    fn met_les_cellules_d_un_tableau_sur_une_ligne() {
        let text = "Avant\rNom\u{07}Rôle\u{07}\u{07}Marie\u{07}Cheffe de projet\u{07}\u{07}Après\r";
        assert_eq!(doc(&[(text, true)]), "Avant\n\nNom | Rôle\n\nMarie | Cheffe de projet\n\nAprès\n");
    }

    #[test]
    fn lit_une_piece_utf16_apres_une_piece_compressee() {
        assert_eq!(doc(&[("Budget : 20 € ", true), ("pour l'œuvre ✓\r", false)]), "Budget : 20 € pour l'œuvre ✓\n");
    }

    #[test]
    fn ne_garde_que_le_texte_principal() {
        let pieces = [("Corps du document.\r", true), ("Note de bas de page.\r", true)];
        let bytes = build_doc(&DocSpec { pieces: &pieces, ccp_text: Some(19), ..Default::default() });
        assert_eq!(doc_to_markdown(&bytes).unwrap(), "Corps du document.\n");
    }

    #[test]
    fn saute_les_prc_et_lit_le_flux_0table() {
        let bytes =
            build_doc(&DocSpec { pieces: &[("Texte.\r", true)], flags: 0, with_prc: true, ..Default::default() });
        assert_eq!(doc_to_markdown(&bytes).unwrap(), "Texte.\n");
    }

    #[test]
    fn refuse_un_doc_protege_par_mot_de_passe() {
        let bytes = build_doc(&DocSpec { pieces: &[("Secret\r", true)], flags: 0x0300, ..Default::default() });
        assert!(doc_to_markdown(&bytes).unwrap_err().to_string().contains("protégé par mot de passe"));
    }

    #[test]
    fn refuse_un_fichier_qui_n_est_pas_un_compose_ole() {
        assert!(doc_to_markdown(b"Ceci n'est pas un document Word").is_err());
        assert!(doc_to_markdown(&[]).is_err());
        assert!(ppt_to_markdown(b"Ceci n'est pas une presentation").is_err());
    }

    #[test]
    fn signale_un_flux_worddocument_absent() {
        let err = doc_to_markdown(&compound(&[("Autre", b"x")])).unwrap_err();
        assert_eq!(err.to_string(), "Fichier .doc illisible : flux WordDocument absent");
    }

    #[test]
    fn refuse_un_flux_worddocument_tronque() {
        let (word, table) = doc_streams(&DocSpec { pieces: &[("Un texte assez long.\r", true)], ..Default::default() });
        // FIB coupée avant fcClx, puis texte coupé au milieu.
        for len in [0x100, word.len() - 5] {
            let bytes = compound(&[("WordDocument", &word[..len]), ("1Table", &table)]);
            assert!(doc_to_markdown(&bytes).is_err());
        }
        let bytes = compound(&[("WordDocument", &word), ("1Table", &table[..table.len() - 3])]);
        assert!(doc_to_markdown(&bytes).is_err());
    }

    /// Variantes tronquées et altérées d'un flux : aucune ne doit faire paniquer.
    fn variants(data: &[u8]) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = (0..data.len()).step_by(13).map(|len| data[..len].to_vec()).collect();
        let mut seed = 0x2545_F491_u32;
        for _ in 0..150 {
            let mut altered = data.to_vec();
            for _ in 0..4 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let pos = (seed >> 8) as usize % altered.len();
                altered[pos] = (seed >> 24) as u8;
            }
            out.push(altered);
        }
        out
    }

    #[test]
    fn ne_panique_pas_sur_des_flux_alteres() {
        let (word, table) = doc_streams(&DocSpec {
            pieces: &[("Avant\rA\u{07}B\u{07}\u{07}\u{13}X\u{14}Y\u{15}\r", true), ("Fin ✓\r", false)],
            with_prc: true,
            ..Default::default()
        });
        for altered in variants(&word) {
            let _ = doc_to_markdown(&compound(&[("WordDocument", &altered), ("1Table", &table)]));
        }
        for altered in variants(&table) {
            let _ = doc_to_markdown(&compound(&[("WordDocument", &word), ("1Table", &altered)]));
        }
        let (stream, current_user) = ppt_with_persist();
        for altered in variants(&stream) {
            let _ = ppt_to_markdown(&compound(&[("PowerPoint Document", &altered), ("Current User", &current_user)]));
            let _ = ppt_to_markdown(&compound(&[("PowerPoint Document", &altered)]));
        }
    }

    // --- PowerPoint ---

    fn record(version: u16, instance: u16, kind: u16, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(version | (instance << 4)).to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    fn atom(kind: u16, data: &[u8]) -> Vec<u8> {
        record(0, 0, kind, data)
    }

    fn container(kind: u16, instance: u16, children: &[Vec<u8>]) -> Vec<u8> {
        record(0x0F, instance, kind, &children.concat())
    }

    fn header(text_type: u32) -> Vec<u8> {
        atom(RT_TEXT_HEADER_ATOM, &text_type.to_le_bytes())
    }

    fn chars(text: &str) -> Vec<u8> {
        let data: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        atom(RT_TEXT_CHARS_ATOM, &data)
    }

    fn bytes8(text: &str) -> Vec<u8> {
        atom(RT_TEXT_BYTES_ATOM, &WINDOWS_1252.encode(text).0)
    }

    fn persist_atom(persist_ref: u32, slide_id: u32) -> Vec<u8> {
        let mut data = vec![0u8; 20];
        put_u32(&mut data, 0, persist_ref);
        put_u32(&mut data, 12, slide_id);
        atom(RT_SLIDE_PERSIST_ATOM, &data)
    }

    /// Forme de dessin OfficeArt contenant des enregistrements de texte.
    fn shape(texts: &[Vec<u8>]) -> Vec<u8> {
        container(0xF004, 0, &[atom(0xF00A, &[0; 8]), container(0xF00D, 0, texts)])
    }

    /// Conteneur Slide (ou Notes) : SlideAtom, puis dessin Drawing > DgContainer > SpgrContainer.
    fn slide_like(kind: u16, first: Vec<u8>, shapes: &[Vec<u8>]) -> Vec<u8> {
        let spgr = container(0xF003, 0, shapes);
        let drawing = container(0x040C, 0, &[container(0xF002, 0, &[atom(0xF008, &[0; 8]), spgr])]);
        container(kind, 0, &[first, drawing])
    }

    fn slide(shapes: &[Vec<u8>]) -> Vec<u8> {
        slide_like(RT_SLIDE, atom(0x03EF, &[0; 24]), shapes)
    }

    fn notes(slide_id: u32, text: &str) -> Vec<u8> {
        let mut notes_atom = vec![0u8; 8];
        put_u32(&mut notes_atom, 0, slide_id);
        slide_like(RT_NOTES, atom(RT_NOTES_ATOM, &notes_atom), &[shape(&[header(TEXT_NOTES), chars(text)])])
    }

    /// Document : SlideListWithText des diapositives (instance 0) et des notes (instance 2).
    fn document(slide_persists: &[(u32, u32)], notes_persists: &[(u32, u32)]) -> Vec<u8> {
        let mut slide_list = Vec::new();
        for (i, &(persist_ref, slide_id)) in slide_persists.iter().enumerate() {
            slide_list.push(persist_atom(persist_ref, slide_id));
            match i {
                // Diapositive 1 : titre présent dans la liste et dans le dessin (doublon évité).
                0 => slide_list.extend([header(TEXT_TITLE), chars("Introduction")]),
                // Diapositive 3 : textes seulement dans la liste, référencés par le dessin.
                2 => slide_list.extend([
                    header(TEXT_CENTER_TITLE),
                    bytes8("Conclusion"),
                    header(1),
                    chars("Merci\rQuestions ?"),
                ]),
                _ => {}
            }
        }
        let notes_list: Vec<Vec<u8>> = notes_persists.iter().map(|&(p, id)| persist_atom(p, id)).collect();
        container(
            RT_DOCUMENT,
            0,
            &[
                atom(0x03E9, &[0; 40]),
                container(
                    RT_SLIDE_LIST_WITH_TEXT,
                    1,
                    &[persist_atom(9, 0x8000_0000), header(TEXT_TITLE), chars("Masque")],
                ),
                container(RT_SLIDE_LIST_WITH_TEXT, 0, &slide_list),
                container(RT_SLIDE_LIST_WITH_TEXT, 2, &notes_list),
            ],
        )
    }

    fn slide_1() -> Vec<u8> {
        slide(&[
            shape(&[header(TEXT_TITLE), chars("Introduction")]),
            shape(&[header(1), chars("Contexte\r\tObjectifs  du\u{0B}projet")]),
        ])
    }

    fn slide_2() -> Vec<u8> {
        // Deux formes : corps et zone de texte libre (type « autre »), en deux blocs.
        slide(&[shape(&[header(1), bytes8("Corps sans titre")]), shape(&[header(4), chars("Encadré")])])
    }

    fn slide_3() -> Vec<u8> {
        slide(&[
            shape(&[atom(RT_OUTLINE_TEXT_REF_ATOM, &0u32.to_le_bytes())]),
            shape(&[atom(RT_OUTLINE_TEXT_REF_ATOM, &1u32.to_le_bytes())]),
        ])
    }

    fn main_master() -> Vec<u8> {
        slide_like(
            0x03F8,
            atom(0x03EF, &[0; 24]),
            &[shape(&[header(TEXT_TITLE), chars("Cliquez pour modifier le titre")])],
        )
    }

    const EXPECTED_PPT: &str = "## Diapositive 1 — Introduction\n\nContexte\nObjectifs du\nprojet\n\n\
                                > Notes : Penser à saluer l'équipe\n\n\
                                ## Diapositive 2\n\nCorps sans titre\n\nEncadré\n\n\
                                ## Diapositive 3 — Conclusion\n\nMerci\nQuestions ?\n";

    #[test]
    fn extrait_les_diapositives_d_un_ppt() {
        let stream = [
            document(&[(2, 256), (3, 257), (4, 258)], &[(5, 512)]),
            main_master(),
            slide_1(),
            slide_2(),
            slide_3(),
            notes(256, "Penser à saluer\rl'équipe"),
        ]
        .concat();
        let (markdown, count) = ppt_to_markdown(&compound(&[("PowerPoint Document", &stream)])).unwrap();
        assert_eq!(markdown, EXPECTED_PPT);
        assert_eq!(count, 3);
    }

    #[test]
    fn numerote_les_diapositives_sans_slidelistwithtext() {
        let stream = [document(&[], &[]), slide_2(), slide_1()].concat();
        let (markdown, count) = ppt_to_markdown(&compound(&[("PowerPoint Document", &stream)])).unwrap();
        assert_eq!(
            markdown,
            "## Diapositive 1\n\nCorps sans titre\n\nEncadré\n\n## Diapositive 2 — Introduction\n\nContexte\nObjectifs du\nprojet\n"
        );
        assert_eq!(count, 2);
    }

    /// Flux dont les diapositives sont rangées dans le désordre, avec une version périmée
    /// de la diapositive 1 ; seule la table de persistance donne le bon ordre.
    fn ppt_with_persist() -> (Vec<u8>, Vec<u8>) {
        let mut stream = slide(&[shape(&[header(TEXT_TITLE), chars("Ancienne version")])]);
        let mut offsets = Vec::new();
        for part in [
            document(&[(2, 256), (3, 257), (4, 258)], &[(5, 512)]),
            slide_3(),
            slide_2(),
            slide_1(),
            notes(256, "Penser à saluer\rl'équipe"),
        ] {
            offsets.push(stream.len() as u32);
            stream.extend(part);
        }
        // persistId 1 : Document, 2 : diapositive 1, 3 : diapositive 2, 4 : diapositive 3, 5 : notes.
        let order = [offsets[0], offsets[3], offsets[2], offsets[1], offsets[4]];
        let mut dir = (1u32 | (5 << 20)).to_le_bytes().to_vec();
        order.iter().for_each(|offset| dir.extend_from_slice(&offset.to_le_bytes()));
        let dir_at = stream.len() as u32;
        stream.extend(atom(RT_PERSIST_DIRECTORY_ATOM, &dir));
        let mut edit = vec![0u8; 28];
        put_u32(&mut edit, 12, dir_at);
        put_u32(&mut edit, 16, 1);
        let edit_at = stream.len() as u32;
        stream.extend(atom(RT_USER_EDIT_ATOM, &edit));
        let mut current_user = vec![0u8; 20];
        put_u32(&mut current_user, 0, 0x14);
        put_u32(&mut current_user, 4, 0xE391_C05F);
        put_u32(&mut current_user, 8, edit_at);
        (stream, atom(RT_CURRENT_USER_ATOM, &current_user))
    }

    #[test]
    fn suit_la_table_de_persistance_pour_l_ordre_des_diapositives() {
        let (stream, current_user) = ppt_with_persist();
        let bytes = compound(&[("PowerPoint Document", &stream), ("Current User", &current_user)]);
        let (markdown, count) = ppt_to_markdown(&bytes).unwrap();
        assert_eq!(markdown, EXPECTED_PPT);
        assert_eq!(count, 3);
    }

    #[test]
    fn signale_une_presentation_protegee_ou_sans_flux() {
        let stream = [document(&[(2, 256)], &[]), slide_1()].concat();
        let bytes = compound(&[("PowerPoint Document", &stream), ("EncryptedSummary", b"x")]);
        assert!(ppt_to_markdown(&bytes).unwrap_err().to_string().contains("protégée par mot de passe"));
        let crypted = [atom(RT_CRYPT_SESSION, &[0; 8]), stream.clone()].concat();
        let bytes = compound(&[("PowerPoint Document", &crypted)]);
        assert!(ppt_to_markdown(&bytes).unwrap_err().to_string().contains("protégée par mot de passe"));
        // Jeton de chiffrement dans CurrentUserAtom.
        let (stream, mut current_user) = ppt_with_persist();
        put_u32(&mut current_user, 12, 0xF3D1_C4DF);
        let bytes = compound(&[("PowerPoint Document", &stream), ("Current User", &current_user)]);
        assert!(ppt_to_markdown(&bytes).unwrap_err().to_string().contains("protégée par mot de passe"));
        let err = ppt_to_markdown(&compound(&[("WordDocument", b"x")])).unwrap_err();
        assert_eq!(err.to_string(), "Fichier .ppt illisible : flux PowerPoint Document absent");
    }

    #[test]
    fn refuse_un_flux_powerpoint_tronque() {
        let stream = [document(&[(2, 256), (3, 257)], &[]), slide_1(), slide_2()].concat();
        let bytes = compound(&[("PowerPoint Document", &stream[..stream.len() - 10])]);
        assert!(ppt_to_markdown(&bytes).is_err());
        let (stream, current_user) = ppt_with_persist();
        let bytes = compound(&[("PowerPoint Document", &stream[..stream.len() / 2]), ("Current User", &current_user)]);
        assert!(ppt_to_markdown(&bytes).is_err());
    }
}
