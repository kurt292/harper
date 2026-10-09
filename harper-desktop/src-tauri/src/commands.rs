//! This module centralizes all the Rust commands to be made available to the JavaScript
//! environment. Use [`application_message_handler`] to load them into the Tauri runtime.

use crate::config::Config;
use crate::desktop_updater::{DesktopUpdater, UpdateResult};
use crate::highlighter_service::HighlighterService;
use crate::os_broker::{AccessibilityPermissionStatus, AppSearchResult, OsBroker};
use crate::{IntegrationView, PlatformBroker};
use base64::{Engine as _, engine::general_purpose};
use harper_core::{
    Dialect, DictWordMetadata, IgnoredLints,
    linting::FlatConfig,
    spell::{Dictionary, MutableDictionary},
};
use std::sync::{Arc, Mutex as StdMutex};
use tauri::ipc::Invoke;
use tauri::{Manager, Runtime, State};
use tokio::sync::Mutex;

pub fn application_message_handler<R: Runtime>() -> impl Fn(Invoke<R>) -> bool {
    tauri::generate_handler![
        get_lint_config,
        get_dialect,
        get_debounce_ms,
        set_debounce_ms,
        get_auto_update,
        set_auto_update,
        get_current_version,
        get_latest_version,
        update_to_latest,
        get_onboarding_completed,
        set_onboarding_completed,
        set_dialect,
        set_lint_config,
        get_dictionary,
        set_dictionary,
        ignore_lint,
        add_to_dictionary,
        get_integrations,
        get_auto_enable_new_apps,
        set_auto_enable_new_apps,
        add_integration,
        remove_integration,
        set_integration_enabled,
        get_application_icon_data_url,
        get_accessibility_permission_status,
        request_accessibility_permission,
        start_highlighter_service,
        stop_highlighter_service,
        launch_app,
        search_apps,
        get_style_guides,
        set_style_guide_active,
        save_style_guide,
        delete_style_guide,
        get_style_model_status,
        style_check,
        get_deny_list,
        set_deny_list,
    ]
}

#[tauri::command]
async fn get_lint_config(config: State<'_, Arc<Mutex<Config>>>) -> Result<FlatConfig, String> {
    let mut lint_config = config.lock().await.lint_config.clone();
    lint_config.fill_with_curated();

    Ok(lint_config)
}

#[tauri::command]
async fn get_dialect(config: State<'_, Arc<Mutex<Config>>>) -> Result<Dialect, String> {
    Ok(config.lock().await.dialect)
}

#[tauri::command]
async fn get_debounce_ms(config: State<'_, Arc<Mutex<Config>>>) -> Result<u64, String> {
    Ok(config.lock().await.debounce_ms)
}

#[tauri::command]
async fn set_debounce_ms(
    debounce_ms: u64,
    config: State<'_, Arc<Mutex<Config>>>,
) -> Result<(), String> {
    let mut config = config.lock().await;
    config.debounce_ms = debounce_ms;
    config
        .save_to_system()
        .await
        .map_err(|error| error.to_string())?;

    Ok(())
}

#[tauri::command]
async fn get_auto_update(config: State<'_, Arc<Mutex<Config>>>) -> Result<bool, String> {
    Ok(config.lock().await.auto_update)
}

#[tauri::command]
async fn set_auto_update(
    auto_update: bool,
    config: State<'_, Arc<Mutex<Config>>>,
) -> Result<(), String> {
    let mut config = config.lock().await;
    config.auto_update = auto_update;
    config
        .save_to_system()
        .await
        .map_err(|error| error.to_string())?;

    Ok(())
}

#[tauri::command]
fn get_current_version<R: Runtime>(app: tauri::AppHandle<R>) -> String {
    DesktopUpdater::current_version(&app)
}

#[tauri::command]
async fn get_latest_version() -> Result<String, String> {
    DesktopUpdater::latest_version().await
}

#[tauri::command]
async fn update_to_latest<R: Runtime>(
    app: tauri::AppHandle<R>,
    updater: State<'_, DesktopUpdater>,
) -> Result<UpdateResult, String> {
    updater
        .update_to_latest(&app, false)
        .await
        .ok_or_else(|| "Manual update check was unexpectedly skipped.".into())
}

