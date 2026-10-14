mod commands;
mod editor;
mod overlay;

use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

use editor::EditorModel;
use glyphon::{
    Attrs, Buffer, Cache, Color, Cursor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use overlay::{OverlayRect, OverlayRenderer};
use vanta_commands::{CommandContext, CommandId, CommandRegistry};
use wgpu::{
    CommandEncoderDescriptor, DeviceDescriptor, Instance, InstanceDescriptor, LoadOp,
    MultisampleState, Operations, RenderPassColorAttachment, RenderPassDescriptor,
    RequestAdapterOptions, SurfaceConfiguration, TextureViewDescriptor,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

const BACKGROUND: wgpu::Color = wgpu::Color {
    r: 0.035,
    g: 0.043,
    b: 0.059,
    a: 1.0,
};
const TEXT_COLOR: Color = Color::rgb(220, 223, 228);
const PADDING: f32 = 20.0;

fn main() -> Result<(), Box<dyn Error>> {
    let initial_path = std::env::args_os().nth(1).map(PathBuf::from);
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut Application::new(initial_path))?;
    Ok(())
}

struct Application {
    initial_path: Option<PathBuf>,
    window_state: Option<WindowState>,
}

impl Application {
    const fn new(initial_path: Option<PathBuf>) -> Self {
        Self {
            initial_path,
            window_state: None,
        }
    }
}

impl ApplicationHandler for Application {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window_state.is_some() {
            return;
        }

        let model = match self.initial_path.as_deref() {
            Some(path) => match EditorModel::open(path) {
                Ok(model) => model,
                Err(error) => {
                    eprintln!("vanta: failed to open {}: {error}", path.display());
                    event_loop.exit();
                    return;
                }
            },
            None => EditorModel::untitled(),
        };
        let attributes = Window::default_attributes()
            .with_title(model.title())
            .with_inner_size(LogicalSize::new(960.0, 640.0))
            .with_min_inner_size(LogicalSize::new(480.0, 320.0));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("vanta: failed to create window: {error}");
                event_loop.exit();
                return;
            }
        };

        match pollster::block_on(WindowState::new(window, event_loop, model)) {
            Ok(state) => self.window_state = Some(state),
            Err(error) => {
                eprintln!("vanta: failed to initialize renderer: {error}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = &mut self.window_state else {
            return;
        };
        if state.window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                state.clipboard.take();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => state.resize(size.width, size.height),
            WindowEvent::ScaleFactorChanged { .. } => state.resize_buffer(),
            WindowEvent::ModifiersChanged(modifiers) => state.modifiers = modifiers.state(),
            WindowEvent::Ime(Ime::Commit(text)) => state.insert_text(&text),
            WindowEvent::CursorMoved { position, .. } => {
                state.pointer_moved(position.x, position.y);
            }
            WindowEvent::MouseInput {
                state: button_state,
                button: MouseButton::Left,
                ..
            } => state.mouse_button(button_state),
            WindowEvent::MouseWheel { delta, .. } => state.mouse_wheel(delta),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                state.keyboard_input(&event.logical_key, event.text.as_deref());
            }
            WindowEvent::RedrawRequested => state.render(),
            _ => {}
        }
    }
}

struct WindowState {
    instance: Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: SurfaceConfiguration,
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    overlay_renderer: OverlayRenderer,
    text_buffer: Buffer,
    model: EditorModel,
    command_registry: CommandRegistry,
    clipboard: Option<arboard::Clipboard>,
    modifiers: ModifiersState,
    pointer_position: (f32, f32),
    selecting: bool,
    window: Arc<Window>,
}

