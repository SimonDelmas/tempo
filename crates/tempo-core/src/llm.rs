//! Client LLM : API compatible OpenAI (Vibe) + implémentation factice pour les tests.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use regex::Regex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::Settings;
use crate::error::{Error, Result};

pub const TEMPERATURE: f64 = 0.2;
pub const JSON_HINT: &str = "Réponds uniquement avec un objet json valide.";

static FENCED_JSON_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)```(?:json)?\s*(.*?)```").unwrap());
static FENCED_MD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)\A\s*```(?:markdown|md)?\s*\n(.*?)\n```\s*\z").unwrap());

/// Message d'une conversation avec le modèle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Msg {
    pub role: String,
    pub content: String,
}

impl Msg {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Msg { role: role.into(), content: content.into() }
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self::new("system", content)
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self::new("user", content)
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new("assistant", content)
    }
}

/// Tolère les réponses entourées de ```json … ``` ou de texte parasite.
pub fn extract_json(raw: &str) -> String {
    let mut raw = raw.trim().to_string();
    if let Some(caps) = FENCED_JSON_RE.captures(&raw) {
        raw = caps[1].trim().to_string();
    }
    match (raw.find('{'), raw.rfind('}')) {
        (Some(start), Some(end)) if end > start => raw[start..=end].to_string(),
        _ => raw,
    }
}

/// Les modèles OpenAI refusent `json_object` si les messages d'entrée ne contiennent pas
/// « json » en minuscules. Vibe passe par l'API Responses, où le message système devient
/// `instructions` et ne compte pas : la consigne doit donc figurer dans un message utilisateur.
pub fn ensure_json_hint(messages: &[Msg]) -> Cow<'_, [Msg]> {
    if messages.iter().any(|m| m.role != "system" && m.content.contains("json")) {
        return Cow::Borrowed(messages);
    }
    let mut result = messages.to_vec();
    match result.iter().rposition(|m| m.role == "user") {
        Some(i) => result[i].content = format!("{}\n\n{JSON_HINT}", result[i].content),
        None => result.push(Msg::user(JSON_HINT)),
    }
    Cow::Owned(result)
}

/// Retire la clôture ```markdown … ``` qui entoure parfois toute une réponse.
pub fn strip_fences(text: &str) -> String {
    match FENCED_MD_RE.captures(text) {
        Some(caps) => caps[1].to_string(),
        None => text.trim().to_string(),
    }
}

pub trait Llm: Send + Sync {
    fn chat(&self, messages: &[Msg], model: &str, json_mode: bool) -> Result<String>;

    /// Réponse par morceaux ; par défaut, un seul morceau.
    fn chat_stream(&self, messages: &[Msg], model: &str, json_mode: bool, on_chunk: &mut dyn FnMut(&str)) -> Result<()> {
        let text = self.chat(messages, model, json_mode)?;
        on_chunk(&text);
        Ok(())
    }
}

/// Réponse json du modèle désérialisée, avec un nouvel essai (et la demande de correction)
/// si elle ne respecte pas le schéma attendu.
pub fn chat_json<T: DeserializeOwned>(llm: &dyn Llm, messages: &[Msg], model: &str) -> Result<T> {
    const RETRIES: usize = 1;
    let mut msgs = messages.to_vec();
    for attempt in 0..=RETRIES {
        let raw = llm.chat(&msgs, model, true)?;
        match serde_json::from_str::<T>(&extract_json(&raw)) {
            Ok(value) => return Ok(value),
            Err(exc) if attempt == RETRIES => {
                return Err(Error::Llm(format!("Réponse JSON invalide du modèle : {exc}")));
            }
            Err(exc) => {
                msgs.push(Msg::assistant(raw));
                msgs.push(Msg::user(format!(
                    "Ta réponse ne respecte pas le schéma attendu ({exc}). Réponds uniquement avec le JSON corrigé."
                )));
            }
        }
    }
    unreachable!()
}

// --- client HTTP ---------------------------------------------------------------

