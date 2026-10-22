//! Mails : lecture des fichiers .eml (MIME, avec `mail-parser`) et .msg (Outlook, [MS-OXMSG],
//! fichier composé OLE lu avec `cfb`), puis Markdown du mail (en-têtes, corps).

use std::collections::HashMap;
use std::io::{Cursor, Read};

use chrono::{DateTime, Local};
use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};
use mail_parser::{Address, ContentType, MessageParser, MimeHeaders, PartType};

use crate::error::{vault_err, Result};

/// Garde-fou : nombre maximal de destinataires et de pièces jointes lus dans un .msg.
const MAX_ENTRIES: usize = 4096;

/// Mail lu dans un fichier .eml ou .msg.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mail {
    pub subject: String,
    /// « Nom <adresse> » ou l'adresse seule
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    /// date et heure locales au format « AAAA-MM-JJ HH:MM », si connue
    pub date: Option<String>,
    /// corps texte
    pub text: Option<String>,
    /// corps HTML
    pub html: Option<String>,
    /// pièces jointes : (nom de fichier, contenu) ; les images incorporées au corps (inline / cid) sont exclues
    pub attachments: Vec<(String, Vec<u8>)>,
}

/// « Nom <adresse> », l'adresse seule si le nom manque ou la répète, ou le nom seul.
fn format_address(name: Option<&str>, address: Option<&str>) -> Option<String> {
    let name = name.map(|n| n.trim().trim_matches(['"', '\'']).trim()).filter(|n| !n.is_empty());
    let address = address.map(str::trim).filter(|a| !a.is_empty());
    match (name, address) {
        (Some(name), Some(address)) if !name.eq_ignore_ascii_case(address) => Some(format!("{name} <{address}>")),
        (_, Some(address)) => Some(address.to_string()),
        (Some(name), None) => Some(name.to_string()),
        (None, None) => None,
    }
}

/// Horodatage Unix en date et heure locales « AAAA-MM-JJ HH:MM ».
fn local_date(timestamp: i64) -> Option<String> {
    // Dates aberrantes (avant 1970, après 2200) ignorées.
    if !(0..=7_258_118_400).contains(&timestamp) {
        return None;
    }
    let utc = DateTime::from_timestamp(timestamp, 0)?;
    Some(utc.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
}

fn non_empty(text: String) -> Option<String> {
    if text.trim().is_empty() { None } else { Some(text) }
}

/// Vrai si le HTML (déjà en minuscules) affiche la partie de ce Content-ID (`cid:…`).
fn cid_referenced(html_lower: Option<&str>, cid: &str) -> bool {
    let cid = cid.trim().trim_matches(['<', '>']).to_lowercase();
    !cid.is_empty() && html_lower.is_some_and(|html| html.contains(&format!("cid:{cid}")))
}

// --- .eml ---------------------------------------------------------------------------

fn eml_addresses(address: Option<&Address>) -> Vec<String> {
    let Some(address) = address else { return Vec::new() };
    address.iter().filter_map(|addr| format_address(addr.name.as_deref(), addr.address.as_deref())).collect()
}

/// Nom de repli d'une pièce jointe sans nom, avec une extension déduite du type MIME.
fn fallback_name(index: usize, content_type: Option<&ContentType>) -> String {
    let subtype = content_type.and_then(|ct| ct.subtype()).map(str::to_ascii_lowercase);
    let extension = match subtype.as_deref() {
        Some("rfc822") => "eml",
        Some("plain") => "txt",
        Some("html") => "html",
        Some("calendar") => "ics",
        Some("pdf") => "pdf",
        Some("png") => "png",
        Some("jpeg") => "jpg",
        Some("gif") => "gif",
        _ => "bin",
    };
    format!("piece-jointe-{index}.{extension}")
}

/// Lit un mail au format MIME (.eml).
pub fn parse_eml(bytes: &[u8]) -> Result<Mail> {
    let not_mail = || vault_err("Fichier .eml illisible : ce n'est pas un mail");
    let message = MessageParser::default().parse(bytes).ok_or_else(not_mail)?;
    if message.from().is_none() && message.to().is_none() && message.subject().is_none() && message.date().is_none() {
        return Err(not_mail());
    }
    let from = eml_addresses(message.from().or_else(|| message.sender()));
    let text: Vec<&str> = message
        .text_bodies()
        .filter_map(|part| match &part.body {
            PartType::Text(text) => Some(text.as_ref()),
            _ => None,
        })
        .collect();
    let html: Vec<&str> = message
        .html_bodies()
        .filter_map(|part| match &part.body {
            PartType::Html(html) => Some(html.as_ref()),
            _ => None,
        })
        .collect();
    let html = non_empty(html.join("\n"));
    let html_lower = html.as_deref().map(str::to_lowercase);
    let mut attachments = Vec::new();
    for (i, part) in message.attachments().enumerate() {
        if matches!(part.body, PartType::Multipart(_)) {
            continue;
        }
        if part.content_id().is_some_and(|cid| cid_referenced(html_lower.as_deref(), cid)) {
            continue;
        }
        let name = part
            .attachment_name()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| fallback_name(i + 1, part.content_type()));
        attachments.push((name, part.contents().to_vec()));
    }
    Ok(Mail {
        subject: message.subject().unwrap_or_default().trim().to_string(),
        from: from.into_iter().next().unwrap_or_default(),
        to: eml_addresses(message.to()),
        cc: eml_addresses(message.cc()),
        date: message.date().filter(|date| date.is_valid()).and_then(|date| local_date(date.to_timestamp())),
        text: non_empty(text.join("\n")),
        html,
        attachments,
    })
}

