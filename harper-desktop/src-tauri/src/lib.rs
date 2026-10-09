mod tray;
mod windows;

use self::highlighter::Highlighter;
use self::highlighter_service::HighlighterService;
use self::tray::set_up_tray_menu;
use crate::communication::{Client, ProtocolError};
use crate::config::{Config, Integration};
use crate::debounce::{DebounceState, DebounceStatus};
use clap::{Parser, Subcommand};
use harper_core::{
    Dialect, DictWordMetadata, Document, IgnoredLints,
    linting::{Lint, LintGroup},
    spell::MutableDictionary,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::stderr;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex as StdMutex},
};
use tauri::Manager as _;
use tracing::{Level, error};
use tracing_subscriber::FmtSubscriber;

use crate::os_broker::{AccessibilityPermissionStatus, OsBroker};
use tokio::{
    io::{Stdin, Stdout},
    runtime::{Builder, Runtime},
    sync::Mutex,
};

pub mod color;
mod commands;
pub mod communication;
pub mod config;
mod debounce;
pub mod deny_list;
mod desktop_updater;
pub mod highlighter;
pub mod highlighter_service;
pub mod lint_kind_color;
mod os_broker;
pub mod rect;
pub mod style_check;
pub mod style_guides;

#[cfg(target_os = "macos")]
mod mac_broker;

#[cfg(target_os = "windows")]
mod windows_broker;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Highlighter {
        #[arg(long)]
        no_parent: bool,
    },
}

#[derive(Debug, Clone, Serialize)]
struct IntegrationView {
    bundle_id: String,
    enabled: bool,
    display_name: String,
}

#[cfg(target_os = "macos")]
pub(crate) type PlatformBroker = mac_broker::MacBroker;

#[cfg(target_os = "windows")]
pub(crate) type PlatformBroker = windows_broker::WindowsBroker;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) type PlatformBroker = os_broker::NoopBroker;

/// Creates the process-local platform broker.
///
/// The Tauri process stores its single broker as managed state, while the highlighter subprocess
/// creates its own broker because it is a separate process.
fn platform_broker(
    is_integration_enabled: impl FnMut(&str) -> bool + Send + 'static,
) -> PlatformBroker {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        PlatformBroker::new(is_integration_enabled)
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = is_integration_enabled;
        PlatformBroker::default()
    }
}

fn warm_app_search_cache(app: tauri::AppHandle) {
    tauri::async_runtime::spawn_blocking(move || {
        let broker = app.state::<StdMutex<PlatformBroker>>();
        let result = broker
            .lock()
            .map_err(|error| format!("failed to read platform broker: {error}"))
            .and_then(|broker| broker.search_apps("").map(|_| ()));

        if let Err(error) = result {
            eprintln!("failed to warm app search cache: {error}");
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let subscriber = FmtSubscriber::builder()
        .map_writer(move |_| stderr)
        .with_ansi(false)
        .with_max_level(Level::WARN)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("Unable to set up tracing subscriber.");

    let args = Args::parse();

    match args.command {
        Some(Command::Highlighter { no_parent }) => run_highlighter(!no_parent),
        None => run_tauri(),
    }
}

pub fn run_tauri() {
    let async_runtime = Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build config runtime");

    // Infer
    let is_first_launch = match Config::main_config_exists() {
        Ok(exists) => !exists,
        Err(error) => {
            eprintln!("failed to check config existence: {error}");
            false
        }
    };

    let config = if is_first_launch {
        let config = Config::new();

        if let Err(error) = async_runtime.block_on(config.save_to_system()) {
            eprintln!("failed to save initial config: {error}");
        }

        config
    } else {
        match async_runtime.block_on(Config::load_from_system()) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("failed to load config, using defaults: {error}");
                Config::new()
            }
        }
    };

    style_guides::ensure_samples();

    let broker = platform_broker(|_| false);

    let highlighter_service_enabled = config.highlighter_service_enabled;
    let config = Arc::new(Mutex::new(config));

    let highlighter_service = HighlighterService::new(config.clone());
    if broker.accessibility_permission_status() == AccessibilityPermissionStatus::Granted
        && highlighter_service_enabled
    {
        let _ = highlighter_service
            .start()
            .inspect_err(|err| error!("Unable to start highlighter: {err}"));
    }

    tauri::Builder::default()
        .manage(config)
        .manage(desktop_updater::DesktopUpdater::default())
        .manage(highlighter_service)
        .manage(StdMutex::new(broker))
        .manage(async_runtime)
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_os::init())
        .invoke_handler(commands::application_message_handler())
        .setup(move |app| {
            app.handle()
                .plugin(tauri_plugin_updater::Builder::new().build())?;

            set_up_tray_menu(app.handle())?;
            warm_app_search_cache(app.handle().clone());
            desktop_updater::start_auto_updates(app.handle().clone());

            if is_first_launch {
                windows::show_settings_window(app.handle())?;
            }

            #[cfg(target_os = "macos")]
            windows::sync_dock_visibility(app.handle(), None)?;

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::Destroyed,
                ..
            } = &event
            {
                let _ = windows::sync_dock_visibility(_app, Some(label))
                    .inspect_err(|err| error!("Could not update Dock visibility: {err}"));
            }

            // Keep the tray and service alive after the last window closes, but allow Quit.
            if let tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } = event
            {
                api.prevent_exit();
            }
        });
}

