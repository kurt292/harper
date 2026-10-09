use crate::windows_broker::automation_service::{AutomationService, Read};
use crate::{
    os_broker::{AccessibilityPermissionStatus, AppSearchResult, OsBroker},
    rect::ActionableLint,
};
use cached::cached;
use egui::Pos2;
use harper_core::linting::Lint;
use std::ffi::{OsString, c_void};
use std::os::windows::ffi::OsStringExt;
use std::process::Command;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use windows::Win32::Foundation::{CloseHandle, HWND, POINT};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GUI_INMOVESIZE, GUITHREADINFO, GetCursorPos, GetForegroundWindow, GetGUIThreadInfo,
    GetWindowThreadProcessId,
};
use windows::core::{PWSTR, Result as WindowsResult};
use wintheon::file::{IconSize, Priority};
use wintheon::gather::Gatherer;
mod automation_service;

pub struct WindowsBroker {
    service: Arc<Mutex<AutomationService>>,
    is_integration_enabled: Box<dyn FnMut(&str) -> bool + Send>,
    /// Last diagnostic state, so the log only changes when the situation does.
    last_diagnostic: Option<String>,
    /// Text of the last successful read, for the model style check.
    last_text: Option<String>,
    /// Executable path of the app owning the focused window, as of the last poll.
    last_app: Option<String>,
}

impl WindowsBroker {
    /// Creates a broker with an app policy that may register newly encountered executable paths.
    /// The policy is called before reading the app's text and may change as settings are refreshed.
    pub fn new(is_integration_enabled: impl FnMut(&str) -> bool + Send + 'static) -> Self {
        Self {
            service: Arc::new(Mutex::new(AutomationService::create_and_start())),
            is_integration_enabled: Box::new(is_integration_enabled),
            last_diagnostic: None,
            last_text: None,
            last_app: None,
        }
    }

    /// Logs `message` once per change. Never include field text.
    fn diagnose(&mut self, message: String) {
        if self.last_diagnostic.as_deref() != Some(message.as_str()) {
            eprintln!("broker: {message}");
            self.last_diagnostic = Some(message);
        }
    }

    pub fn should_lint_focused_window(&mut self) -> Option<bool> {
        let mut service = self.service.lock().ok()?;
        let focused_window = service.resolve_focused_window()?;
        let path = get_window_path(focused_window).ok()?;
        drop(service);

        let exe = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.last_app = Some(path.to_string_lossy().into_owned());
        if !(self.is_integration_enabled)(&path.to_string_lossy()) {
            self.diagnose(format!("{exe}: integration disabled, not linting"));
            return Some(false);
        }

        match window_is_moving(HWND(focused_window as *mut c_void)) {
            Ok(is_moving) => Some(!is_moving),
            Err(error) => {
                eprintln!(
                    "Unable to determine whether the focused Windows window is moving: {error}"
                );
                None
            }
        }
    }
}

impl OsBroker for WindowsBroker {
    fn is_harper_desktop(app_id: &str) -> bool {
        let Ok(executable) = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .inspect_err(|error| eprintln!("failed to identify Harper executable: {error}"))
        else {
            return false;
        };
        std::fs::canonicalize(app_id).is_ok_and(|path| {
            path.to_string_lossy()
                .eq_ignore_ascii_case(&executable.to_string_lossy())
        })
    }