#[tauri::command]
async fn get_onboarding_completed(config: State<'_, Arc<Mutex<Config>>>) -> Result<bool, String> {
    Ok(config.lock().await.onboarding_completed)
}

#[tauri::command]
async fn set_onboarding_completed(
    onboarding_completed: bool,
    config: State<'_, Arc<Mutex<Config>>>,
) -> Result<(), String> {
    let mut config = config.lock().await;
    let previous_value = config.onboarding_completed;
    config.onboarding_completed = onboarding_completed;

    if let Err(error) = config.save_to_system().await {
        config.onboarding_completed = previous_value;
        return Err(error.to_string());
    }

    Ok(())
}

#[tauri::command]
async fn set_dialect(
    dialect: Dialect,
    config: State<'_, Arc<Mutex<Config>>>,
) -> Result<(), String> {
    let mut config = config.lock().await;
    config.dialect = dialect;
    config
        .save_to_system()
        .await
        .map_err(|error| error.to_string())?;

    Ok(())
}

#[tauri::command]
async fn set_lint_config(
    lint_config: FlatConfig,
    config: State<'_, Arc<Mutex<Config>>>,
) -> Result<(), String> {
    let mut lint_config = lint_config;
    lint_config.fill_with_curated();

    let mut config = config.lock().await;
    config.lint_config = lint_config;
    config
        .save_to_system()
        .await
        .map_err(|error| error.to_string())?;

    Ok(())
}

#[tauri::command]
async fn get_dictionary(config: State<'_, Arc<Mutex<Config>>>) -> Result<Vec<String>, String> {
    let mut words = config
        .lock()
        .await
        .mutable_dictionary
        .words_iter()
        .map(|word| word.iter().collect::<String>())
        .collect::<Vec<_>>();
    words.sort();

    Ok(words)
}

#[tauri::command]
async fn set_dictionary(
    words: Vec<String>,
    config: State<'_, Arc<Mutex<Config>>>,
) -> Result<(), String> {
    let mut dictionary = MutableDictionary::new();
    dictionary.extend_words(words.into_iter().map(|word| {
        (
            word.chars().collect::<Vec<_>>(),
            DictWordMetadata::default(),
        )
    }));

    let mut config = config.lock().await;
    config.mutable_dictionary = dictionary;
    config
        .save_to_system()
        .await
        .map_err(|error| error.to_string())?;

    Ok(())
}

#[tauri::command]
async fn ignore_lint(
    ignored_lints: String,
    config: State<'_, Arc<Mutex<Config>>>,
) -> Result<(), String> {
    let ignored_lints =
        serde_json::from_str::<IgnoredLints>(&ignored_lints).map_err(|error| error.to_string())?;

    let mut config = config.lock().await;
    config.ignored_lints.append(ignored_lints);
    config
        .save_to_system()
        .await
        .map_err(|error| error.to_string())?;

    Ok(())
}

#[tauri::command]
async fn add_to_dictionary(
    word: String,
    config: State<'_, Arc<Mutex<Config>>>,
) -> Result<(), String> {
    let mut config = config.lock().await;
    config
        .mutable_dictionary
        .append_word_str(&word, DictWordMetadata::default());
    config
        .save_to_system()
        .await
        .map_err(|error| error.to_string())?;

    Ok(())
}

#[tauri::command]
async fn get_integrations(
    config: State<'_, Arc<Mutex<Config>>>,
    broker: State<'_, StdMutex<PlatformBroker>>,
) -> Result<Vec<IntegrationView>, String> {
    let integrations = config.lock().await.integrations.clone();
    let broker = broker
        .lock()
        .map_err(|error| format!("Failed to read platform broker: {error}"))?;

    Ok(integrations
        .into_iter()
        .map(|integration| IntegrationView {
            display_name: broker.integration_display_name(&integration.bundle_id),
            bundle_id: integration.bundle_id,
            enabled: integration.enabled,
        })
        .collect())
}

#[tauri::command]
async fn get_auto_enable_new_apps(config: State<'_, Arc<Mutex<Config>>>) -> Result<bool, String> {
    Ok(config.lock().await.auto_enable_new_apps)
}