// --- .msg ---------------------------------------------------------------------------

type Compound<'a> = cfb::CompoundFile<Cursor<&'a [u8]>>;

/// Propriétés de taille fixe d'un flux `__properties_version1.0` : après un en-tête de
/// `header` octets, entrées de 16 octets (étiquette, options, valeur sur 8 octets).
type Properties = HashMap<u16, (u16, [u8; 8])>;

fn fixed_properties(data: &[u8], header: usize) -> Properties {
    let mut props = Properties::new();
    for entry in data.get(header..).unwrap_or_default().chunks_exact(16) {
        let Some(tag) = entry.get(..4).and_then(|tag| tag.try_into().ok()).map(u32::from_le_bytes) else { continue };
        let Some(value) = entry.get(8..16).and_then(|value| value.try_into().ok()) else { continue };
        props.insert((tag >> 16) as u16, (tag as u16, value));
    }
    props
}

/// Propriété PT_LONG (0x0003).
fn long_prop(props: &Properties, id: u16) -> Option<u32> {
    match props.get(&id) {
        Some((0x0003, value)) => Some(u32::from_le_bytes([value[0], value[1], value[2], value[3]])),
        _ => None,
    }
}

/// Propriété PT_BOOLEAN (0x000B).
fn bool_prop(props: &Properties, id: u16) -> bool {
    matches!(props.get(&id), Some((0x000B, value)) if value[0] != 0 || value[1] != 0)
}

/// Propriété PT_SYSTIME (0x0040), FILETIME converti en date locale.
fn time_prop(props: &Properties, id: u16) -> Option<String> {
    let Some((0x0040, value)) = props.get(&id) else { return None };
    let filetime = u64::from_le_bytes(*value);
    if filetime == 0 {
        return None;
    }
    // FILETIME : intervalles de 100 ns depuis le 1er janvier 1601 (UTC).
    local_date((filetime / 10_000_000) as i64 - 11_644_473_600)
}

/// Encodage d'une page de code Windows (PR_MESSAGE_CODEPAGE, PR_INTERNET_CPID).
fn codepage_encoding(codepage: u32) -> Option<&'static Encoding> {
    match codepage {
        65001 => Some(UTF_8),
        1252 | 20127 | 28591 => Some(WINDOWS_1252),
        874 | 1250..=1258 => Encoding::for_label(format!("windows-{codepage}").as_bytes()),
        28592..=28605 => Encoding::for_label(format!("iso-8859-{}", codepage - 28590).as_bytes()),
        932 => Some(encoding_rs::SHIFT_JIS),
        936 => Some(encoding_rs::GBK),
        949 => Some(encoding_rs::EUC_KR),
        950 => Some(encoding_rs::BIG5),
        _ => None,
    }
}

/// Texte 8 bits : page de code du message si connue, sinon UTF-8 s'il est valide, sinon cp1252.
fn decode_8bit(bytes: &[u8], encoding: Option<&'static Encoding>) -> String {
    let encoding = encoding.unwrap_or(if std::str::from_utf8(bytes).is_ok() { UTF_8 } else { WINDOWS_1252 });
    encoding.decode_without_bom_handling(bytes).0.into_owned()
}

/// Corps HTML binaire : BOM, sinon charset déclaré dans le HTML, sinon PR_INTERNET_CPID.
fn decode_html(bytes: &[u8], cpid: Option<&'static Encoding>) -> String {
    let head = String::from_utf8_lossy(bytes.get(..4096).unwrap_or(bytes)).to_ascii_lowercase();
    let declared = head.find("charset=").and_then(|at| {
        let label: String = head
            .get(at + 8..)
            .unwrap_or_default()
            .trim_start_matches(['"', '\'', ' '])
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
            .collect();
        Encoding::for_label(label.as_bytes()).map(Encoding::output_encoding)
    });
    let fallback = if std::str::from_utf8(bytes).is_ok() { UTF_8 } else { WINDOWS_1252 };
    declared.or(cpid).unwrap_or(fallback).decode(bytes).0.into_owned()
}