/// Run as a highlighter process.
/// Can configure whether to run standalone, or with a parent Tauri process
pub fn run_highlighter(has_parent: bool) {
    let client = Arc::new(StdMutex::new(Client::current_process()));
    let sync_runtime = Arc::new(
        Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build highlighter protocol runtime"),
    );

    let startup_config = if has_parent {
        fetch_highlighter_config(
            &mut client.lock().expect("IPC client lock poisoned"),
            &sync_runtime,
        )
    } else {
        Ok(Config::default())
    };

    let startup_config = match startup_config {
        Ok(config) => config,
        Err(error) => {
            eprintln!("failed to hydrate highlighter config, using defaults: {error}");
            Config::new()
        }
    };

    let startup_linter = startup_config.create_linter();
    let ignored_lints = Rc::new(RefCell::new(startup_config.ignored_lints));
    let user_dictionary = Rc::new(RefCell::new(startup_config.mutable_dictionary));
    let dialect = Rc::new(RefCell::new(startup_config.dialect));
    let integrations = Arc::new(StdMutex::new(IntegrationState {
        integrations: startup_config.integrations,
        auto_enable_new_apps: startup_config.auto_enable_new_apps,
    }));
    let debounce_ms = Rc::new(RefCell::new(startup_config.debounce_ms));
    let linter = Rc::new(RefCell::new(startup_linter));

    let integration_client = client.clone();
    let integration_runtime = sync_runtime.clone();
    let is_integration_enabled = integration_callback(integrations.clone(), move |bundle_id| {
        if !has_parent {
            return true;
        }
        match integration_runtime.block_on(
            integration_client
                .lock()
                .expect("IPC client lock poisoned")
                .resolve_integration(bundle_id),
        ) {
            Ok(enabled) => enabled,
            Err(error) => {
                eprintln!("failed to register integration: {error}");
                false
            }
        }
    });
    let broker = platform_broker(is_integration_enabled);

    let lint_ignored_lints = ignored_lints.clone();
    let lint_linter = linter.clone();
    let lint_user_dictionary = user_dictionary.clone();
    let lint_debounce_ms = debounce_ms.clone();
    let lint_debounce_state = Rc::new(RefCell::new(DebounceState::default()));

    let ignore_client = client.clone();
    let ignore_runtime = sync_runtime.clone();
    let ignore_ignored_lints = ignored_lints.clone();

    let dictionary_client = client.clone();
    let dictionary_runtime = sync_runtime.clone();
    let dictionary_user_dictionary = user_dictionary.clone();
    let dictionary_linter = linter.clone();
    let dictionary_dialect = dialect.clone();
    let dictionary_debounce_ms = debounce_ms.clone();

    let disable_client = client.clone();
    let disable_runtime = sync_runtime.clone();
    let disable_linter = linter.clone();

    let refresh_client = client.clone();
    let refresh_runtime = sync_runtime.clone();
    let refresh_ignored_lints = ignored_lints.clone();
    let refresh_user_dictionary = user_dictionary.clone();
    let refresh_dialect = dialect.clone();
    let refresh_integrations = integrations.clone();
    let refresh_debounce_ms = debounce_ms.clone();
    let refresh_linter = linter.clone();

    // Broadside: findings from the on-demand model check, appended to every lint pass for the
    // text they still apply to. Filled by the event loop when a check finishes.
    let model_findings: style_check::SharedFindings =
        Rc::new(RefCell::new(style_check::FindingSets::default()));
    let lint_model_findings = model_findings.clone();
    let lint_model_ignored = ignored_lints.clone();
    let style_checker = style_check::StyleChecker::start();
    // Broadside: the focused app, kept current by the event loop. Guides bound to it switch on.
    let current_app: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let lint_current_app = current_app.clone();
    let refresh_current_app = current_app.clone();
    let lint_dialect = dialect.clone();
    let mut lint_app_cache: Option<String> = None;

    let lint_text = move |text: &str| {
        // Rebuild the linter when the focused app changes so app-bound guides follow focus.
        let app = lint_current_app.borrow().clone();
        if app != lint_app_cache {
            let dictionary =
                Config::dictionary_from_user_dictionary(lint_user_dictionary.borrow().clone());
            let flat_config = lint_linter.borrow().config.clone();
            let mut group = LintGroup::new_curated(dictionary, *lint_dialect.borrow())
                .with_lint_config(flat_config);
            style_guides::install_into_for_app(&mut group, app.as_deref());
            *lint_linter.borrow_mut() = group;
            lint_app_cache = app;
        }

        let debounce_ms = *lint_debounce_ms.borrow();
        let mut debounce_state = lint_debounce_state.borrow_mut();

        match debounce_state.status(text, debounce_ms) {
            DebounceStatus::Cached(mut lints) => {
                append_findings(&mut lints, text, &lint_model_findings, &lint_model_ignored);
                return lints;
            }
            DebounceStatus::Ready => {}
        }

        let dictionary =
            Config::dictionary_from_user_dictionary(lint_user_dictionary.borrow().clone());
        let doc = Document::new_markdown_default(text, &dictionary);
        let mut organized_lints = lint_linter.borrow_mut().organized_lints(&doc);

        for lints in organized_lints.values_mut() {
            lint_ignored_lints.borrow().remove_ignored(lints, &doc);
        }

        debounce_state.store_lints(text, debounce_ms, &organized_lints);
        append_findings(
            &mut organized_lints,
            text,
            &lint_model_findings,
            &lint_model_ignored,
        );

        organized_lints
    };

    let ignore_lint = move |lint: &Lint, document: &Document| {
        {
            ignore_ignored_lints
                .borrow_mut()
                .ignore_lint(lint, document);
        }

        let snapshot = ignore_ignored_lints.borrow().clone();
        if let Err(error) = ignore_runtime.block_on(
            ignore_client
                .lock()
                .expect("IPC client lock poisoned")
                .ignore_lint(&snapshot),
        ) {
            eprintln!("failed to sync ignored lints: {error}");
        }
    };

    let add_to_dictionary = move |word: &str| {
        dictionary_user_dictionary
            .borrow_mut()
            .append_word_str(word, DictWordMetadata::default());

        let lint_config = dictionary_linter.borrow().config.clone();
        let config = Config {
            mutable_dictionary: dictionary_user_dictionary.borrow().clone(),
            dialect: *dictionary_dialect.borrow(),
            ignored_lints: IgnoredLints::new(),
            lint_config,
            integrations: Vec::new(),
            auto_enable_new_apps: false,
            onboarding_completed: false,
            debounce_ms: *dictionary_debounce_ms.borrow(),
            auto_update: true,
            last_update_check: None,
            highlighter_service_enabled: true,
        };
        *dictionary_linter.borrow_mut() = config.create_linter();

        if let Err(error) = dictionary_runtime.block_on(
            dictionary_client
                .lock()
                .expect("IPC client lock poisoned")
                .add_to_dictionary(word),
        ) {
            eprintln!("failed to sync dictionary update: {error}");
        }
    };

    let disable_rule = move |rule_name: &str| match disable_runtime.block_on(
        disable_client
            .lock()
            .expect("IPC client lock poisoned")
            .disable_rule(rule_name),
    ) {
        Ok(config) => disable_linter.borrow_mut().config = config,
        Err(error) => eprintln!("failed to disable rule {rule_name}: {error}"),
    };

    let refresh_config = move || {
        if !has_parent {
            return;
        }

        match fetch_highlighter_config(
            &mut refresh_client.lock().expect("IPC client lock poisoned"),
            &refresh_runtime,
        ) {
            Ok(config) => apply_highlighter_config(
                config,
                &refresh_ignored_lints,
                &refresh_user_dictionary,
                &refresh_dialect,
                &refresh_integrations,
                &refresh_debounce_ms,
                &refresh_linter,
                refresh_current_app.borrow().as_deref(),
            ),
            Err(error) => {
                eprintln!("failed to refresh highlighter config: {error}");
                std::process::exit(1);
            }
        }
    };

    if let Err(error) = Highlighter::new(
        broker,
        lint_text,
        ignore_lint,
        add_to_dictionary,
        disable_rule,
        refresh_config,
        style_checker,
        model_findings,
        current_app,
    )
    .and_then(Highlighter::run_window_for_each_monitor)
    {
        eprintln!("failed to run highlighter: {error}");
    }
}

