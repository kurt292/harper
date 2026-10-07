use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::iter::once;
use std::sync::mpsc::{
    Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError, channel,
    sync_channel,
};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::rect::Rect;
use crate::windows_broker::get_focused_monitor_scale;
use harper_core::{Span, linting::Suggestion};
use is_macro::Is;
use uiautomation::types::{
    ControlType, Handle, TextPatternRangeEndpoint, TextUnit, TreeScope, UIProperty,
};
use uiautomation::variants::Variant;
use uiautomation::{
    UIAutomation, UIElement,
    patterns::{UITextPattern, UITextRange, UIValuePattern},
};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::IUIAutomationTextRange;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_DELETE, VK_MENU,
    VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
};

/// Information about a worker thread.
///
/// Reads never block the caller. A job is queued, the caller carries on with whatever it had,
/// and the result is collected on a later call. Every job carries a sequence number and the
/// fingerprint of its inputs, so a result is only handed back to a caller asking about the same
/// window and text it was computed for.
struct WorkerData {
    sender: SyncSender<(u64, WorkerJob, Vec<JobArgument>)>,
    receiver: Receiver<(u64, JobResult)>,
    next_job_id: u64,
    /// Jobs queued or running, oldest first.
    pending: VecDeque<PendingJob>,
    /// The latest finished result per job kind, with the fingerprint it was computed for.
    completed: HashMap<JobKind, (u64, JobResult)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum JobKind {
    Text,
    Rects,
    Apply,
}

struct PendingJob {
    id: u64,
    kind: JobKind,
    fingerprint: u64,
    since: Instant,
}

/// Outcome of a non-blocking read.
pub enum Read<T> {
    /// A result computed for exactly these inputs.
    Ready(T),
    /// The worker has not answered yet; keep whatever was shown last.
    Pending,
    /// The worker answered that there is nothing to read.
    Unavailable,
}

/// Applying a suggestion types into the target and verifies it, so it needs longer.
const APPLY_JOB_TIMEOUT: Duration = Duration::from_secs(3);

/// A worker stuck inside UI Automation for this long is abandoned and replaced.
const STUCK_WORKER_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct ApplySuggestionRequest {
    window: isize,
    expected_text: String,
    span: Span<char>,
    suggestion: Suggestion,
}

#[derive(Debug, Is)]
enum JobArgument {
    Span(Span<char>),
    Window(isize),
    Text(String),
    ApplySuggestion(ApplySuggestionRequest),
}

/// The result of a job run by the worker thread.
#[derive(Debug, Is)]
enum JobResult {
    None,
    String(String),
    GroupedRects(Vec<Vec<Rect>>),
    Err,
}

/// An actual function pointer to be run by the worker thread.
type WorkerJob = fn(&UIAutomation, Vec<JobArgument>) -> JobResult;

/// Runs and communicates with a worker thread to interact with the Win32 Automation API to query the accessibility tree.
/// Necessary because the API has very specific thread setting requirements to work.
pub struct AutomationService {
    worker_data: Option<WorkerData>,
    // Needed to redirect focus to the last focused window when the focus arrives on the Harper highlighter window
    last_focused_window: Option<isize>,
}

impl AutomationService {
    pub fn create_and_start() -> Self {
        let mut output = Self {
            last_focused_window: None,
            worker_data: None,
        };

        output.start_worker_thread();

        output
    }

    /// Starts the worker thread if it is not already running.
    /// Does nothing if the worker thread is already running.
    fn start_worker_thread(&mut self) {
        let (job_sender, job_receiver) = sync_channel::<(u64, WorkerJob, Vec<JobArgument>)>(1);
        let (result_sender, result_receiver): (
            Sender<(u64, JobResult)>,
            Receiver<(u64, JobResult)>,
        ) = channel();

        std::thread::spawn(move || {
            let automation = UIAutomation::new().unwrap();

            loop {
                // Stop the thread if the other side of the channel has been closed (or dropped).
                let job = match job_receiver.try_recv() {
                    Err(TryRecvError::Disconnected) => break,
                    Err(TryRecvError::Empty) => None,
                    Ok(job) => Some(job),
                };

                if let Some((id, job, arguments)) = job {
                    let result = job(&automation, arguments);

                    // Stop the thread if the other side of the channel has been closed (or dropped).
                    if result_sender.send((id, result)).is_err() {
                        break;
                    }
                }

                sleep(Duration::from_millis(16));
            }
        });

        self.worker_data = Some(WorkerData {
            receiver: result_receiver,
            sender: job_sender,
            next_job_id: 0,
            pending: VecDeque::new(),
            completed: HashMap::new(),
        });
    }