fn utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).filter_map(|c| c.try_into().ok()).map(u16::from_le_bytes).collect();
    String::from_utf16_lossy(&units)
}

struct MsgReader<'a> {
    comp: Compound<'a>,
    /// Page de code des chaînes 8 bits (PR_MESSAGE_CODEPAGE).
    ansi: Option<&'static Encoding>,
}

impl MsgReader<'_> {
    /// Contenu d'un flux, `None` s'il n'existe pas.
    fn stream(&mut self, path: &str) -> Result<Option<Vec<u8>>> {
        if !self.comp.is_stream(path) {
            return Ok(None);
        }
        let corrupt = |_| vault_err(format!("Fichier .msg illisible : flux {path} corrompu"));
        let mut data = Vec::new();
        self.comp.open_stream(path).map_err(corrupt)?.read_to_end(&mut data).map_err(corrupt)?;
        Ok(Some(data))
    }

    fn properties(&mut self, dir: &str, header: usize) -> Result<Properties> {
        let data = self.stream(&format!("{dir}__properties_version1.0"))?.unwrap_or_default();
        Ok(fixed_properties(&data, header))
    }

    /// Propriété chaîne `__substg1.0_XXXX001F` (UTF-16LE) ou `…001E` (8 bits), non vide.
    fn string(&mut self, dir: &str, id: u16) -> Result<Option<String>> {
        let base = format!("{dir}__substg1.0_{id:04X}");
        let text = match self.stream(&format!("{base}001F"))? {
            Some(data) => utf16le(&data),
            None => match self.stream(&format!("{base}001E"))? {
                Some(data) => decode_8bit(&data, self.ansi),
                None => return Ok(None),
            },
        };
        Ok(non_empty(text.trim_end_matches('\0').trim().to_string()))
    }

    /// Première des propriétés chaînes présente.
    fn first_string(&mut self, dir: &str, ids: &[u16]) -> Result<Option<String>> {
        for &id in ids {
            if let Some(text) = self.string(dir, id)? {
                return Ok(Some(text));
            }
        }
        Ok(None)
    }

    /// Première adresse SMTP parmi ces propriétés (une adresse Exchange « /O=… » est ignorée).
    fn first_address(&mut self, dir: &str, ids: &[u16]) -> Result<Option<String>> {
        for &id in ids {
            if let Some(address) = self.string(dir, id)?.filter(|a| a.contains('@')) {
                return Ok(Some(address));
            }
        }
        Ok(None)
    }

    /// Stockages de premier niveau dont le nom commence par `prefix`, triés.
    fn storages(&self, prefix: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .comp
            .read_root_storage()
            .filter(|entry| entry.is_storage() && entry.name().starts_with(prefix))
            .map(|entry| entry.name().to_string())
            .take(MAX_ENTRIES)
            .collect();
        names.sort();
        names
    }
}

fn display_list(text: Option<String>) -> Vec<String> {
    let text = text.unwrap_or_default();
    text.split(';').map(str::trim).filter(|name| !name.is_empty()).map(str::to_string).collect()
}

