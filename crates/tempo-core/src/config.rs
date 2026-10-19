//! Réglages de Tempo, lus dans les variables d'environnement et les fichiers `.env`.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

/// Racine du dépôt à la compilation (sert en développement : `.env`, `vault-exemple/`,
/// `frontend/dist`). Absente sur le poste où l'application est installée.
pub fn project_root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    root.join("vault-exemple").is_dir().then(|| dunce::canonicalize(&root).unwrap_or(root))
}

/// Dossier de configuration de l'utilisateur (`%APPDATA%\Tempo` sous Windows).
pub fn user_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("Tempo"))
}

/// Raccourcis par défaut pour revenir à la vue précédente / aller à la vue suivante.
pub const DEFAULT_SHORTCUT_BACK: &str = "Alt+ArrowLeft";
pub const DEFAULT_SHORTCUT_FORWARD: &str = "Alt+ArrowRight";

#[derive(Debug, Clone)]
pub struct Settings {
    pub vault: PathBuf,
    pub llm_base_url: Option<String>,
    pub llm_api_key: Option<String>,
    pub llm_ca_bundle: Option<String>,
    pub model_fast: String,
    pub model_smart: String,
    /// embeddings (recherche sémantique) : API compatible OpenAI, désactivés sans modèle
    pub embed_model: Option<String>,
    pub embed_base_url: Option<String>,
    pub embed_api_key: Option<String>,
    pub host: String,
    pub port: u16,
    /// raccourcis clavier de navigation (vue précédente / suivante), ex. `Alt+ArrowLeft`
    pub shortcut_back: String,
    pub shortcut_forward: String,
    /// surveillance du dossier et vectorisation en arrière-plan (désactivées dans les tests)
    pub watch: bool,
}

impl Settings {
    /// Réglages minimaux pour un vault donné, sans IA (tests).
    pub fn for_vault(vault: impl Into<PathBuf>) -> Self {
        Settings {
            vault: vault.into(),
            llm_base_url: None,
            llm_api_key: None,
            llm_ca_bundle: None,
            model_fast: "gpt-luna".into(),
            model_smart: "claude-opus".into(),
            embed_model: None,
            embed_base_url: None,
            embed_api_key: None,
            host: "127.0.0.1".into(),
            port: 8765,
            shortcut_back: DEFAULT_SHORTCUT_BACK.into(),
            shortcut_forward: DEFAULT_SHORTCUT_FORWARD.into(),
            watch: false,
        }
    }

    pub fn llm_enabled(&self) -> bool {
        self.llm_base_url.is_some() && self.llm_api_key.is_some()
    }

    pub fn embed_enabled(&self) -> bool {
        self.embed_model.is_some() && self.embed_base_url.is_some() && self.embed_api_key.is_some()
    }
}

/// Variables lues dans les fichiers `.env` (le processus n'est pas modifié).
static ENV_FILE_VARS: LazyLock<RwLock<HashMap<String, String>>> = LazyLock::new(Default::default);

/// Variable de l'environnement, sinon des fichiers `.env` chargés.
pub fn var(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .or_else(|| ENV_FILE_VARS.read().unwrap().get(name).cloned())
        .filter(|v| !v.trim().is_empty())
}

/// Lecture tolérante d'un fichier `.env` (comme python-dotenv) : une ligne illisible est
/// ignorée sans empêcher la lecture des suivantes.
pub fn parse_env_file(text: &str) -> Vec<(String, String)> {
    let mut vars = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
            continue;
        }
        let value = value.trim();
        let value = match value.chars().next() {
            Some(quote @ ('"' | '\'')) => match value[1..].find(quote) {
                Some(end) => {
                    let inner = &value[1..1 + end];
                    if quote == '"' { inner.replace("\\n", "\n") } else { inner.to_string() }
                }
                None => value[1..].to_string(),
            },
            // commentaire en fin de ligne : ` # …`
            _ => value.split(" #").next().unwrap_or("").trim().to_string(),
        };
        vars.push((key.to_string(), value));
    }
    vars
}

/// Fichiers `.env` candidats, du plus prioritaire au moins prioritaire. Une variable déjà
/// définie n'est jamais écrasée : l'environnement du processus l'emporte toujours.
pub fn env_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(path) = var("TEMPO_ENV_FILE") {
        files.push(PathBuf::from(path));
    }
    if let Ok(cwd) = env::current_dir() {
        files.push(cwd.join(".env"));
    }
    if let Some(root) = project_root() {
        files.push(root.join(".env"));
    }
    if let Some(dir) = user_config_dir() {
        files.push(dir.join(".env"));
    }
    files
}

pub fn load_settings() -> Settings {
    let files = env_files();
    {
        let mut loaded = ENV_FILE_VARS.write().unwrap();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            for (key, value) in parse_env_file(&text) {
                // le premier fichier qui définit une variable l'emporte
                loaded.entry(key).or_insert(value);
            }
        }
    }
    let vault = var("TEMPO_VAULT").map(PathBuf::from).unwrap_or_else(|| {
        project_root()
            .map(|r| r.join("vault-exemple"))
            .or_else(|| dirs::document_dir().map(|d| d.join("Tempo")))
            .unwrap_or_else(|| PathBuf::from("Tempo"))
    });
    let vault = expand_home(&vault);
    std::fs::create_dir_all(&vault).ok();
    let vault = dunce::canonicalize(&vault).unwrap_or(vault);
    let base_url = var("VIBE_BASE_URL");
    let api_key = var("VIBE_API_KEY");
    Settings {
        vault,
        llm_ca_bundle: var("VIBE_CA_BUNDLE"),
        model_fast: var("TEMPO_MODEL_FAST").unwrap_or_else(|| "gpt-luna".into()),
        model_smart: var("TEMPO_MODEL_SMART").unwrap_or_else(|| "claude-opus".into()),
        embed_model: var("TEMPO_EMBED_MODEL"),
        embed_base_url: var("TEMPO_EMBED_BASE_URL").or_else(|| base_url.clone()),
        embed_api_key: var("TEMPO_EMBED_API_KEY").or_else(|| api_key.clone()),
        llm_base_url: base_url,
        llm_api_key: api_key,
        host: var("TEMPO_HOST").unwrap_or_else(|| "127.0.0.1".into()),
        port: var("TEMPO_PORT").and_then(|p| p.parse().ok()).unwrap_or(8765),
        shortcut_back: var("TEMPO_SHORTCUT_BACK").unwrap_or_else(|| DEFAULT_SHORTCUT_BACK.into()),
        shortcut_forward: var("TEMPO_SHORTCUT_FORWARD").unwrap_or_else(|| DEFAULT_SHORTCUT_FORWARD.into()),
        watch: true,
    }
}

fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_settings_completes() {
        // les fichiers .env sont lus sans interblocage, quels qu'ils soient
        let settings = load_settings();
        assert!(!settings.model_fast.is_empty() && settings.vault.is_dir());
    }

    #[test]
    fn env_file_is_parsed_leniently() {
        let text = "# commentaire\nA=1\nligne invalide\nexport B = \"deux mots\"  \nC='x#y'\nD=valeur # note\nE=\n";
        let vars: HashMap<_, _> = parse_env_file(text).into_iter().collect();
        assert_eq!(vars["A"], "1");
        assert_eq!(vars["B"], "deux mots");
        assert_eq!(vars["C"], "x#y");
        assert_eq!(vars["D"], "valeur");
        assert_eq!(vars["E"], "");
        assert_eq!(vars.len(), 5);
    }
}