/// Highlighter-local app policy snapshot, replaced on each config poll.
/// Also caches discovery decisions (including denials) until the next poll to avoid per-frame IPC.
struct IntegrationState {
    integrations: Vec<Integration>,
    auto_enable_new_apps: bool,
}

/// Builds the broker's policy callback. Known apps are checked locally; only unknown apps with
/// automatic enablement on reach `resolve`, which persists registration in the parent process.
/// Harper itself is excluded from discovery, but explicitly configured entries are respected.
/// Standalone callers can approve registration locally without IPC.
fn integration_callback(
    state: Arc<StdMutex<IntegrationState>>,
    mut resolve: impl FnMut(&str) -> bool + Send + 'static,
) -> impl FnMut(&str) -> bool + Send + 'static {
    move |bundle_id| {
        let bundle_id = bundle_id.trim();
        if bundle_id.is_empty() {
            return false;
        }
        let mut state = state.lock().expect("integration state lock poisoned");
        if let Some(integration) = state
            .integrations
            .iter()
            .find(|item| item.matches(bundle_id))
        {
            return integration.enabled;
        }
        if !state.auto_enable_new_apps || PlatformBroker::is_harper_desktop(bundle_id) {
            return false;
        }
        let enabled = resolve(bundle_id);
        state.integrations.push(Integration {
            bundle_id: bundle_id.to_owned(),
            enabled,
        });
        enabled
    }
}

