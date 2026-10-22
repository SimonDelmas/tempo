//! Embeddings locaux : modèle multilingual-e5-small quantifié (ONNX, 384 dimensions, français et
//! anglais) exécuté par ONNX Runtime (`onnxruntime.dll`, chargée à l'exécution).
//!
//! Rien n'est chargé au démarrage : le modèle l'est au premier texte à vectoriser, puis libéré
//! après [`IDLE`] sans usage. L'inférence se limite à [`THREADS`] cœurs pour rester discrète.
//! Les fichiers sont livrés avec l'installeur (`resources/`), jamais téléchargés à l'exécution.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ort::session::Session;
use ort::value::{DynTensor, Tensor};
use tokenizers::{Tokenizer, TruncationParams};

use crate::convert::find_tool;
use crate::error::{Error, Result};
use crate::semantic::{EmbedKind, Embedder};

pub const MODEL_NAME: &str = "multilingual-e5-small";
const MODEL_FILE: &str = "model_int8.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";
const MAX_TOKENS: usize = 512;
const BATCH: usize = 16;
/// cœurs utilisés par l'inférence
pub const THREADS: usize = 2;
/// durée sans usage après laquelle le modèle est libéré
pub const IDLE: Duration = Duration::from_secs(300);

/// Fichiers du modèle local : dossier du modèle et bibliothèque ONNX Runtime.
#[derive(Debug, Clone)]
pub struct LocalModel {
    pub dir: PathBuf,
    pub runtime: PathBuf,
}

impl LocalModel {
    /// Cherche le modèle (`TEMPO_EMBED_LOCAL_DIR`, sinon `resources/models/multilingual-e5-small`)
    /// et `onnxruntime.dll` (`TEMPO_ONNXRUNTIME`, sinon à côté de l'exécutable ou dans le dépôt).
    pub fn discover(dir: Option<&Path>, runtime: Option<&Path>) -> Option<LocalModel> {
        let model = match dir {
            Some(dir) => Some(dir.join(MODEL_FILE)).filter(|p| p.is_file()),
            None => find_tool(None, &format!("models/{MODEL_NAME}/{MODEL_FILE}")),
        }?;
        let dir = model.parent()?.to_path_buf();
        if !dir.join(TOKENIZER_FILE).is_file() {
            return None;
        }
        let runtime = find_tool(runtime, "onnxruntime.dll").filter(|p| p.is_file())?;
        Some(LocalModel { dir, runtime })
    }
}

fn local_err(message: impl std::fmt::Display) -> Error {
    Error::Llm(format!("Embeddings locaux : {message}"))
}

/// ONNX Runtime ne s'initialise qu'une fois par processus.
fn init_runtime(path: &Path) -> Result<()> {
    static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        ort::init_from(path)
            .map_err(|e| format!("chargement d'ONNX Runtime impossible ({}) : {e}", path.display()))?
            .with_name("tempo")
            .commit();
        Ok(())
    })
    .clone()
    .map_err(local_err)
}

struct Loaded {
    session: Session,
    tokenizer: Tokenizer,
    inputs: Vec<String>,
    pad: u32,
}

/// Modèle chargé à la demande, libéré après inactivité.
pub struct LocalEmbedder {
    model: LocalModel,
    state: Mutex<Option<Loaded>>,
    last_used: Mutex<Instant>,
}

impl LocalEmbedder {
    pub fn new(model: LocalModel) -> Self {
        LocalEmbedder { model, state: Mutex::new(None), last_used: Mutex::new(Instant::now()) }
    }

    fn load(&self) -> Result<Loaded> {
        init_runtime(&self.model.runtime)?;
        let mut tokenizer = Tokenizer::from_file(self.model.dir.join(TOKENIZER_FILE)).map_err(local_err)?;
        tokenizer
            .with_truncation(Some(TruncationParams { max_length: MAX_TOKENS, ..Default::default() }))
            .map_err(local_err)?;
        tokenizer.with_padding(None);
        let builder = Session::builder().map_err(local_err)?;
        let mut builder = builder.with_intra_threads(THREADS).map_err(local_err)?;
        let session = builder.commit_from_file(self.model.dir.join(MODEL_FILE)).map_err(local_err)?;
        let inputs = session.inputs().iter().map(|i| i.name().to_string()).collect();
        let pad = tokenizer.token_to_id("<pad>").unwrap_or(1);
        Ok(Loaded { session, tokenizer, inputs, pad })
    }

    /// Modèle actuellement en mémoire.
    pub fn is_loaded(&self) -> bool {
        self.state.lock().unwrap().is_some()
    }