/// Lit un mail Outlook (.msg, fichier composé OLE).
pub fn parse_msg(bytes: &[u8]) -> Result<Mail> {
    let comp = cfb::CompoundFile::open(Cursor::new(bytes))
        .map_err(|_| vault_err("Fichier .msg illisible : ce n'est pas un mail Outlook (fichier OLE attendu)"))?;
    let mut msg = MsgReader { comp, ansi: None };
    if !msg.comp.is_stream("__properties_version1.0") {
        return Err(vault_err("Fichier .msg illisible : propriétés du message absentes"));
    }
    // En-tête de 32 octets pour le message de premier niveau.
    let props = msg.properties("", 32)?;
    msg.ansi = long_prop(&props, 0x3FFD).and_then(codepage_encoding);

    let sender = format_address(
        msg.first_string("", &[0x0C1A, 0x0042])?.as_deref(),
        msg.first_address("", &[0x5D01, 0x0C1F, 0x5D02, 0x0065])?.as_deref(),
    );
    let html = match msg.stream("__substg1.0_10130102")? {
        Some(data) => non_empty(decode_html(&data, long_prop(&props, 0x3FDE).and_then(codepage_encoding))),
        None => msg.string("", 0x1013)?,
    };
    let html_lower = html.as_deref().map(str::to_lowercase);

    // Destinataires : PR_RECIPIENT_TYPE (1 À, 2 Cc, 3 Cci), nom et adresse SMTP.
    let (mut to, mut cc) = (Vec::new(), Vec::new());
    for storage in msg.storages("__recip_version1.0_#") {
        let dir = format!("{storage}/");
        let kind = long_prop(&msg.properties(&dir, 8)?, 0x0C15).unwrap_or(1) & 0x0F;
        let name = msg.first_string(&dir, &[0x3001])?;
        let address = msg.first_address(&dir, &[0x39FE, 0x3003])?;
        if let Some(recipient) = format_address(name.as_deref(), address.as_deref()) {
            match kind {
                1 => to.push(recipient),
                2 => cc.push(recipient),
                _ => {}
            }
        }
    }
    // À défaut, listes affichées PR_DISPLAY_TO / PR_DISPLAY_CC (noms séparés par « ; »).
    if to.is_empty() {
        to = display_list(msg.string("", 0x0E04)?);
    }
    if cc.is_empty() {
        cc = display_list(msg.string("", 0x0E03)?);
    }

    let mut attachments = Vec::new();
    for (i, storage) in msg.storages("__attach_version1.0_#").into_iter().enumerate() {
        let dir = format!("{storage}/");
        // Message incorporé ou objet OLE : données en stockage plutôt qu'en flux, ignoré.
        if msg.comp.is_storage(format!("{dir}__substg1.0_3701000D")) {
            continue;
        }
        let Some(data) = msg.stream(&format!("{dir}__substg1.0_37010102"))? else { continue };
        // PR_ATTACHMENT_HIDDEN, puis image incorporée au corps (PR_ATTACH_CONTENT_ID référencé).
        if bool_prop(&msg.properties(&dir, 8)?, 0x7FFE) {
            continue;
        }
        if msg.string(&dir, 0x3712)?.is_some_and(|cid| cid_referenced(html_lower.as_deref(), &cid)) {
            continue;
        }
        let name = match msg.first_string(&dir, &[0x3707, 0x3704, 0x3001])? {
            Some(name) => name,
            None => format!("piece-jointe-{}{}", i + 1, msg.string(&dir, 0x3703)?.unwrap_or_default()),
        };
        attachments.push((name, data));
    }

    Ok(Mail {
        subject: msg.first_string("", &[0x0037, 0x0E1D])?.unwrap_or_default(),
        from: sender.unwrap_or_default(),
        to,
        cc,
        date: time_prop(&props, 0x0039).or_else(|| time_prop(&props, 0x0E06)),
        text: msg.string("", 0x1000)?,
        html,
        attachments,
    })
}

// --- Markdown -----------------------------------------------------------------------

fn push_header(lines: &mut Vec<String>, label: &str, value: &str) {
    if !value.trim().is_empty() {
        lines.push(format!("**{label}** : {}", value.trim()));
    }
}

/// Lignes vides successives réduites à une, espaces de fin retirés, sans ligne vide aux bords.
fn clean_body(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<&str> = Vec::new();
    let mut blank = false;
    for line in text.lines().map(str::trim_end) {
        if line.is_empty() {
            blank = !lines.is_empty();
            continue;
        }
        if blank {
            lines.push("");
            blank = false;
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// Entité HTML nommée courante ou numérique (`&#233;`, `&#xE9;`).
fn entity(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "eacute" => 'é',
        "egrave" => 'è',
        "ecirc" => 'ê',
        "euml" => 'ë',
        "agrave" => 'à',
        "acirc" => 'â',
        "ccedil" => 'ç',
        "icirc" => 'î',
        "iuml" => 'ï',
        "ocirc" => 'ô',
        "ugrave" => 'ù',
        "ucirc" => 'û',
        "uuml" => 'ü',
        "Eacute" => 'É',
        "Egrave" => 'È',
        "Agrave" => 'À',
        "Ccedil" => 'Ç',
        "oelig" => 'œ',
        "OElig" => 'Œ',
        "euro" => '€',
        "laquo" => '«',
        "raquo" => '»',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "hellip" => '…',
        "ndash" => '–',
        "mdash" => '—',
        "bull" => '•',
        "middot" => '·',
        "deg" => '°',
        "copy" => '©',
        "reg" => '®',
        _ => return None,
    })
}

/// Texte entre deux balises : entités décodées, blancs ASCII successifs réduits à une espace
/// (les espaces insécables sont gardées, comme en HTML).
fn push_html_text(out: &mut String, text: &str) {
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        let (before, tail) = rest.split_at(amp);
        decoded.push_str(before);
        let tail = tail.get(1..).unwrap_or_default();
        let semi = tail.char_indices().take(12).find(|&(_, c)| c == ';').map(|(at, _)| at);
        match semi.and_then(|at| Some((entity(tail.get(..at)?)?, at))) {
            Some((c, at)) => {
                decoded.push(c);
                rest = tail.get(at + 1..).unwrap_or_default();
            }
            None => {
                decoded.push('&');
                rest = tail;
            }
        }
    }
    decoded.push_str(rest);
    let mut after_space = out.ends_with([' ', '\n']);
    for c in decoded.chars() {
        if c.is_ascii_whitespace() {
            if !after_space {
                out.push(' ');
            }
            after_space = true;
        } else {
            out.push(c);
            after_space = false;
        }
    }
}