    /// Stops the worker thread if it is running. This method does nothing if it is not running.
    fn stop_worker_thread(&mut self) {
        // This drops the inner fields, which closes the channel, which signals to the worker to stop running.
        self.worker_data = None;
    }

    /// Moves finished results from the worker into `completed`, and replaces a worker that has
    /// been stuck inside UI Automation for too long.
    ///
    /// The caller is the highlighter's event loop. A UI Automation call that blocks (a huge
    /// browser tree, an unresponsive provider) must never block that loop, or Windows reports the
    /// overlay as hung and closes it. Nothing in the read path waits on the worker.
    fn collect_results(&mut self) {
        let Some(worker_data) = self.worker_data.as_mut() else {
            return;
        };

        while let Ok((id, result)) = worker_data.receiver.try_recv() {
            if let Some(index) = worker_data.pending.iter().position(|job| job.id == id) {
                let job = worker_data
                    .pending
                    .remove(index)
                    .expect("index came from position");
                worker_data
                    .completed
                    .insert(job.kind, (job.fingerprint, result));
            }
        }

        if let Some(oldest) = worker_data.pending.front()
            && oldest.since.elapsed() > STUCK_WORKER_TIMEOUT
        {
            eprintln!(
                "UI Automation worker stuck for {} ms; replacing it",
                oldest.since.elapsed().as_millis()
            );
            self.start_worker_thread();
        }
    }

    /// Queues a job of `kind` unless one is already outstanding. Never blocks.
    fn submit(
        &mut self,
        kind: JobKind,
        fingerprint: u64,
        job: WorkerJob,
        arguments: Vec<JobArgument>,
    ) {
        let Some(worker_data) = self.worker_data.as_mut() else {
            return;
        };
        if worker_data.pending.iter().any(|job| job.kind == kind) {
            return;
        }

        worker_data.next_job_id += 1;
        let id = worker_data.next_job_id;
        if worker_data.sender.try_send((id, job, arguments)).is_ok() {
            worker_data.pending.push_back(PendingJob {
                id,
                kind,
                fingerprint,
                since: Instant::now(),
            });
        }
    }

    /// Takes the finished result for `kind` if it was computed for `fingerprint`. A result for
    /// other inputs is stale and dropped.
    fn take_completed(&mut self, kind: JobKind, fingerprint: u64) -> Option<JobResult> {
        let worker_data = self.worker_data.as_mut()?;
        match worker_data.completed.remove(&kind) {
            Some((computed_for, result)) if computed_for == fingerprint => Some(result),
            _ => None,
        }
    }

    /// The focused field's text, as of the latest finished read. Queues the next read.
    pub fn get_text(&mut self) -> Read<String> {
        let Some(window) = self.resolve_focused_window() else {
            return Read::Unavailable;
        };
        self.collect_results();

        let fingerprint = window as u64;
        self.submit(
            JobKind::Text,
            fingerprint,
            get_text_job,
            vec![JobArgument::Window(window)],
        );

        match self.take_completed(JobKind::Text, fingerprint) {
            Some(JobResult::String(text)) => Read::Ready(text),
            Some(_) => Read::Unavailable,
            None => Read::Pending,
        }
    }