#[tauri::command]
async fn set_auto_enable_new_apps(
    auto_enable_new_apps: bool,
    config: State<'_, Arc<Mutex<Config>>>,
) -> Result<(), String> {
    let mut config = config.lock().await;
    let previous = config.auto_enable_new_apps;
    config.auto_enable_new_apps = auto_enable_new_apps;
    if let Err(error) = config.save_to_system().await {
        config.auto_enable_new_apps = previous;
        return Err(error.to_string());
    }
    Ok(())
}

#[tauri::command]
async fn add_integration(
    bundle_id: String,
    config: State<'_, Arc<Mutex<Config>>>,
) -> Result<(), String> {
    let mut config = config.lock().await;
    config.add_integration(bundle_id);
    config
        .save_to_system()
        .await
        .map_err(|error| error.to_string())?;

    Ok(())
}

#[tauri::command]
async fn remove_integration(
    bundle_id: String,
    config: State<'_, Arc<Mutex<Config>>>,
) -> Result<(), String> {
    let mut config = config.lock().await;
    config.remove_integration(&bundle_id);
    config
        .save_to_system()
        .await
        .map_err(|error| error.to_string())?;

    Ok(())
}

#[tauri::command]
async fn set_integration_enabled(
    bundle_id: String,
    enabled: bool,
    config: State<'_, Arc<Mutex<Config>>>,
) -> Result<(), String> {
    let mut config = config.lock().await;
    config.set_integration_enabled(&bundle_id, enabled);
    config
        .save_to_system()
        .await
        .map_err(|error| error.to_string())?;

    Ok(())
}

#[tauri::command]
async fn get_application_icon_data_url<R: Runtime>(
    bundle_id: String,
    app_handle: tauri::AppHandle<R>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let broker = app_handle.state::<StdMutex<PlatformBroker>>();
        let icon_png = broker
            .lock()
            .map_err(|error| format!("Failed to read platform broker: {error}"))?
            .application_icon_png(&bundle_id)?;
        let encoded = general_purpose::STANDARD.encode(icon_png);

        Ok(format!("data:image/png;base64,{encoded}"))
    })
    .await
    .map_err(|error| format!("Failed to load application icon: {error}"))?
}

#[tauri::command]
fn get_accessibility_permission_status(
    broker: State<'_, StdMutex<PlatformBroker>>,
) -> AccessibilityPermissionStatus {
    match broker.lock() {
        Ok(broker) => broker.accessibility_permission_status(),
        Err(error) => {
            eprintln!("Failed to read platform broker: {error}");
            AccessibilityPermissionStatus::Unsupported
        }
    }
}

#[tauri::command]
fn request_accessibility_permission(
    broker: State<'_, StdMutex<PlatformBroker>>,
) -> AccessibilityPermissionStatus {
    match broker.lock() {
        Ok(broker) => broker.request_accessibility_permission(),
        Err(error) => {
            eprintln!("Failed to read platform broker: {error}");
            AccessibilityPermissionStatus::Unsupported
        }
    }
}

#[tauri::command]
pub(crate) async fn start_highlighter_service(
    config: State<'_, Arc<Mutex<Config>>>,
    highlighter_service: State<'_, HighlighterService>,
) -> Result<bool, String> {
    {
        let mut config = config.lock().await;
        config.highlighter_service_enabled = true;
        config
            .save_to_system()
            .await
            .map_err(|error| error.to_string())?;
    }

    highlighter_service
        .start()
        .map_err(|error| error.to_string())?;

    Ok(highlighter_service.is_running())
}

#[tauri::command]
pub(crate) async fn stop_highlighter_service(
    config: State<'_, Arc<Mutex<Config>>>,
    highlighter_service: State<'_, HighlighterService>,
) -> Result<bool, String> {
    {
        let mut config = config.lock().await;
        config.highlighter_service_enabled = false;
        config
            .save_to_system()
            .await
            .map_err(|error| error.to_string())?;
    }

    Ok(highlighter_service.stop())
}

#[tauri::command]
fn launch_app(
    bundle_id: String,
    broker: State<'_, StdMutex<PlatformBroker>>,
) -> Result<(), String> {
    broker
        .lock()
        .map_err(|error| format!("Failed to read platform broker: {error}"))?
        .launch_app_bundle(&bundle_id)
}

