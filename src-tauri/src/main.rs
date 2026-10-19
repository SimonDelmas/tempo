//! Application de bureau Tempo.
//!
//! Au démarrage, l'API (`tempo-core`) est servie sur un port local (48765 par défaut), protégée par un
//! jeton aléatoire (cookie HttpOnly posé à l'ouverture de la fenêtre) ; elle sert aussi
//! l'interface, embarquée dans l'exécutable par Tauri. La fenêtre charge cette adresse : le
//! frontend garde ainsi ses appels relatifs `/api/*` et la diffusion SSE du chat.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use tauri::{Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;
use tempo_core::{llm, load_settings, router, ApiOptions, AssetFn, Frontend, Services};

const WINDOW: &str = "main";
/// Port local par défaut. Il reste le même d'un lancement à l'autre car le stockage local de
/// la fenêtre (conversations du chat, préférences) dépend de l'origine, port compris.
const DEFAULT_PORT: u16 = 48765;

/// Écoute sur `127.0.0.1:<TEMPO_DESKTOP_PORT ou 48765>`, ou sur un port libre s'il est pris.
fn bind_local() -> std::io::Result<std::net::TcpListener> {
    let port = tempo_core::config::var("TEMPO_DESKTOP_PORT").and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT);
    std::net::TcpListener::bind(("127.0.0.1", port)).or_else(|_| std::net::TcpListener::bind("127.0.0.1:0"))
}

fn start(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let settings = load_settings();
    let llm = llm::build_llm(&settings)?;
    let services = Services::new(settings, llm, None)?;
    services.start()?;
    app.manage(services.clone());

    let resolver = app.handle().clone();
    let assets: AssetFn = Arc::new(move |path: &str| {
        resolver.asset_resolver().get(path.to_string()).map(|asset| (asset.bytes, asset.mime_type))
    });
    let token = uuid::Uuid::new_v4().simple().to_string();
    let app_router = router(services, ApiOptions { frontend: Frontend::Embedded(assets), token: Some(token.clone()) });

    let listener = bind_local()?;
    listener.set_nonblocking(true)?;
    let origin = format!("http://{}", listener.local_addr()?);
    tauri::async_runtime::spawn(async move {
        match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => {
                if let Err(err) = axum::serve(listener, app_router).await {
                    eprintln!("Serveur Tempo arrêté : {err}");
                }
            }
            Err(err) => eprintln!("Serveur Tempo indisponible : {err}"),
        }
    });

    let opener = app.handle().clone();
    let local = origin.clone();
    WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::External(Url::parse(&format!("{origin}/?token={token}"))?))
        .title("Tempo")
        .inner_size(1400.0, 900.0)
        .min_inner_size(900.0, 600.0)
        // sous Windows, le dépôt de fichiers natif de Tauri intercepte le glisser-déposer HTML5
        // (board, backlog) : on le désactive, l'application n'en a pas l'usage
        .disable_drag_drop_handler()
        // les liens externes s'ouvrent dans le navigateur, pas dans la fenêtre de l'application
        .on_navigation(move |url| {
            let internal = url.origin().ascii_serialization() == local;
            if !internal && matches!(url.scheme(), "http" | "https" | "mailto") {
                let _ = opener.opener().open_url(url.as_str(), None::<&str>);
            }
            internal
        })
        .build()?;
    Ok(())
}

fn main() {
    let app = tauri::Builder::default()
        // une seule instance : relancer l'application ramène la fenêtre existante
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window(WINDOW) {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            if let Err(err) = start(app) {
                app.dialog()
                    .message(format!("Tempo n'a pas pu démarrer :\n{err}"))
                    .title("Tempo")
                    .kind(MessageDialogKind::Error)
                    .blocking_show();
                return Err(err);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("impossible de lancer Tempo");
    app.run(|handle, event| {
        if let RunEvent::Exit = event
            && let Some(services) = handle.try_state::<Arc<Services>>() {
                services.stop();
            }
    });
}