    /// Applies a suggestion. This one waits, bounded, because it is a user action whose outcome
    /// matters and which types into the target application.
    pub fn apply_suggestion(
        &mut self,
        expected_text: String,
        span: Span<char>,
        suggestion: Suggestion,
    ) {
        let Some(window) = self.resolve_focused_window() else {
            return;
        };
        self.collect_results();

        let request = ApplySuggestionRequest {
            window,
            expected_text,
            span,
            suggestion,
        };
        let Some(worker_data) = self.worker_data.as_mut() else {
            return;
        };
        worker_data.next_job_id += 1;
        let id = worker_data.next_job_id;
        let deadline = Instant::now() + APPLY_JOB_TIMEOUT;

        // The queue holds one job; wait for room behind any read in progress.
        let mut payload = (
            id,
            apply_suggestion_job as WorkerJob,
            vec![JobArgument::ApplySuggestion(request)],
        );
        loop {
            match worker_data.sender.try_send(payload) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) => {
                    if Instant::now() > deadline {
                        eprintln!("Could not apply the suggestion: UI Automation worker is busy");
                        return;
                    }
                    payload = returned;
                    sleep(Duration::from_millis(10));
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
        worker_data.pending.push_back(PendingJob {
            id,
            kind: JobKind::Apply,
            fingerprint: 0,
            since: Instant::now(),
        });

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match worker_data.receiver.recv_timeout(remaining) {
                Ok((result_id, result)) => {
                    if let Some(index) = worker_data
                        .pending
                        .iter()
                        .position(|job| job.id == result_id)
                    {
                        let job = worker_data
                            .pending
                            .remove(index)
                            .expect("index came from position");
                        if result_id == id {
                            return;
                        }
                        worker_data
                            .completed
                            .insert(job.kind, (job.fingerprint, result));
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    eprintln!(
                        "Applying the suggestion exceeded {} s; giving up on it",
                        APPLY_JOB_TIMEOUT.as_secs()
                    );
                    return;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Bounding boxes for `spans` in `text`, as of the latest finished read for exactly those
    /// inputs. Queues the next read. Each span may have multiple bounding boxes; input spans share
    /// the same index as their output bounding box.
    pub fn get_bounding_boxes(
        &mut self,
        text: &str,
        spans: impl IntoIterator<Item = Span<char>>,
    ) -> Read<Vec<Vec<Rect>>> {
        let Some(window) = self.resolve_focused_window() else {
            return Read::Unavailable;
        };
        self.collect_results();

        let spans: Vec<Span<char>> = spans.into_iter().collect();
        let fingerprint = {
            let mut hasher = DefaultHasher::new();
            window.hash(&mut hasher);
            text.hash(&mut hasher);
            for span in &spans {
                span.start.hash(&mut hasher);
                span.end.hash(&mut hasher);
            }
            hasher.finish()
        };

        self.submit(
            JobKind::Rects,
            fingerprint,
            get_bounding_rect_job,
            once(JobArgument::Window(window))
                .chain(once(JobArgument::Text(text.to_string())))
                .chain(spans.into_iter().map(JobArgument::Span))
                .collect(),
        );

        match self.take_completed(JobKind::Rects, fingerprint) {
            Some(JobResult::GroupedRects(rects)) => Read::Ready(rects),
            Some(_) => Read::Unavailable,
            None => Read::Pending,
        }
    }

    /// Returns the foreground source window, retaining the last external window while Harper's
    /// overlay owns focus.
    pub fn resolve_focused_window(&mut self) -> Option<isize> {
        let (focused_window, focused_process_id) = focused_window()?;

        if focused_process_id == std::process::id() {
            return self.last_focused_window;
        }

        self.last_focused_window = Some(focused_window);
        Some(focused_window)
    }
}

impl Drop for AutomationService {
    fn drop(&mut self) {
        self.stop_worker_thread();
    }
}

// ---------------------------------------------------------------------------
// Applying suggestions
// ---------------------------------------------------------------------------

/// How long to wait for the target field to regain keyboard focus after the user clicks a
/// suggestion in Harper's overlay.
const FOCUS_RETURN_TIMEOUT: Duration = Duration::from_millis(400);

/// How long to wait for the user to release modifier keys before typing a replacement.
const MODIFIER_RELEASE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long to give the target application to process simulated input before re-reading.
const POST_INPUT_SETTLE: Duration = Duration::from_millis(120);

/// Why the selection-based write-back could not be used.
enum SelectionApplyError {
    /// The provider lacks what the method needs. Falling back to `ValuePattern.SetValue` is fine.
    Unavailable(String),
    /// A verification step failed before any input was sent. The field is untouched.
    Aborted(String),
}

fn apply_suggestion_job(automation: &UIAutomation, mut arguments: Vec<JobArgument>) -> JobResult {
    let Some(JobArgument::ApplySuggestion(request)) = arguments.pop() else {
        return JobResult::Err;
    };

    if !arguments.is_empty() {
        return JobResult::Err;
    }

    // Clicking the suggestion card moves keyboard focus to Harper's overlay, so a lookup that
    // requires a focused field finds nothing. Prefer the element the last read used, verified
    // against the text that was linted, and only then search the window without a focus filter.
    let Some(element) = cached_text_element(request.window, &request.expected_text)
        .or_else(|| {
            text_element_for_window(automation, request.window, Some(&request.expected_text)).ok()
        })
        .or_else(|| search_text_element(automation, request.window, &request.expected_text).ok())
    else {
        eprintln!(
            "Unable to apply Windows suggestion: the source text element is no longer available"
        );
        return JobResult::None;
    };

    let Ok(current_text) = get_text(&element) else {
        eprintln!("Unable to apply Windows suggestion: the source text can no longer be read");
        return JobResult::None;
    };

    let updated_text = match apply_suggestion_to_text(
        &current_text,
        &request.expected_text,
        request.span,
        &request.suggestion,
    ) {
        Ok(updated_text) => updated_text,
        Err(error) => {
            eprintln!("Unable to apply Windows suggestion: {error}");
            return JobResult::None;
        }
    };

    // Preferred path: select exactly the affected range through UIA and type the replacement.
    // This keeps the caret where a human edit would leave it and works in providers that expose
    // `TextPattern` but no writable `ValuePattern` (Chromium contenteditable surfaces, for one).
    match apply_via_selection(
        automation,
        &element,
        request.window,
        &current_text,
        request.span,
        &request.suggestion,
    ) {
        Ok(()) => {
            sleep(POST_INPUT_SETTLE);
            match get_text(&element) {
                Ok(after) if after == updated_text => {}
                Ok(after) => eprintln!(
                    "Windows suggestion applied but the field reads differently than expected \
                     ({} chars vs {} expected); leaving it for the user",
                    after.chars().count(),
                    updated_text.chars().count()
                ),
                Err(error) => {
                    eprintln!(
                        "Windows suggestion applied but the field could not be re-read: {error}"
                    )
                }
            }
            return JobResult::None;
        }
        Err(SelectionApplyError::Unavailable(reason)) => {
            eprintln!("Selection write-back unavailable ({reason}); falling back to SetValue");
        }
        Err(SelectionApplyError::Aborted(reason)) => {
            eprintln!(
                "Selection write-back aborted before typing ({reason}); falling back to SetValue"
            );
        }
    }

    // Fallback: replace the whole value. Verified safe content-wise above, but most providers move
    // the caret to the start or end of the field afterwards.
    let Ok(value_pattern) = element.get_pattern::<UIValuePattern>() else {
        eprintln!(
            "Unable to apply Windows suggestion: the text element has no writable value pattern"
        );
        return JobResult::None;
    };

    match value_pattern.is_readonly() {
        Ok(true) => {
            eprintln!("Unable to apply Windows suggestion: the text element is read-only");
        }
        Ok(false) => {
            // Re-check right before writing: the selection attempt may have taken a moment.
            match get_text(&element) {
                Ok(text) if text == request.expected_text => {
                    if let Err(error) = value_pattern.set_value(&updated_text) {
                        eprintln!("Unable to apply Windows suggestion: {error}");
                    }
                }
                Ok(_) => eprintln!(
                    "Unable to apply Windows suggestion: the source text changed before writing"
                ),
                Err(error) => eprintln!("Unable to apply Windows suggestion: {error}"),
            }
        }
        Err(error) => {
            eprintln!("Unable to determine whether the Windows text element is writable: {error}");
        }
    }

    JobResult::None
}

fn apply_suggestion_to_text(
    current_text: &str,
    expected_text: &str,
    span: Span<char>,
    suggestion: &Suggestion,
) -> std::result::Result<String, &'static str> {
    if current_text != expected_text {
        return Err("the source text changed after linting");
    }

    let mut chars = current_text.chars().collect::<Vec<_>>();
    if span.end > chars.len() {
        return Err("the lint span is outside the source text");
    }

    suggestion.apply(span, &mut chars);
    Ok(chars.into_iter().collect())
}

/// Selects the span the suggestion targets and types the replacement.
///
/// Every step that can observe the provider's state verifies it before anything is sent, so the
/// worst case is "nothing happened", never "the wrong text changed".
fn apply_via_selection(
    automation: &UIAutomation,
    element: &UIElement,
    window: isize,
    current_text: &str,
    span: Span<char>,
    suggestion: &Suggestion,
) -> std::result::Result<(), SelectionApplyError> {
    use SelectionApplyError::{Aborted, Unavailable};

    let pattern: UITextPattern = element
        .get_pattern()
        .map_err(|_| Unavailable("no TextPattern".to_string()))?;

    let (start, len, typed): (usize, usize, String) = match suggestion {
        Suggestion::ReplaceWith(chars) => (span.start, span.len(), chars.iter().collect()),
        Suggestion::InsertAfter(chars) => (span.end, 0, chars.iter().collect()),
        Suggestion::Remove => (span.start, span.len(), String::new()),
    };

    let range = range_for_span(&pattern, start as i32, len as i32)
        .map_err(|error| Unavailable(format!("could not build a range: {error}")))?;

    let expected_slice: String = current_text.chars().skip(start).take(len).collect();
    let actual_slice = range
        .get_text(-1)
        .map_err(|error| Unavailable(format!("range text unreadable: {error}")))?;
    if actual_slice != expected_slice {
        return Err(Aborted(format!(
            "range reads {actual_slice:?}, expected {expected_slice:?}"
        )));
    }

    // The user just clicked Harper's overlay, so the target may have lost keyboard focus.
    return_focus_to(automation, element, window)?;

    range
        .select()
        .map_err(|error| Unavailable(format!("Select failed: {error}")))?;

    let selected = pattern
        .get_selection()
        .ok()
        .and_then(|ranges| ranges.into_iter().next())
        .and_then(|range| range.get_text(-1).ok())
        .unwrap_or_default();
    if selected != expected_slice {
        return Err(Aborted(format!(
            "selection reads {selected:?} after Select(), expected {expected_slice:?}"
        )));
    }

    if !wait_for_modifiers_released(MODIFIER_RELEASE_TIMEOUT) {
        return Err(Aborted("modifier keys still held".to_string()));
    }

    if typed.is_empty() {
        send_virtual_key(VK_DELETE);
    } else {
        send_unicode_text(&typed);
    }

    Ok(())
}

/// Brings `window` forward and waits until `element` reports keyboard focus again.
fn return_focus_to(
    automation: &UIAutomation,
    element: &UIElement,
    window: isize,
) -> std::result::Result<(), SelectionApplyError> {
    let target_id = element
        .get_runtime_id()
        .map_err(|error| SelectionApplyError::Unavailable(format!("no runtime id: {error}")))?;

    let already_focused = || {
        automation
            .get_focused_element()
            .and_then(|focused| focused.get_runtime_id())
            .map(|id| id == target_id)
            .unwrap_or(false)
    };

    if already_focused() {
        return Ok(());
    }

    unsafe {
        let _ = SetForegroundWindow(HWND(window as *mut std::ffi::c_void));
    }
    let _ = element.set_focus();

    let deadline = Instant::now() + FOCUS_RETURN_TIMEOUT;
    while Instant::now() < deadline {
        if already_focused() {
            return Ok(());
        }
        sleep(Duration::from_millis(15));
    }

    Err(SelectionApplyError::Aborted(
        "the target field did not regain keyboard focus".to_string(),
    ))
}

/// A range covering `len` characters starting at character offset `start` of the document.
fn range_for_span(
    pattern: &UITextPattern,
    start: i32,
    len: i32,
) -> uiautomation::Result<UITextRange> {
    let range = pattern.get_document_range()?;

    range.move_endpoint_by_range(
        TextPatternRangeEndpoint::End,
        &range,
        TextPatternRangeEndpoint::Start,
    )?;
    range.move_endpoint_by_unit(TextPatternRangeEndpoint::Start, TextUnit::Character, start)?;
    range.move_endpoint_by_range(
        TextPatternRangeEndpoint::End,
        &range,
        TextPatternRangeEndpoint::Start,
    )?;
    range.move_endpoint_by_unit(TextPatternRangeEndpoint::End, TextUnit::Character, len)?;

    Ok(range)
}

fn wait_for_modifiers_released(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let held = unsafe {
            GetAsyncKeyState(VK_CONTROL.0 as i32) < 0
                || GetAsyncKeyState(VK_MENU.0 as i32) < 0
                || GetAsyncKeyState(VK_SHIFT.0 as i32) < 0
        };
        if !held {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(15));
    }
}

/// Types `text` into the focused control as Unicode key events, independent of keyboard layout.
fn send_unicode_text(text: &str) {
    let mut inputs = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
            inputs.push(keyboard_input(VIRTUAL_KEY(0), unit, flags));
        }
    }
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

fn send_virtual_key(key: VIRTUAL_KEY) {
    let inputs = [
        keyboard_input(key, 0, KEYBD_EVENT_FLAGS(0)),
        keyboard_input(key, 0, KEYEVENTF_KEYUP),
    ];
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

fn keyboard_input(key: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

// ---------------------------------------------------------------------------
// Finding and reading the focused text field
// ---------------------------------------------------------------------------

fn get_text(element: &UIElement) -> uiautomation::Result<String> {
    let pattern: UITextPattern = element.get_pattern()?;
    let range = pattern.get_document_range()?;
    range.get_text(-1)
}

/// Whether Harper should read from and write to this element at all.
///
/// Password fields are never touched. Beyond that, the element must expose `TextPattern` and be
/// editable: a writable `ValuePattern` is the strongest signal. Chromium reports the whole page as
/// a `Document` with a read-only `ValuePattern` whenever focus is not inside a field, and this
/// check is what keeps Harper from linting an entire web page.
fn is_lintable_text_element(element: &UIElement) -> bool {
    if element.is_password().unwrap_or(true) {
        return false;
    }

    if element.get_pattern::<UITextPattern>().is_err() {
        return false;
    }

    match element.get_pattern::<UIValuePattern>() {
        Ok(value) => matches!(value.is_readonly(), Ok(false)),
        Err(_) => matches!(
            element.get_control_type(),
            Ok(ControlType::Edit | ControlType::Document)
        ),
    }
}

fn element_text_matches(element: &UIElement, expected_text: Option<&str>) -> bool {
    match expected_text {
        None => true,
        Some(expected) => matches!(get_text(element), Ok(text) if text == expected),
    }
}

/// Finds the focused text element below `window`.
///
/// The focused element is tried first because UIA can answer that in one call, where a subtree
/// search over a browser's accessibility tree is slow. The search remains as a fallback for
/// providers whose focused element is a container around the real text control.
///
/// When `expected_text` is provided, unrelated text providers are excluded.
fn text_element_for_window(
    automation: &UIAutomation,
    window: isize,
    expected_text: Option<&str>,
) -> uiautomation::Result<UIElement> {
    // Keyboard focus is on exactly one element, and UIA can name it in one call. When it does,
    // that answer is final: a focused button or link means there is no field to lint, and
    // searching the window's whole subtree for one would only burn time. In a browser that
    // search takes seconds per call and, run from the highlighter's event loop, left the
    // overlay stuck before its first frame (an opaque white window over the screen).
    if let Ok(focused) = automation.get_focused_element() {
        if element_belongs_to_window(&focused, window)
            && is_lintable_text_element(&focused)
            && element_text_matches(&focused, expected_text)
        {
            return Ok(focused);
        }
        return Err(Error::new(
            uiautomation::errors::ERR_NOTFOUND,
            "the focused element is not a lintable text field",
        ));
    }

    // Fallback only when UIA cannot report the focused element at all.
    let root = automation.element_from_handle(Handle::from(window))?;
    let text_condition = automation.create_property_condition(
        UIProperty::IsTextPatternAvailable,
        Variant::from(true),
        None,
    )?;
    let keyboard_condition = automation.create_property_condition(
        UIProperty::HasKeyboardFocus,
        Variant::from(true),
        None,
    )?;
    let condition = automation.create_and_condition(text_condition, keyboard_condition)?;

    let element = root.find_first(TreeScope::Subtree, &condition)?;
    if is_lintable_text_element(&element) && element_text_matches(&element, expected_text) {
        return Ok(element);
    }

    Err(Error::new(
        uiautomation::errors::ERR_NOTFOUND,
        "no text element found",
    ))
}

/// The focused element often has no window handle of its own (every Chromium control reports
/// `0`), so ownership is checked by process instead.
fn element_belongs_to_window(element: &UIElement, window: isize) -> bool {
    let mut window_process_id = 0;
    unsafe {
        GetWindowThreadProcessId(
            HWND(window as *mut std::ffi::c_void),
            Some(&mut window_process_id),
        );
    }
    window_process_id != 0 && element.get_process_id().ok() == Some(window_process_id)
}

thread_local! {
    /// The text element the last successful read used, with its window. Lives on the worker
    /// thread because UIA element wrappers belong to the apartment that created them.
    static LAST_TEXT_ELEMENT: std::cell::RefCell<Option<(isize, UIElement)>> =
        const { std::cell::RefCell::new(None) };
}

/// The remembered element for `window`, if it still reads exactly `expected_text`.
fn cached_text_element(window: isize, expected_text: &str) -> Option<UIElement> {
    LAST_TEXT_ELEMENT.with(|cell| {
        let cached = cell.borrow();
        let (cached_window, element) = cached.as_ref()?;
        if *cached_window != window {
            return None;
        }
        element_text_matches(element, Some(expected_text)).then(|| element.clone())
    })
}

/// Finds a lintable text element in `window` that reads exactly `expected_text`, with no
/// requirement that it has keyboard focus. Slow in large trees; used only when applying.
fn search_text_element(
    automation: &UIAutomation,
    window: isize,
    expected_text: &str,
) -> uiautomation::Result<UIElement> {
    let root = automation.element_from_handle(Handle::from(window))?;
    let condition = automation.create_property_condition(
        UIProperty::IsTextPatternAvailable,
        Variant::from(true),
        None,
    )?;
    for element in root.find_all(TreeScope::Subtree, &condition)? {
        if is_lintable_text_element(&element) && element_text_matches(&element, Some(expected_text))
        {
            return Ok(element);
        }
    }
    Err(Error::new(
        uiautomation::errors::ERR_NOTFOUND,
        "no text element with the linted text found",
    ))
}

fn get_text_job(automation: &UIAutomation, args: Vec<JobArgument>) -> JobResult {
    let Some(JobArgument::Window(window)) = args.first() else {
        return JobResult::Err;
    };
    let Ok(element) = text_element_for_window(automation, *window, None) else {
        return JobResult::Err;
    };

    let result = get_text(&element).map_or(JobResult::Err, JobResult::String);
    if matches!(result, JobResult::String(_)) {
        LAST_TEXT_ELEMENT.with(|cell| *cell.borrow_mut() = Some((*window, element)));
    }
    result
}

// ---------------------------------------------------------------------------
// Bounding rectangles
// ---------------------------------------------------------------------------

use std::{ffi::c_void, mem::size_of};

use uiautomation::{Error, Result};
use windows::Win32::System::{
    Com::SAFEARRAY,
    Ole::{
        SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetElement, SafeArrayGetElemsize,
        SafeArrayGetLBound, SafeArrayGetUBound,
    },
};

struct OwnedSafeArray(*mut SAFEARRAY);

impl Drop for OwnedSafeArray {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = SafeArrayDestroy(self.0);
            }
        }
    }
}

fn bounding_rectangles_for_span(
    element: &UIElement,
    start: i32,
    len: i32,
) -> Result<Vec<(f64, f64, f64, f64)>> {
    if start < 0 || len < 0 {
        return Err(Error::new(
            uiautomation::errors::ERR_INVALID_ARG,
            "start and len must be non-negative",
        ));
    }

    let pattern: UITextPattern = element.get_pattern()?;
    let range = range_for_span(&pattern, start, len)?;

    let raw: &IUIAutomationTextRange = range.as_ref();
    let array = OwnedSafeArray(unsafe { raw.GetBoundingRectangles()? });

    if array.0.is_null() {
        return Ok(Vec::new());
    }

    let dim = unsafe { SafeArrayGetDim(array.0) };

    if dim != 1 {
        return Err(Error::new(
            uiautomation::errors::ERR_FORMAT,
            "bounding rectangles SAFEARRAY is not one-dimensional",
        ));
    }

    let elem_size = unsafe { SafeArrayGetElemsize(array.0) };

    if elem_size as usize != size_of::<f64>() {
        return Err(Error::new(
            uiautomation::errors::ERR_FORMAT,
            "bounding rectangles SAFEARRAY does not contain f64-sized elements",
        ));
    }

    let lower = unsafe { SafeArrayGetLBound(array.0, 1)? };
    let upper = unsafe { SafeArrayGetUBound(array.0, 1)? };

    if upper < lower {
        return Ok(Vec::new());
    }

    let count = usize::try_from(i64::from(upper) - i64::from(lower) + 1)
        .map_err(|_| Error::new(uiautomation::errors::ERR_FORMAT, "SAFEARRAY is too large"))?;

    if count % 4 != 0 {
        return Err(Error::new(
            uiautomation::errors::ERR_FORMAT,
            "bounding rectangles SAFEARRAY length is not divisible by four",
        ));
    }

    let mut result = Vec::with_capacity(count / 4);

    for rect in 0..count / 4 {
        let mut values = [0.0_f64; 4];

        for (component, value) in values.iter_mut().enumerate() {
            let offset = rect
                .checked_mul(4)
                .and_then(|n| n.checked_add(component))
                .ok_or_else(|| {
                    Error::new(uiautomation::errors::ERR_FORMAT, "SAFEARRAY index overflow")
                })?;

            let index = i64::from(lower)
                .checked_add(i64::try_from(offset).map_err(|_| {
                    Error::new(uiautomation::errors::ERR_FORMAT, "SAFEARRAY index overflow")
                })?)
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| {
                    Error::new(uiautomation::errors::ERR_FORMAT, "SAFEARRAY index overflow")
                })?;

            unsafe {
                SafeArrayGetElement(array.0, &index, value as *mut f64 as *mut c_void)?;
            }
        }

        result.push((values[0], values[1], values[2], values[3]));
    }