#[tauri::command]
fn search_apps(
    query: String,
    broker: State<'_, StdMutex<PlatformBroker>>,
) -> Result<Vec<AppSearchResult>, String> {
    broker
        .lock()
        .map_err(|error| format!("Failed to read platform broker: {error}"))?
        .search_apps(&query)
}

// ---------------------------------------------------------------------------
// Broadside style guides
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct StyleGuideView {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub priority: u32,
    pub rule_count: usize,
    pub model_rule_count: usize,
    pub path: String,
    /// The guide as pretty JSON, for the editor.
    pub json: String,
}

fn style_store() -> Result<broadside_style::GuideStore, String> {
    crate::style_guides::store().ok_or_else(|| "config directory unavailable".to_string())
}

#[tauri::command]
async fn get_style_guides() -> Result<Vec<StyleGuideView>, String> {
    let store = style_store()?;
    let loaded = store.load().map_err(|error| error.to_string())?;
    let mut views = Vec::with_capacity(loaded.guides.len());
    for guide in loaded.guides {
        views.push(StyleGuideView {
            path: store.path_for(&guide.id).to_string_lossy().into_owned(),
            json: serde_json::to_string_pretty(&guide).map_err(|error| error.to_string())?,
            rule_count: guide.rules.len(),
            model_rule_count: guide.rules.iter().filter(|r| !r.is_deterministic()).count(),
            id: guide.id,
            name: guide.name,
            active: guide.active,
            priority: guide.priority,
        });
    }
    Ok(views)
}

#[tauri::command]
async fn set_style_guide_active(id: String, active: bool) -> Result<(), String> {
    style_store()?
        .set_active(&id, active)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Saves a guide from its JSON text. Returns the normalized JSON that was written.
#[tauri::command]
async fn save_style_guide(json: String) -> Result<String, String> {
    let guide: broadside_style::StyleGuide =
        serde_json::from_str(&json).map_err(|error| format!("invalid guide JSON: {error}"))?;
    let store = style_store()?;
    store.save(&guide).map_err(|error| error.to_string())?;
    serde_json::to_string_pretty(&guide).map_err(|error| error.to_string())
}

#[tauri::command]
async fn delete_style_guide(id: String) -> Result<(), String> {
    style_store()?
        .delete(&id)
        .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StyleModelStatus {
    pub endpoint: String,
    pub model: String,
    pub reachable: bool,
    pub model_available: bool,
    pub available_models: Vec<String>,
    pub error: Option<String>,
}

#[tauri::command]
async fn get_style_model_status() -> Result<StyleModelStatus, String> {
    let config = broadside_style::model::ModelConfig::default();
    let result = tauri::async_runtime::spawn_blocking({
        let config = config.clone();
        move || broadside_style::model::available_models(&config)
    })
    .await
    .map_err(|error| error.to_string())?;

    Ok(match result {
        Ok(models) => StyleModelStatus {
            model_available: models.iter().any(|m| m == &config.model),
            available_models: models,
            reachable: true,
            error: None,
            endpoint: config.endpoint,
            model: config.model,
        },
        Err(error) => StyleModelStatus {
            reachable: false,
            model_available: false,
            available_models: Vec::new(),
            error: Some(error.to_string()),
            endpoint: config.endpoint,
            model: config.model,
        },
    })
}

/// Lane B: run the active guides' model rules against `text`. Slow; the UI must show progress.
#[tauri::command]
async fn style_check(text: String) -> Result<broadside_style::model::StyleCheckReport, String> {
    let guides = crate::style_guides::load_guides();
    let config = broadside_style::model::ModelConfig::default();
    tauri::async_runtime::spawn_blocking(move || {
        broadside_style::model::check(&text, &guides, &config)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Broadside deny-list
// ---------------------------------------------------------------------------

#[tauri::command]
async fn get_deny_list() -> Result<crate::deny_list::DenyList, String> {
    Ok(crate::deny_list::DenyList::load())
}

#[tauri::command]
async fn set_deny_list(apps: Vec<String>, urls: Vec<String>) -> Result<(), String> {
    let list = crate::deny_list::DenyList {
        apps: apps
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        urls: urls
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    };
    list.save()
}