/// Repli quand la conversion du HTML échoue : balises retirées, entités courantes décodées,
/// `<br>` et blocs (`<p>`, `<div>`, titres, listes, lignes de tableau) en sauts de ligne.
fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::new();
    let mut pos = 0;
    while let Some(found) = html.get(pos..).and_then(|rest| rest.find('<')) {
        let start = pos + found;
        push_html_text(&mut out, html.get(pos..start).unwrap_or_default());
        let rest = lower.get(start..).unwrap_or_default();
        if rest.starts_with("<!--") {
            pos = rest.find("-->").map_or(html.len(), |end| start + end + 3);
            continue;
        }
        let Some(len) = rest.find('>') else {
            pos = html.len();
            break;
        };
        pos = start + len + 1;
        let tag = rest.get(1..len).unwrap_or_default();
        let closing = tag.starts_with('/');
        let name: String = tag.trim_start_matches('/').chars().take_while(char::is_ascii_alphanumeric).collect();
        match name.as_str() {
            // Contenu ignoré jusqu'à la balise fermante.
            "head" | "style" | "script" | "title" if !closing => {
                let after = lower.get(pos..).unwrap_or_default();
                pos = match after.find(&format!("</{name}")) {
                    Some(close) => {
                        after.get(close..).and_then(|c| c.find('>')).map_or(html.len(), |end| pos + close + end + 1)
                    }
                    None => html.len(),
                };
            }
            "br" => out.push('\n'),
            "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "table" | "ul" | "ol" | "blockquote" => {
                out.push_str("\n\n")
            }
            "div" | "tr" | "li" | "hr" | "pre" => out.push('\n'),
            "td" | "th" => out.push(' '),
            _ => {}
        }
    }
    push_html_text(&mut out, html.get(pos..).unwrap_or_default());
    out.lines().map(str::trim).collect::<Vec<_>>().join("\n")
}

/// Corps du mail en Markdown : texte, sinon HTML converti, sinon repli simple.
fn body_markdown(mail: &Mail, html_to_markdown: &dyn Fn(&str) -> Result<String>) -> String {
    if let Some(text) = mail.text.as_deref().filter(|text| !text.trim().is_empty()) {
        return clean_body(text);
    }
    let Some(html) = mail.html.as_deref().filter(|html| !html.trim().is_empty()) else { return String::new() };
    match html_to_markdown(html).map(|markdown| clean_body(&markdown)) {
        Ok(markdown) if !markdown.is_empty() => markdown,
        _ => clean_body(&html_to_text(html)),
    }
}