    fn get_boxes(
        &mut self,
        lint_text: &mut dyn FnMut(&str) -> BTreeMap<String, Vec<Lint>>,
    ) -> Option<Vec<ActionableLint>> {
        match self.should_lint_focused_window() {
            Some(true) => {}
            Some(false) => return Some(Vec::new()),
            None => return None,
        }

        let text_read = self.service.lock().ok()?.get_text();
        let text = match text_read {
            Read::Ready(text) => text,
            // The worker has not answered yet; the overlay keeps what it last showed.
            Read::Pending => return None,
            Read::Unavailable => {
                self.diagnose("no lintable focused text field".to_string());
                return None;
            }
        };
        if text.len() > 16_000 {
            self.diagnose(format!(
                "field has {} bytes, over the 16k limit; skipping",
                text.len()
            ));
            return Some(Vec::new());
        }

        self.last_text = Some(text.clone());
        let lints = lint_text(&text);
        let lint_count: usize = lints.values().map(Vec::len).sum();
        let rects_read = self
            .service
            .lock()
            .ok()?
            .get_bounding_boxes(&text, lints.values().flatten().map(|lint| lint.span));
        let rects = match rects_read {
            Read::Ready(rects) => rects,
            Read::Pending => return None,
            Read::Unavailable => {
                self.diagnose(format!(
                    "{} chars, {lint_count} lints, but bounding boxes unavailable",
                    text.chars().count()
                ));
                return None;
            }
        };
        let first_rect = rects
            .iter()
            .flatten()
            .next()
            .map(|r| {
                format!(
                    " first rect x={:.0} y={:.0} w={:.0} h={:.0} (scale {:.2})",
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    get_focused_monitor_scale()
                )
            })
            .unwrap_or_default();
        self.diagnose(format!(
            "{} chars, {lint_count} lints, {} rects{first_rect}",
            text.chars().count(),
            rects.iter().map(Vec::len).sum::<usize>()
        ));

        Some(
            lints
                .into_iter()
                .flat_map(|(lint_id, lints)| {
                    lints.into_iter().map(move |lint| (lint_id.clone(), lint))
                })
                .zip(rects)
                .flat_map(|((lint_id, lint), rects)| {
                    let text = text.clone();
                    let service = self.service.clone();
                    rects.into_iter().map(move |rect| {
                        let service = service.clone();
                        let suggestion_text = text.clone();
                        let suggestion_span = lint.span;
                        ActionableLint::new(
                            rect,
                            lint_id.clone(),
                            lint.clone(),
                            text.clone(),
                            move |suggestion| {
                                service.lock().unwrap().apply_suggestion(
                                    suggestion_text,
                                    suggestion_span,
                                    suggestion,
                                );
                            },
                        )
                    })
                })
                .collect(),
        )
    }

    fn last_read_text(&self) -> Option<String> {
        self.last_text.clone()
    }

    fn focused_app_id(&self) -> Option<String> {
        self.last_app.clone()
    }

    fn cursor_position(&self) -> Option<Pos2> {
        let mut point = POINT::default();

        // GetCursorPos fails with "Access is denied" while the lock screen or a UAC prompt owns
        // the desktop. Report no cursor rather than take the highlighter down with a panic.
        unsafe {
            GetCursorPos(&mut point).ok()?;
        }

        let monitor_scale = get_focused_monitor_scale();

        let pos = Pos2::new(
            point.x as f32 / monitor_scale as f32,
            point.y as f32 / monitor_scale as f32,
        );

        Some(pos)
    }

    fn accessibility_permission_status(&self) -> AccessibilityPermissionStatus {
        AccessibilityPermissionStatus::Granted
    }

    fn request_accessibility_permission(&self) -> AccessibilityPermissionStatus {
        AccessibilityPermissionStatus::Granted
    }

    fn integration_display_name(&self, bundle_id: &str) -> String {
        if let Some(entry) = look_up_application(bundle_id) {
            return entry.display_name;
        }

        bundle_id.to_string()
    }

    fn installed_application_bundle_ids(&self) -> Result<Vec<String>, String> {
        let list = installed_applications_list();
        Ok(list
            .iter()
            .map(|i| i.path.to_string_lossy().into_owned())
            .collect())
    }

    fn application_icon_png(&self, bundle_id: &str) -> Result<Vec<u8>, String> {
        if let Some(entry) = look_up_application(bundle_id) {
            if let Some(png) = entry.icon_png {
                return Ok(png);
            } else {
                return Err("Found application but it was missing an icon.".to_string());
            }
        } else {
            return Err("Unable to locate application.".to_string());
        }
    }

    fn launch_app_bundle(&self, bundle_id: &str) -> Result<(), String> {
        Command::new(bundle_id)
            .spawn()
            .map_err(|err| err.to_string())?;
        Ok(())
    }