    /// Libère le modèle s'il n'a pas servi depuis `idle`.
    pub fn unload_if_idle(&self, idle: Duration) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.is_some() && self.last_used.lock().unwrap().elapsed() >= idle {
            *state = None;
            return true;
        }
        false
    }

    /// Vecteurs normalisés (moyenne des jetons pondérée par le masque), dans l'ordre des textes.
    pub fn embed(&self, texts: &[String], kind: EmbedKind) -> Result<Vec<Vec<f32>>> {
        let mut state = self.state.lock().unwrap();
        if state.is_none() {
            *state = Some(self.load()?);
        }
        let loaded = state.as_mut().unwrap();
        let prefix = match kind {
            EmbedKind::Query => "query: ",
            EmbedKind::Passage => "passage: ",
        };
        let mut vectors = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            let prefixed: Vec<String> = batch.iter().map(|t| format!("{prefix}{t}")).collect();
            let encodings = loaded.tokenizer.encode_batch(prefixed, true).map_err(local_err)?;
            let len = encodings.iter().map(|e| e.get_ids().len()).max().unwrap_or(1).max(1);
            let rows = encodings.len();
            let mut ids = vec![loaded.pad as i64; rows * len];
            let mut mask = vec![0i64; rows * len];
            for (r, encoding) in encodings.iter().enumerate() {
                for (c, (&id, &m)) in encoding.get_ids().iter().zip(encoding.get_attention_mask()).enumerate() {
                    ids[r * len + c] = id as i64;
                    mask[r * len + c] = m as i64;
                }
            }
            let shape = vec![rows as i64, len as i64];
            let mut inputs: HashMap<String, DynTensor> = HashMap::new();
            for name in &loaded.inputs {
                let data = match name.as_str() {
                    "input_ids" => ids.clone(),
                    "attention_mask" => mask.clone(),
                    _ => vec![0i64; rows * len], // token_type_ids
                };
                inputs.insert(name.clone(), Tensor::from_array((shape.clone(), data)).map_err(local_err)?.upcast());
            }
            let outputs = loaded.session.run(inputs).map_err(local_err)?;
            let (out_shape, hidden) = outputs[0].try_extract_tensor::<f32>().map_err(local_err)?;
            let dim = *out_shape.last().unwrap_or(&0) as usize;
            if dim == 0 || hidden.len() != rows * len * dim {
                return Err(local_err("sortie du modèle inattendue"));
            }
            for r in 0..rows {
                let mut vector = vec![0f32; dim];
                let mut count = 0f32;
                for c in 0..len {
                    if mask[r * len + c] == 0 {
                        continue;
                    }
                    count += 1.0;
                    let token = &hidden[(r * len + c) * dim..(r * len + c + 1) * dim];
                    for (v, x) in vector.iter_mut().zip(token) {
                        *v += x;
                    }
                }
                let norm = vector.iter().map(|x| (x / count.max(1.0)).powi(2)).sum::<f32>().sqrt().max(1e-12);
                vectors.push(vector.iter().map(|x| x / count.max(1.0) / norm).collect());
            }
        }
        *self.last_used.lock().unwrap() = Instant::now();
        Ok(vectors)
    }
}

/// [`Embedder`] sur le modèle local ; un fil libère le modèle après [`IDLE`] sans usage.
pub fn local_embedder(model: LocalModel) -> Embedder {
    let embedder = Arc::new(LocalEmbedder::new(model));
    let weak = Arc::downgrade(&embedder);
    std::thread::Builder::new()
        .name("tempo-embeddings-idle".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(30));
            let Some(embedder) = weak.upgrade() else { return };
            embedder.unload_if_idle(IDLE);
        })
        .ok();
    Arc::new(move |texts: &[String], kind: EmbedKind| embedder.embed(texts, kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn vecteurs_du_modele_local() {
        let Some(model) = LocalModel::discover(None, None) else {
            eprintln!("modèle local absent : test sauté (scripts/telecharger-outils.ps1)");
            return;
        };
        let embedder = LocalEmbedder::new(model);
        assert!(!embedder.is_loaded());
        let passages: Vec<String> = [
            "Les unités PCE et ST deviennent PC dans SAP.",
            "Planning de la recette : lot 1 en mars, lot 2 en juin.",
            "The units of measure are converted before the SAP migration.",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let vectors = embedder.embed(&passages, EmbedKind::Passage).unwrap();
        assert!(embedder.is_loaded());
        assert_eq!(vectors.len(), 3);
        assert_eq!(vectors[0].len(), 384);
        assert!((cosine(&vectors[0], &vectors[0]) - 1.0).abs() < 1e-3);
        let query = embedder.embed(&["conversion des unités de mesure".into()], EmbedKind::Query).unwrap();
        // une paraphrase, même en anglais, est plus proche que le planning
        assert!(cosine(&query[0], &vectors[0]) > cosine(&query[0], &vectors[1]));
        assert!(cosine(&query[0], &vectors[2]) > cosine(&query[0], &vectors[1]));
        assert!(!embedder.unload_if_idle(Duration::from_secs(3600)));
        assert!(embedder.unload_if_idle(Duration::ZERO) && !embedder.is_loaded());
    }
}
