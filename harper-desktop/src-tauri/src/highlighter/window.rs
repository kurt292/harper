use std::num::NonZeroU32;
use std::sync::Arc;

use egui_wgpu::wgpu::PresentMode;
use egui_wgpu::winit::Painter;
use egui_wgpu::{RendererOptions, WgpuConfiguration, WgpuSetup};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
use winit::window::{Window as WinitWindow, WindowButtons, WindowId, WindowLevel};

use super::Error;
use super::render_state::RenderState;

/// A transparent click-through overlay window for one monitor.
///
/// `Window` owns the native winit window plus egui/wgpu integration required to render into it. It
/// deliberately does not own highlighter drawing decisions; those are supplied by `RenderState`
/// during each redraw.
pub struct Window {
    inner: Arc<WinitWindow>,
    egui_state: egui_winit::State,
    painter: Painter,
    viewport_id: egui::ViewportId,
    frames_presented: u64,
    /// When this window last presented a frame. See `WindowManager` for why other windows'
    /// redraws may trigger this one.
    last_render: Option<std::time::Instant>,
    /// Monitor name, for diagnostics.
    label: String,
    /// The monitor's physical size. Re-asserted whenever Windows rescales the window after a
    /// DPI change, which otherwise leaves a secondary-monitor overlay at the wrong size.
    monitor_size: PhysicalSize<u32>,
}

impl Window {
    pub async fn new(
        event_loop: &ActiveEventLoop,
        monitor: MonitorHandle,
        context: egui::Context,
    ) -> Result<Self, Error> {
        let position = monitor.position();
        // One pixel shorter than the monitor on purpose. A borderless always-on-top window that
        // covers a monitor exactly is eligible for the compositor's direct-scanout path, which
        // ignores transparency (an opaque block) and blanks the display on every transition.
        let monitor_size = monitor.size();
        let size = PhysicalSize::new(monitor_size.width, monitor_size.height.saturating_sub(1));
        let window = Arc::new(
            event_loop.create_window(
                WinitWindow::default_attributes()
                    .with_title("Harper")
                    .with_inner_size(size)
                    .with_position(position)
                    .with_resizable(false)
                    .with_enabled_buttons(WindowButtons::empty())
                    .with_decorations(false)
                    .with_transparent(true)
                    .with_window_level(WindowLevel::AlwaysOnTop)
                    .with_active(false),
            )?,
        );

        eprintln!(
            "overlay window created for monitor {:?}: requested {}x{} at {},{}, actual inner {:?}, scale {}",
            monitor.name(),
            size.width,
            size.height,
            position.x,
            position.y,
            window.inner_size(),
            window.scale_factor()
        );
        window.set_outer_position(PhysicalPosition::new(position.x, position.y));
        let _ = window.request_inner_size(PhysicalSize::new(size.width, size.height));
        window.set_cursor_hittest(false)?;
        let viewport_id = egui::ViewportId::from_hash_of(window.id());

        let egui_state = egui_winit::State::new(
            context.clone(),
            viewport_id,
            event_loop,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );

        let mut painter = Painter::new(
            context,
            WgpuConfiguration {
                present_mode: PresentMode::Fifo,
                wgpu_setup: WgpuSetup::from_display_handle(event_loop.owned_display_handle()),
                ..Default::default()
            },
            true,
            RendererOptions::default(),
        )
        .await;
        painter
            .set_window(viewport_id, Some(window.clone()))
            .await?;
        window.request_redraw();

        Ok(Self {
            inner: window,
            egui_state,
            painter,
            viewport_id,
            frames_presented: 0,
            last_render: None,
            label: monitor.name().unwrap_or_else(|| "unnamed monitor".to_string()),
            monitor_size: PhysicalSize::new(size.width, size.height),
        })
    }

    pub fn id(&self) -> WindowId {
        self.inner.id()
    }

    pub fn request_redraw(&self) {
        self.inner.request_redraw();
    }

    /// Controls whether the transparent overlay can receive pointer events.
    ///
    /// Highlight windows are click-through by default so the underlying app remains usable. The
    /// window manager temporarily enables hit-testing when global cursor polling shows the pointer
    /// is over an interactive highlight or popup.
    pub fn set_cursor_hittest(&self, enabled: bool) -> Result<(), Error> {
        self.inner.set_cursor_hittest(enabled)?;

        Ok(())
    }

    pub fn handle_event(&mut self, event: &WindowEvent) {
        let response = self.egui_state.on_window_event(&self.inner, event);

        if response.repaint {
            self.inner.request_redraw();
        }

        if let WindowEvent::ScaleFactorChanged {
            scale_factor,
            inner_size_writer,
        } = event
        {
            // Keep the overlay at the monitor's physical size instead of letting Windows scale
            // it by the new DPI ratio.
            let mut writer = inner_size_writer.clone();
            let result = writer.request_inner_size(self.monitor_size);
            eprintln!(
                "overlay {}: scale factor changed to {scale_factor}, re-requesting {}x{} ({result:?})",
                self.label, self.monitor_size.width, self.monitor_size.height
            );
        }

        if let WindowEvent::Resized(size) = event
            && let (Some(width), Some(height)) =
                (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        {
            eprintln!("overlay {}: resized to {}x{}", self.label, size.width, size.height);
            self.painter
                .on_window_resized(self.viewport_id, width, height);
            self.inner.request_redraw();
        }
    }

    /// Renders unless a frame was presented within `max_age`.
    pub fn render_if_stale(&mut self, render_state: &mut RenderState, max_age: std::time::Duration) {
        let fresh = self
            .last_render
            .is_some_and(|last| last.elapsed() < max_age);
        if !fresh {
            self.render(render_state);
        }
    }

    pub fn render(&mut self, render_state: &mut RenderState) {
        let context = self.egui_state.egui_ctx().clone();
        let input = self.egui_state.take_egui_input(&self.inner);
        // This window's top-left in the broker's coordinate space (physical pixels divided by the
        // monitor scale), so a second-monitor overlay draws relative to itself.
        let scale = self.inner.scale_factor();
        let origin = self
            .inner
            .outer_position()
            .map(|position| (position.x as f64 / scale, position.y as f64 / scale))
            .unwrap_or((0.0, 0.0));
        let output = context.run_ui(input, |ui| {
            render_state.render(ui, origin);
        });

        self.egui_state
            .handle_platform_output(&self.inner, output.platform_output);

        let clipped_primitives = context.tessellate(output.shapes, output.pixels_per_point);
        self.painter.paint_and_update_textures(
            self.viewport_id,
            output.pixels_per_point,
            [0.0, 0.0, 0.0, 0.0],
            &clipped_primitives,
            &output.textures_delta,
            Vec::new(),
        );

        self.frames_presented += 1;
        self.last_render = Some(std::time::Instant::now());
        if self.frames_presented == 1 {
            eprintln!(
                "overlay {}: frame {} presented, inner {:?}, pixels_per_point {}",
                self.label,
                self.frames_presented,
                self.inner.inner_size(),
                output.pixels_per_point
            );
        }
    }
}