impl WindowState {
    async fn new(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        model: EditorModel,
    ) -> Result<Self, String> {
        let size = window.inner_size();
        let instance = Instance::new(InstanceDescriptor::new_with_display_handle(Box::new(
            event_loop.owned_display_handle(),
        )));
        let surface = instance
            .create_surface(Arc::clone(&window))
            .map_err(|error| error.to_string())?;
        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..RequestAdapterOptions::default()
            })
            .await
            .map_err(|error| error.to_string())?;
        let (device, queue) = adapter
            .request_device(&DeviceDescriptor::default())
            .await
            .map_err(|error| error.to_string())?;
        let surface_config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| "GPU surface has no supported configuration".to_owned())?;
        surface.configure(&device, &surface_config);

        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, surface_config.format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
        let overlay_renderer = OverlayRenderer::new(&device, surface_config.format);
        let mut text_buffer = Buffer::new(&mut font_system, Metrics::new(15.0, 22.0));
        Self::update_text_buffer(
            &mut text_buffer,
            &mut font_system,
            &model,
            size.width,
            size.height,
        );

        Ok(Self {
            instance,
            device,
            queue,
            surface,
            surface_config,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            overlay_renderer,
            text_buffer,
            model,
            command_registry: commands::built_in_registry().map_err(|error| error.to_string())?,
            clipboard: arboard::Clipboard::new().ok(),
            modifiers: ModifiersState::empty(),
            pointer_position: (PADDING, PADDING),
            selecting: false,
            window,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
        self.resize_buffer();
    }

    fn resize_buffer(&mut self) {
        Self::update_text_buffer(
            &mut self.text_buffer,
            &mut self.font_system,
            &self.model,
            self.surface_config.width,
            self.surface_config.height,
        );
        self.window.request_redraw();
    }

    // Window dimensions are bounded far below the precision limit that matters
    // for physical pixels, while glyphon intentionally consumes `f32` sizes.
    #[allow(clippy::cast_precision_loss)]
    fn update_text_buffer(
        buffer: &mut Buffer,
        font_system: &mut FontSystem,
        model: &EditorModel,
        width: u32,
        height: u32,
    ) {
        buffer.set_size(
            Some((width as f32 - PADDING * 2.0).max(1.0)),
            Some((height as f32 - PADDING * 2.0).max(1.0)),
        );
        buffer.set_text(
            model.text(),
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(font_system, false);
    }

    fn refresh_after_edit(&mut self) {
        self.window.set_title(&self.model.title());
        self.resize_buffer();
    }

    fn request_redraw(&self) {
        self.window.request_redraw();
    }

    fn insert_text(&mut self, text: &str) {
        if let Err(error) = self.model.insert(text) {
            eprintln!("vanta: edit rejected: {error}");
        } else {
            self.refresh_after_edit();
        }
    }

    fn keyboard_input(&mut self, key: &Key, text: Option<&str>) {
        let command_modifier = self.modifiers.super_key() || self.modifiers.control_key();
        if command_modifier
            && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("s"))
        {
            self.execute_command(commands::FILE_SAVE);
            return;
        }
        if command_modifier
            && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("a"))
        {
            self.execute_command(commands::EDITOR_SELECT_ALL);
            return;
        }
        if command_modifier
            && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("c"))
        {
            self.execute_command(commands::EDITOR_COPY);
            return;
        }
        if command_modifier
            && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("x"))
        {
            self.execute_command(commands::EDITOR_CUT);
            return;
        }
        if command_modifier
            && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("v"))
        {
            self.execute_command(commands::EDITOR_PASTE);
            return;
        }

        match key {
            Key::Named(NamedKey::Backspace) => self.execute_command(commands::EDITOR_BACKSPACE),
            Key::Named(NamedKey::Delete) => self.execute_command(commands::EDITOR_DELETE),
            Key::Named(NamedKey::ArrowLeft) => self.execute_command(commands::EDITOR_MOVE_LEFT),
            Key::Named(NamedKey::ArrowRight) => self.execute_command(commands::EDITOR_MOVE_RIGHT),
            Key::Named(NamedKey::Enter) => self.execute_command(commands::EDITOR_INSERT_NEWLINE),
            Key::Named(NamedKey::Tab) => self.execute_command(commands::EDITOR_INSERT_TAB),
            _ if !command_modifier => {
                if let Some(text) = text.filter(|text| !text.chars().any(char::is_control)) {
                    self.insert_text(text);
                }
            }
            _ => {}
        }
    }

    fn execute_command(&mut self, name: &str) {
        let id = CommandId::new(name).expect("built-in command IDs are validated during startup");
        let context =
            CommandContext::default().with_flag("editor.has_selection", self.model.has_selection());
        if !self.command_registry.is_enabled(&id, &context) {
            return;
        }

        match name {
            commands::FILE_SAVE => match self.model.save() {
                Ok(()) => self.refresh_after_edit(),
                Err(error) => eprintln!("vanta: save failed: {error}"),
            },
            commands::EDITOR_SELECT_ALL => {
                self.model.select_all();
                self.request_redraw();
            }
            commands::EDITOR_COPY => self.copy_selection(),
            commands::EDITOR_CUT => self.cut_selection(),
            commands::EDITOR_PASTE => self.paste(),
            commands::EDITOR_BACKSPACE => {
                if let Err(error) = self.model.backspace() {
                    eprintln!("vanta: edit rejected: {error}");
                } else {
                    self.refresh_after_edit();
                }
            }
            commands::EDITOR_DELETE => {
                if let Err(error) = self.model.delete_forward() {
                    eprintln!("vanta: edit rejected: {error}");
                } else {
                    self.refresh_after_edit();
                }
            }
            commands::EDITOR_MOVE_LEFT => {
                self.model.move_left(self.modifiers.shift_key());
                self.request_redraw();
            }
            commands::EDITOR_MOVE_RIGHT => {
                self.model.move_right(self.modifiers.shift_key());
                self.request_redraw();
            }
            commands::EDITOR_INSERT_NEWLINE => self.insert_text("\n"),
            commands::EDITOR_INSERT_TAB => self.insert_text("\t"),
            _ => eprintln!("vanta: command `{name}` has no dispatcher"),
        }
    }

    fn copy_selection(&mut self) {
        let Some(text) = self.model.selected_text() else {
            return;
        };
        let Some(clipboard) = &mut self.clipboard else {
            eprintln!("vanta: clipboard is unavailable");
            return;
        };
        if let Err(error) = clipboard.set_text(text) {
            eprintln!("vanta: copy failed: {error}");
        }
    }

    fn cut_selection(&mut self) {
        if self.model.selected_text().is_none() {
            return;
        }
        self.copy_selection();
        if let Err(error) = self.model.delete_selection() {
            eprintln!("vanta: cut edit rejected: {error}");
        } else {
            self.refresh_after_edit();
        }
    }

    fn paste(&mut self) {
        let Some(clipboard) = &mut self.clipboard else {
            eprintln!("vanta: clipboard is unavailable");
            return;
        };
        match clipboard.get_text() {
            Ok(text) => self.insert_text(&text),
            Err(error) => eprintln!("vanta: paste failed: {error}"),
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn pointer_moved(&mut self, x: f64, y: f64) {
        self.pointer_position = (x as f32, y as f32);
        if self.selecting {
            self.place_cursor(true);
        }
    }

    fn mouse_button(&mut self, state: ElementState) {
        self.selecting = state == ElementState::Pressed;
        if self.selecting {
            self.place_cursor(self.modifiers.shift_key());
        }
    }

    fn place_cursor(&mut self, extend: bool) {
        let (x, y) = self.pointer_position;
        let Some(cursor) = self.text_buffer.hit(x - PADDING, y - PADDING) else {
            return;
        };
        if let Err(error) = self.model.set_cursor_point(
            vanta_document::TextPoint {
                line: cursor.line,
                byte_column: cursor.index,
            },
            extend,
        ) {
            eprintln!("vanta: pointer position rejected: {error}");
        }
        self.request_redraw();
    }

    #[allow(clippy::cast_possible_truncation)]
    fn mouse_wheel(&mut self, delta: MouseScrollDelta) {
        let pixels = match delta {
            MouseScrollDelta::LineDelta(_, lines) => lines * 22.0,
            MouseScrollDelta::PixelDelta(position) => position.y as f32,
        };
        let mut scroll = self.text_buffer.scroll();
        scroll.vertical -= pixels;
        self.text_buffer.set_scroll(scroll);
        self.text_buffer
            .shape_until_scroll(&mut self.font_system, false);
        self.request_redraw();
    }

    fn shaped_cursor(&self) -> Cursor {
        let point = self.model.cursor_point();
        Cursor::new(point.line, point.byte_column)
    }

    fn selection_rectangles(&self) -> Vec<OverlayRect> {
        let Some((start, end)) = self.model.selection_points() else {
            return Vec::new();
        };
        let start = Cursor::new(start.line, start.byte_column);
        let end = Cursor::new(end.line, end.byte_column);
        self.text_buffer
            .layout_runs()
            .flat_map(|run| {
                run.highlight(start, end)
                    .map(move |(left, width)| OverlayRect {
                        left: PADDING + left,
                        top: PADDING + run.line_top,
                        width,
                        height: run.line_height,
                    })
            })
            .collect()
    }

    fn caret_rectangle(&self) -> Option<OverlayRect> {
        let (left, top) = self.text_buffer.cursor_position(&self.shaped_cursor())?;
        Some(OverlayRect {
            left: PADDING + left,
            top: PADDING + top,
            width: 2.0,
            height: 22.0,
        })
    }

    fn render(&mut self) {
        if !self.prepare_text() {
            return;
        }

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
                self.surface.configure(&self.device, &self.surface_config);
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                match self.instance.create_surface(Arc::clone(&self.window)) {
                    Ok(surface) => {
                        self.surface = surface;
                        self.surface.configure(&self.device, &self.surface_config);
                    }
                    Err(error) => eprintln!("vanta: failed to recreate GPU surface: {error}"),
                }
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                eprintln!("vanta: GPU surface validation failed");
                return;
            }
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let selection_rectangles = self.selection_rectangles();
        let caret_rectangle = self.caret_rectangle();
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("Vanta frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("Vanta editor"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(BACKGROUND),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.overlay_renderer.draw_selection(
                &mut pass,
                &selection_rectangles,
                self.surface_config.width,
                self.surface_config.height,
            );
            if let Err(error) = self
                .text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
            {
                eprintln!("vanta: text rendering failed: {error}");
                return;
            }
            if let Some(caret_rectangle) = caret_rectangle {
                self.overlay_renderer.draw_caret(
                    &mut pass,
                    caret_rectangle,
                    self.surface_config.width,
                    self.surface_config.height,
                );
            }
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.atlas.trim();
    }

    fn prepare_text(&mut self) -> bool {
        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.surface_config.width,
                height: self.surface_config.height,
            },
        );
        let right = i32::try_from(self.surface_config.width).unwrap_or(i32::MAX);
        let bottom = i32::try_from(self.surface_config.height).unwrap_or(i32::MAX);
        if let Err(error) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            [TextArea {
                buffer: &self.text_buffer,
                left: PADDING,
                top: PADDING,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right,
                    bottom,
                },
                default_color: TEXT_COLOR,
                custom_glyphs: &[],
            }],
            &mut self.swash_cache,
        ) {
            eprintln!("vanta: text preparation failed: {error}");
            return false;
        }
        true
    }
}