/// Markdown du mail : en-têtes puis corps. Le corps texte est préféré ; à défaut, le HTML
/// est converti par `html_to_markdown` (pandoc dans Tempo), et si cette conversion échoue,
/// par un repli simple (balises retirées, entités courantes décodées, <br>/<p> en sauts de ligne).
pub fn mail_to_markdown(mail: &Mail, html_to_markdown: &dyn Fn(&str) -> Result<String>) -> String {
    let mut lines = Vec::new();
    push_header(&mut lines, "De", &mail.from);
    push_header(&mut lines, "À", &mail.to.join(", "));
    push_header(&mut lines, "Cc", &mail.cc.join(", "));
    push_header(&mut lines, "Date", mail.date.as_deref().unwrap_or_default());
    let names: Vec<&str> = mail.attachments.iter().map(|(name, _)| name.as_str()).collect();
    push_header(&mut lines, "Pièces jointes", &names.join(", "));
    // Deux espaces en fin de ligne : saut de ligne Markdown entre les en-têtes.
    let mut markdown = lines.join("  \n");
    let body = body_markdown(mail, html_to_markdown);
    if !body.is_empty() {
        if !markdown.is_empty() {
            markdown.push_str("\n\n");
        }
        markdown.push_str(&body);
    }
    if !markdown.is_empty() {
        markdown.push('\n');
    }
    markdown
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use chrono::{TimeZone, Utc};

    fn local(utc: DateTime<Utc>) -> String {
        utc.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
    }

    const EML: &str = "From: =?UTF-8?Q?Jean_Dupr=C3=A9?= <jean.dupre@exemple.fr>
To: Marie <marie@exemple.fr>, paul@exemple.fr
Cc: =?ISO-8859-1?B?yXF1aXBl?= <equipe@exemple.fr>
Subject: =?UTF-8?Q?R=C3=A9union_de_lancement?=
Date: Mon, 12 Oct 2026 14:03:00 +0200
MIME-Version: 1.0
Content-Type: multipart/mixed; boundary=\"mixte\"

--mixte
Content-Type: multipart/related; boundary=\"lie\"

--lie
Content-Type: multipart/alternative; boundary=\"alt\"

--alt
Content-Type: text/plain; charset=utf-8
Content-Transfer-Encoding: quoted-printable

Bonjour =C3=A0 tous,  =20


Voici le planning.
--alt
Content-Type: text/html; charset=utf-8

<html><body><p>Bonjour &agrave; tous,</p><p><img src=\"cid:logo@exemple\"></p></body></html>
--alt--

--lie
Content-Type: image/png
Content-ID: <logo@exemple>
Content-Disposition: inline; filename=\"logo.png\"
Content-Transfer-Encoding: base64

iVBORw0KGgo=
--lie--

--mixte
Content-Type: application/pdf; name=\"specification.pdf\"
Content-Disposition: attachment; filename=\"=?UTF-8?Q?sp=C3=A9cification.pdf?=\"
Content-Transfer-Encoding: base64

JVBERi0xLjQK
--mixte--
";

    #[test]
    fn lit_un_eml_multipart_avec_pieces_jointes() {
        let mail = parse_eml(EML.replace('\n', "\r\n").as_bytes()).unwrap();
        assert_eq!(mail.subject, "Réunion de lancement");
        assert_eq!(mail.from, "Jean Dupré <jean.dupre@exemple.fr>");
        assert_eq!(mail.to, ["Marie <marie@exemple.fr>", "paul@exemple.fr"]);
        assert_eq!(mail.cc, ["Équipe <equipe@exemple.fr>"]);
        assert_eq!(mail.date.as_deref(), Some(local(Utc.with_ymd_and_hms(2026, 10, 12, 12, 3, 0).unwrap()).as_str()));
        assert!(mail.text.as_deref().unwrap().contains("Bonjour à tous,"));
        assert!(mail.html.as_deref().unwrap().contains("cid:logo@exemple"));
        // L'image incorporée au corps (cid) est exclue des pièces jointes.
        assert_eq!(mail.attachments, [("spécification.pdf".to_string(), b"%PDF-1.4\n".to_vec())]);

        let markdown = mail_to_markdown(&mail, &|_| Err(vault_err("pandoc absent")));
        let expected = format!(
            "**De** : Jean Dupré <jean.dupre@exemple.fr>  \n**À** : Marie <marie@exemple.fr>, paul@exemple.fr  \n\
             **Cc** : Équipe <equipe@exemple.fr>  \n**Date** : {}  \n**Pièces jointes** : spécification.pdf\n\n\
             Bonjour à tous,\n\nVoici le planning.\n",
            mail.date.as_deref().unwrap()
        );
        assert_eq!(markdown, expected);
    }

    /// Fichier composé OLE construit en mémoire (stockages parents créés au besoin).
    fn compound(storages: &[&str], streams: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut comp = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        for storage in storages {
            comp.create_storage_all(storage).unwrap();
        }
        for (path, data) in streams {
            if let Some((parent, _)) = path.rsplit_once('/') {
                comp.create_storage_all(parent).unwrap();
            }
            comp.create_stream(path).unwrap().write_all(data).unwrap();
        }
        comp.flush().unwrap();
        comp.into_inner().into_inner()
    }

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
    }

    /// Flux `__properties_version1.0` : en-tête puis entrées (étiquette, valeur).
    fn properties(header: usize, entries: &[(u32, [u8; 8])]) -> Vec<u8> {
        let mut data = vec![0u8; header];
        for (tag, value) in entries {
            data.extend_from_slice(&tag.to_le_bytes());
            data.extend_from_slice(&6u32.to_le_bytes());
            data.extend_from_slice(value);
        }
        data
    }

    fn long(value: u32) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[..4].copy_from_slice(&value.to_le_bytes());
        out
    }

    fn recipient(index: u32, kind: u32, name: &str, smtp: (u16, &str)) -> Vec<(String, Vec<u8>)> {
        let dir = format!("__recip_version1.0_#{index:08X}");
        vec![
            (format!("{dir}/__properties_version1.0"), properties(8, &[(0x0C15_0003, long(kind))])),
            (format!("{dir}/__substg1.0_3001001F"), utf16(name)),
            (format!("{dir}/__substg1.0_{:04X}001F", smtp.0), utf16(smtp.1)),
        ]
    }

    fn attachment(
        index: u32,
        props: &[(u32, [u8; 8])],
        strings: &[(u16, &str)],
        data: Option<&[u8]>,
    ) -> Vec<(String, Vec<u8>)> {
        let dir = format!("__attach_version1.0_#{index:08X}");
        let mut streams = vec![(format!("{dir}/__properties_version1.0"), properties(8, props))];
        for (id, text) in strings {
            streams.push((format!("{dir}/__substg1.0_{id:04X}001F"), utf16(text)));
        }
        if let Some(data) = data {
            streams.push((format!("{dir}/__substg1.0_37010102"), data.to_vec()));
        }
        streams
    }

    fn build_msg(storages: &[&str], streams: Vec<(String, Vec<u8>)>) -> Vec<u8> {
        let streams: Vec<(&str, Vec<u8>)> = streams.iter().map(|(path, data)| (path.as_str(), data.clone())).collect();
        compound(storages, &streams)
    }

    #[test]
    fn lit_un_msg_outlook() {
        let sent = Utc.with_ymd_and_hms(2026, 10, 12, 12, 3, 0).unwrap();
        let filetime = (sent.timestamp() as u64 + 11_644_473_600) * 10_000_000;
        let html = "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=windows-1252\"></head>\
                    <body><p>Réunion</p><img src=\"cid:image001.png@01DA\"></body></html>";
        let mut streams = vec![
            ("__properties_version1.0".to_string(), properties(32, &[(0x0039_0040, filetime.to_le_bytes())])),
            ("__substg1.0_0037001F".to_string(), utf16("Compte rendu")),
            ("__substg1.0_0C1A001F".to_string(), utf16("Jean Dupont")),
            ("__substg1.0_0C1F001F".to_string(), utf16("/O=EXEMPLE/OU=EXCHANGE/CN=RECIPIENTS/CN=JDUPONT")),
            ("__substg1.0_5D01001F".to_string(), utf16("jean.dupont@exemple.fr")),
            ("__substg1.0_0E04001F".to_string(), utf16("Marie Martin; Paul Durand")),
            ("__substg1.0_1000001F".to_string(), utf16("Bonjour,\r\n\r\nVoici le compte rendu.\r\n")),
            ("__substg1.0_10130102".to_string(), WINDOWS_1252.encode(html).0.into_owned()),
        ];
        streams.extend(recipient(0, 1, "Marie Martin", (0x39FE, "marie.martin@exemple.fr")));
        streams.extend(recipient(1, 1, "Paul Durand", (0x3003, "paul.durand@exemple.fr")));
        streams.extend(recipient(2, 2, "Équipe projet", (0x39FE, "equipe@exemple.fr")));
        streams.extend(recipient(3, 3, "Copie cachée", (0x39FE, "cache@exemple.fr")));
        streams.extend(attachment(0, &[], &[(0x3707, "planning.xlsx"), (0x3704, "PLANNI~1.XLS")], Some(b"PK\x03\x04")));
        streams.extend(attachment(
            1,
            &[],
            &[(0x3707, "image001.png"), (0x3712, "image001.png@01DA")],
            Some(b"\x89PNG"),
        ));
        streams.extend(attachment(2, &[(0x7FFE_000B, long(1))], &[(0x3707, "cache.txt")], Some(b"x")));
        streams.extend(attachment(3, &[], &[(0x3001, "Message transféré")], None));
        streams.extend(attachment(4, &[], &[(0x3704, "NOTES.TXT")], Some(b"notes")));
        let bytes = build_msg(&["__attach_version1.0_#00000003/__substg1.0_3701000D"], streams);

        let mail = parse_msg(&bytes).unwrap();
        assert_eq!(mail.subject, "Compte rendu");
        assert_eq!(mail.from, "Jean Dupont <jean.dupont@exemple.fr>");
        assert_eq!(mail.to, ["Marie Martin <marie.martin@exemple.fr>", "Paul Durand <paul.durand@exemple.fr>"]);
        assert_eq!(mail.cc, ["Équipe projet <equipe@exemple.fr>"]);
        assert_eq!(mail.date, Some(local(sent)));
        assert_eq!(mail.text.as_deref(), Some("Bonjour,\r\n\r\nVoici le compte rendu."));
        assert!(mail.html.as_deref().unwrap().contains("<p>Réunion</p>"));
        assert_eq!(
            mail.attachments,
            [("planning.xlsx".to_string(), b"PK\x03\x04".to_vec()), ("NOTES.TXT".to_string(), b"notes".to_vec())]
        );
    }

    #[test]
    fn lit_les_destinataires_affiches_et_les_chaines_8_bits_d_un_msg() {
        let streams = vec![
            ("__properties_version1.0".to_string(), properties(32, &[(0x3FFD_0003, long(1252))])),
            ("__substg1.0_0037001E".to_string(), WINDOWS_1252.encode("Réunion d'équipe").0.into_owned()),
            ("__substg1.0_0042001F".to_string(), utf16("Jean Dupont")),
            ("__substg1.0_0E04001F".to_string(), utf16("Marie Martin; Paul Durand;")),
            ("__substg1.0_0E03001F".to_string(), utf16("Équipe projet")),
        ];
        let mail = parse_msg(&build_msg(&[], streams)).unwrap();
        assert_eq!(mail.subject, "Réunion d'équipe");
        assert_eq!(mail.from, "Jean Dupont");
        assert_eq!(mail.to, ["Marie Martin", "Paul Durand"]);
        assert_eq!(mail.cc, ["Équipe projet"]);
        assert_eq!(mail.date, None);
        assert!(mail.text.is_none() && mail.html.is_none() && mail.attachments.is_empty());
    }

    fn sample() -> Mail {
        Mail {
            subject: "Planning".into(),
            from: "Jean Dupont <jean.dupont@exemple.fr>".into(),
            to: vec!["Marie <marie@exemple.fr>".into(), "Paul <paul@exemple.fr>".into()],
            cc: vec!["Équipe <equipe@exemple.fr>".into()],
            date: Some("2026-10-12 14:03".into()),
            text: None,
            html: Some(
                "<html><head><title>Planning</title><style>p { color: red; }</style></head><body>\n\
                 <p>Bonjour&nbsp;Marie,</p>\n<p>Voici le <b>planning</b> &amp; le budget&#8239;:<br>\n  à valider.</p>\
                 <!-- commentaire --><div>Jean</div></body></html>"
                    .into(),
            ),
            attachments: vec![("spec.pdf".into(), vec![1]), ("planning.xlsx".into(), vec![2])],
        }
    }

    const HEADERS: &str = "**De** : Jean Dupont <jean.dupont@exemple.fr>  \n\
                           **À** : Marie <marie@exemple.fr>, Paul <paul@exemple.fr>  \n\
                           **Cc** : Équipe <equipe@exemple.fr>  \n\
                           **Date** : 2026-10-12 14:03  \n\
                           **Pièces jointes** : spec.pdf, planning.xlsx\n\n";

    #[test]
    fn convertit_le_html_par_le_repli_si_la_conversion_echoue() {
        let markdown = mail_to_markdown(&sample(), &|_| Err(vault_err("pandoc absent")));
        assert_eq!(
            markdown,
            format!("{HEADERS}Bonjour Marie,\n\nVoici le planning & le budget\u{202F}:\nà valider.\n\nJean\n")
        );
    }

    #[test]
    fn utilise_la_conversion_html_puis_prefere_le_corps_texte() {
        let mut mail = sample();
        let convert = |html: &str| -> Result<String> {
            assert!(html.contains("<b>planning</b>"));
            Ok("Bonjour Marie,   \n\n\n\nVoici le **planning**.\n\n".into())
        };
        assert_eq!(mail_to_markdown(&mail, &convert), format!("{HEADERS}Bonjour Marie,\n\nVoici le **planning**.\n"));
        mail.text = Some("Texte brut.\r\n".into());
        mail.cc.clear();
        mail.attachments.clear();
        assert_eq!(
            mail_to_markdown(&mail, &convert),
            "**De** : Jean Dupont <jean.dupont@exemple.fr>  \n**À** : Marie <marie@exemple.fr>, Paul <paul@exemple.fr>  \n\
             **Date** : 2026-10-12 14:03\n\nTexte brut.\n"
        );
        assert_eq!(mail_to_markdown(&Mail::default(), &convert), "");
    }

    #[test]
    fn refuse_une_entree_qui_n_est_pas_un_mail() {
        assert!(parse_eml(b"").is_err());
        assert!(parse_eml(b"\x00\x01\x02 pas un mail \xFF\xFE").is_err());
        assert!(parse_eml(b"Ceci n'est pas un mail.\r\nNi un en-tete.").is_err());
        assert!(parse_msg(b"Ceci n'est pas un mail Outlook").is_err());
        let err = parse_msg(&compound(&[], &[("WordDocument", vec![0; 16])])).unwrap_err();
        assert_eq!(err.to_string(), "Fichier .msg illisible : propriétés du message absentes");
    }

    #[test]
    fn ne_panique_pas_sur_des_msg_alteres() {
        let props = properties(32, &[(0x0039_0040, [0xFF; 8]), (0x3FFD_0003, long(65001))]);
        for len in 0..props.len() {
            let bytes = compound(
                &[],
                &[("__properties_version1.0", props[..len].to_vec()), ("__substg1.0_0037001F", vec![0x41])],
            );
            let _ = parse_msg(&bytes);
        }
        let bytes = build_msg(&[], attachment(0, &[(0x7FFE_000B, [0; 8])], &[], Some(b"x")));
        assert!(parse_msg(&bytes).is_err());
    }
}