    /// Search for an application in the OS' global list of installed apps.
    fn search_apps(&self, query: &str) -> Result<Vec<AppSearchResult>, String> {
        let list = installed_applications_list();
        let query = query.trim();

        if query.is_empty() {
            return Ok(list.iter().map(|entry| entry.to_search_result()).collect());
        }

        if let Some(result) = list
            .iter()
            .find(|result| result.path.to_string_lossy() == query)
            .cloned()
        {
            return Ok(vec![result.to_search_result()]);
        }

        let lower_query = query.to_lowercase();
        Ok(list
            .iter()
            .filter(|result| {
                result.display_name.to_lowercase().contains(&lower_query)
                    || result
                        .path
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&lower_query)
            })
            .cloned()
            .map(|entry| entry.to_search_result())
            .collect())
    }
}

/// Reports whether `hwnd` is in its GUI thread's modal move-or-resize loop.
///
/// Win32 combines moving and resizing under `GUI_INMOVESIZE`. Checking `hwndMoveSize` ensures a
/// different window owned by the same GUI thread does not produce a false positive.
fn window_is_moving(hwnd: HWND) -> WindowsResult<bool> {
    unsafe {
        let thread_id = GetWindowThreadProcessId(hwnd, None);
        if thread_id == 0 {
            return Err(windows::core::Error::from_thread());
        }

        let mut thread_info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(thread_id, &mut thread_info)?;

        Ok(thread_info.flags.contains(GUI_INMOVESIZE) && thread_info.hwndMoveSize == hwnd)
    }
}

fn get_focused_monitor_scale() -> f64 {
    unsafe {
        let window = GetForegroundWindow();
        let monitor = MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST);

        let mut x = 0;
        let mut y = 0;

        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y);

        let effective_scale = x as f64 / 96.;
        effective_scale
    }
}

#[derive(Debug, Clone)]
struct ApplicationListEntry {
    path: PathBuf,
    /// The PNG bytes of an icon. 256 on a side, square.
    icon_png: Option<Vec<u8>>,
    display_name: String,
}

impl ApplicationListEntry {
    fn to_search_result(&self) -> AppSearchResult {
        AppSearchResult {
            name: self.display_name.clone(),
            bundle_id: self.path.to_string_lossy().to_owned().to_string(),
        }
    }
}

fn look_up_application(bundle_id: &str) -> Option<ApplicationListEntry> {
    // In Windows, the application path is the bundle ID.
    let list = installed_applications_list();
    if let Some(entry) = list
        .iter()
        .find(|entry| entry.path.to_string_lossy() == bundle_id)
    {
        Some(entry.clone())
    } else {
        None
    }
}

#[cached]
fn installed_applications_list() -> Arc<Vec<ApplicationListEntry>> {
    let mut list = Vec::new();

    for res in gatherer().scan() {
        if let Ok(app) = res {
            let icon = if let Ok(icon) = app.entry.icon() {
                icon.extract_icon_as_png_at(IconSize::Jumbo)
            } else {
                None
            };

            list.push(ApplicationListEntry {
                path: app.entry.path().to_owned(),
                icon_png: icon,
                display_name: app.entry.display_name(),
            })
        }
    }

    Arc::new(list)
}

fn gatherer() -> Gatherer {
    Gatherer::new()
        .with_desktop(Priority(1.0))
        .with_start_menu(Priority(1.5))
        .with_windows_apps(Priority(2.0))
}

/// Returns the full executable path for the process that owns `hwnd`.
pub fn get_window_path(window_id: isize) -> WindowsResult<PathBuf> {
    unsafe {
        let mut process_id = 0;
        GetWindowThreadProcessId(HWND(window_id as *mut c_void), Some(&mut process_id));

        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id)?;

        let mut buffer = vec![0u16; 32_768];
        let mut length = buffer.len() as u32;
        let path_result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        );
        let close_result = CloseHandle(process);

        path_result?;
        close_result?;

        Ok(PathBuf::from(OsString::from_wide(
            &buffer[..length as usize],
        )))
    }
}