/// Client HTTP : certificats du système (Windows) ou d'un fichier PEM, proxy de l'environnement.
pub fn http_agent(ca_bundle: Option<&str>, timeout: Duration) -> Result<ureq::Agent> {
    use ureq::tls::{parse_pem, PemItem, RootCerts, TlsConfig};
    let roots = match ca_bundle {
        Some(path) => {
            let pem = std::fs::read(path)
                .map_err(|e| Error::Internal(format!("Certificat illisible ({path}) : {e}")))?;
            let certs: Vec<_> = parse_pem(&pem)
                .filter_map(|item| match item {
                    Ok(PemItem::Certificate(cert)) => Some(cert),
                    _ => None,
                })
                .collect();
            RootCerts::new_with_certs(&certs)
        }
        None => RootCerts::PlatformVerifier,
    };
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .tls_config(TlsConfig::builder().root_certs(roots).build())
        .build();
    Ok(ureq::Agent::new_with_config(config))
}

/// POST json vers une API compatible OpenAI ; renvoie la réponse si le statut est 2xx.
pub fn post_json(
    agent: &ureq::Agent,
    url: &str,
    api_key: &str,
    body: &Value,
    label: &str,
) -> Result<ureq::http::Response<ureq::Body>> {
    let response = agent
        .post(url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .map_err(|e| Error::Llm(format!("Erreur de l'API {label} : {e}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.into_body().read_to_string().unwrap_or_default();
        let excerpt: String = text.chars().take(500).collect();
        return Err(Error::Llm(format!("Erreur de l'API {label} : HTTP {status} {excerpt}")));
    }
    Ok(response)
}

pub struct OpenAiCompatibleLlm {
    agent: ureq::Agent,
    base_url: String,
    api_key: String,
}

impl OpenAiCompatibleLlm {
    pub fn new(base_url: &str, api_key: &str, ca_bundle: Option<&str>) -> Result<Self> {
        Ok(OpenAiCompatibleLlm {
            agent: http_agent(ca_bundle, Duration::from_secs(180))?,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        })
    }

    /// Corps de la requête `chat/completions`.
    pub fn request_body(messages: &[Msg], model: &str, json_mode: bool, stream: bool) -> Value {
        let messages = if json_mode { ensure_json_hint(messages) } else { Cow::Borrowed(messages) };
        let mut body = json!({"model": model, "messages": messages, "temperature": TEMPERATURE});
        if json_mode {
            body["response_format"] = json!({"type": "json_object"});
        }
        if stream {
            body["stream"] = json!(true);
        }
        body
    }

    fn post(&self, body: &Value) -> Result<ureq::http::Response<ureq::Body>> {
        post_json(&self.agent, &format!("{}/chat/completions", self.base_url), &self.api_key, body, "LLM")
    }
}

impl Llm for OpenAiCompatibleLlm {
    fn chat(&self, messages: &[Msg], model: &str, json_mode: bool) -> Result<String> {
        let response = self.post(&Self::request_body(messages, model, json_mode, false))?;
        let text = response
            .into_body()
            .read_to_string()
            .map_err(|e| Error::Llm(format!("Erreur de l'API LLM : {e}")))?;
        let data: Value =
            serde_json::from_str(&text).map_err(|e| Error::Llm(format!("Erreur de l'API LLM : réponse illisible ({e})")))?;
        Ok(data["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_string())
    }

    fn chat_stream(&self, messages: &[Msg], model: &str, json_mode: bool, on_chunk: &mut dyn FnMut(&str)) -> Result<()> {
        let response = self.post(&Self::request_body(messages, model, json_mode, true))?;
        let reader = BufReader::new(response.into_body().into_reader());
        for line in reader.lines() {
            let line = line.map_err(|e| Error::Llm(format!("Erreur de l'API LLM : {e}")))?;
            let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
            if data == "[DONE]" {
                break;
            }
            let Ok(event) = serde_json::from_str::<Value>(data) else { continue };
            if let Some(text) = event["choices"][0]["delta"]["content"].as_str()
                && !text.is_empty() {
                    on_chunk(text);
                }
        }
        Ok(())
    }
}

/// Sans configuration : toute fonction IA renvoie une erreur explicite.
pub struct DisabledLlm;

impl Llm for DisabledLlm {
    fn chat(&self, _: &[Msg], _: &str, _: bool) -> Result<String> {
        Err(Error::LlmNotConfigured)
    }
}

pub fn build_llm(settings: &Settings) -> Result<Arc<dyn Llm>> {
    match (&settings.llm_base_url, &settings.llm_api_key) {
        (Some(url), Some(key)) => Ok(Arc::new(OpenAiCompatibleLlm::new(url, key, settings.llm_ca_bundle.as_deref())?)),
        _ => Ok(Arc::new(DisabledLlm)),
    }
}

// --- implémentation factice ----------------------------------------------------

/// Appel mémorisé par [`FakeLlm`].
#[derive(Debug, Clone)]
pub struct LlmCall {
    pub messages: Vec<Msg>,
    pub model: String,
    pub json_mode: bool,
}

type Responder = Box<dyn Fn(&[Msg], &str) -> String + Send + Sync>;

enum Responses {
    Queue(VecDeque<String>),
    Func(Responder),
}

/// Renvoie des réponses prédéfinies (liste ou fonction) et mémorise les appels.
/// Avec `chunk`, `chat_stream` découpe chaque réponse en morceaux de cette taille.
pub struct FakeLlm {
    responses: Mutex<Responses>,
    chunk: usize,
    pub calls: Mutex<Vec<LlmCall>>,
}

impl FakeLlm {
    pub fn new<S: Into<String>>(responses: impl IntoIterator<Item = S>) -> Arc<Self> {
        Self::chunked(responses, 0)
    }

    pub fn chunked<S: Into<String>>(responses: impl IntoIterator<Item = S>, chunk: usize) -> Arc<Self> {
        Arc::new(FakeLlm {
            responses: Mutex::new(Responses::Queue(responses.into_iter().map(Into::into).collect())),
            chunk,
            calls: Mutex::new(Vec::new()),
        })
    }

    pub fn with_fn(f: impl Fn(&[Msg], &str) -> String + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(FakeLlm { responses: Mutex::new(Responses::Func(Box::new(f))), chunk: 0, calls: Mutex::new(Vec::new()) })
    }

    pub fn calls(&self) -> Vec<LlmCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl Llm for FakeLlm {
    fn chat(&self, messages: &[Msg], model: &str, json_mode: bool) -> Result<String> {
        self.calls.lock().unwrap().push(LlmCall { messages: messages.to_vec(), model: model.into(), json_mode });
        match &mut *self.responses.lock().unwrap() {
            Responses::Queue(queue) => queue.pop_front().ok_or_else(|| Error::Llm("FakeLlm : plus de réponse prévue".into())),
            Responses::Func(f) => Ok(f(messages, model)),
        }
    }

    fn chat_stream(&self, messages: &[Msg], model: &str, json_mode: bool, on_chunk: &mut dyn FnMut(&str)) -> Result<()> {
        let text = self.chat(messages, model, json_mode)?;
        let chars: Vec<char> = text.chars().collect();
        let size = if self.chunk > 0 { self.chunk } else { chars.len().max(1) };
        for piece in chars.chunks(size) {
            on_chunk(&piece.iter().collect::<String>());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_json_hint_targets_user_message() {
        // le message système ne compte pas (il devient `instructions` dans l'API Responses)
        let messages = vec![Msg::system("un objet json"), Msg::user("Résumé à traiter")];
        let result = ensure_json_hint(&messages);
        assert_eq!(result[0], messages[0]);
        assert!(result[1].role == "user" && result[1].content.contains("json"));
        assert_eq!(messages[1].content, "Résumé à traiter");
        let already = vec![Msg::user("un objet json")];
        assert!(matches!(ensure_json_hint(&already), Cow::Borrowed(_)));
        assert_eq!(ensure_json_hint(&[Msg::system("x")]).last().unwrap().role, "user");
    }

    #[test]
    fn extract_and_strip() {
        assert_eq!(extract_json("Voici :\n```json\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(extract_json("bla {\"a\": {\"b\": 2}} fin"), "{\"a\": {\"b\": 2}}");
        assert_eq!(strip_fences("```markdown\n- point 1\n```"), "- point 1");
        assert_eq!(strip_fences("  texte  "), "texte");
    }
}