    Ok(result)
}

fn get_bounding_rect_job(automation: &UIAutomation, arguments: Vec<JobArgument>) -> JobResult {
    let Some(JobArgument::Window(window)) = arguments.first() else {
        return JobResult::Err;
    };
    let Some(JobArgument::Text(expected_text)) = arguments.get(1) else {
        return JobResult::Err;
    };
    let Ok(text_element) = text_element_for_window(automation, *window, Some(expected_text)) else {
        return JobResult::Err;
    };

    let effective_monitor_scale = get_focused_monitor_scale();

    let mut rects = Vec::with_capacity(arguments.len().saturating_sub(2));

    for span in arguments.into_iter().skip(2) {
        let span = span.expect_span();

        let Ok(found_rects) =
            bounding_rectangles_for_span(&text_element, span.start as i32, span.len() as i32)
        else {
            return JobResult::Err;
        };

        rects.push(
            found_rects
                .iter()
                .map(|(x, y, w, h)| {
                    Rect::new(
                        *x / effective_monitor_scale,
                        *y / effective_monitor_scale,
                        *w / effective_monitor_scale,
                        *h / effective_monitor_scale,
                    )
                })
                .collect(),
        );
    }

    JobResult::GroupedRects(rects)
}

fn focused_window() -> Option<(isize, u32)> {
    let hwnd: HWND = unsafe { GetForegroundWindow() };

    if hwnd.0.is_null() {
        return None;
    }

    let mut process_id = 0;

    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
    }

    Some((hwnd.0 as isize, process_id))
}
