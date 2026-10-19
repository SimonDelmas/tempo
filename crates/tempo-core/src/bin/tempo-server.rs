//! Serveur autonome : API et interface compilée sur http://127.0.0.1:8765 (développement,
//! ou utilisation dans un navigateur sans l'application de bureau).

use std::path::PathBuf;

use tempo_core::{config, llm, load_settings, router, ApiOptions, Frontend, Services};

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("Erreur : {err}");
        std::process::exit(1);
    }
}

async fn run() -> tempo_core::Result<()> {
    let settings = load_settings();
    let address = format!("{}:{}", settings.host, settings.port);
    println!("Tempo : http://{address}  (vault : {})", settings.vault.display());
    if !settings.llm_enabled() {
        println!("Attention : API LLM non configurée (.env), les fonctions IA sont désactivées.");
    }
    let llm = llm::build_llm(&settings)?;
    let services = Services::new(settings, llm, None)?;
    let count = tokio::task::spawn_blocking({
        let services = services.clone();
        move || services.start()
    })
    .await
    .map_err(|e| tempo_core::Error::Internal(e.to_string()))??;
    println!("{count} notes indexées.");

    let dist = std::env::var("TEMPO_FRONTEND_DIST")
        .map(PathBuf::from)
        .ok()
        .or_else(|| config::project_root().map(|r| r.join("frontend").join("dist")))
        .filter(|d| d.is_dir());
    let frontend = match dist {
        Some(dir) => Frontend::Dir(dir),
        None => {
            println!("Interface non compilée (frontend/dist) : seule l'API est servie.");
            Frontend::None
        }
    };
    let app = router(services.clone(), ApiOptions { frontend, token: None });
    let listener = tokio::net::TcpListener::bind(&address).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    services.stop();
    Ok(())
}