fn fetch_highlighter_config(
    client: &mut Client<Stdin, Stdout>,
    runtime: &Runtime,
) -> Result<Config, ProtocolError> {
    runtime.block_on(async {
        let dialect = client.get_dialect().await?;
        let mutable_dictionary = client.get_dictionary().await?;
        let ignored_lints = client.get_ignored_lints().await?;
        let lint_config = client.get_lint_config().await?;
        let integrations = client.get_integrations().await?;
        let auto_enable_new_apps = client.get_auto_enable_new_apps().await?;
        let debounce_ms = client.get_debounce_ms().await?;

        Ok(Config {
            dialect,
            mutable_dictionary,
            ignored_lints,
            lint_config,
            integrations,
            auto_enable_new_apps,
            onboarding_completed: false,
            debounce_ms,
            auto_update: true,
            last_update_check: None,
            highlighter_service_enabled: true,
        })
    })
}

fn apply_highlighter_config(
    config: Config,
    ignored_lints: &Rc<RefCell<IgnoredLints>>,
    user_dictionary: &Rc<RefCell<MutableDictionary>>,
    dialect: &Rc<RefCell<Dialect>>,
    integrations: &Arc<StdMutex<IntegrationState>>,
    debounce_ms: &Rc<RefCell<u64>>,
    linter: &Rc<RefCell<LintGroup>>,
    current_app: Option<&str>,
) {
    let linter_config = config.create_linter_for_app(current_app);
    *ignored_lints.borrow_mut() = config.ignored_lints;
    *user_dictionary.borrow_mut() = config.mutable_dictionary;
    *dialect.borrow_mut() = config.dialect;
    match integrations.lock() {
        Ok(mut integrations) => {
            *integrations = IntegrationState {
                integrations: config.integrations,
                auto_enable_new_apps: config.auto_enable_new_apps,
            }
        }
        Err(error) => eprintln!("failed to update integrations: {error}"),
    }
    *debounce_ms.borrow_mut() = config.debounce_ms;
    *linter.borrow_mut() = linter_config;
}

/// Adds the model check's and the thesaurus's findings for `text` under their own rule names,
/// minus any the user has ignored. Findings are re-anchored to `text`; ones that no longer
/// apply are left out.
fn append_findings(
    lints: &mut BTreeMap<String, Vec<Lint>>,
    text: &str,
    findings: &style_check::SharedFindings,
    ignored: &Rc<RefCell<IgnoredLints>>,
) {
    let sets = findings.borrow();
    let mut document = None;
    for (rule_name, set) in sets.iter() {
        let mut found = set.lints_for(text);
        if found.is_empty() {
            continue;
        }
        let document = document.get_or_insert_with(|| Document::new_markdown_default_curated(text));
        ignored.borrow().remove_ignored(&mut found, document);
        if !found.is_empty() {
            lints.insert(rule_name.to_string(), found);
        }
    }
}
