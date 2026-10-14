mod editor;

use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

use editor::EditorModel;
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use wgpu::{
    CommandEncoderDescriptor, DeviceDescriptor, Instance, InstanceDescriptor, LoadOp,
    MultisampleState, Operations, RenderPassColorAttachment, RenderPassDescriptor,
    RequestAdapterOptions, SurfaceConfiguration, TextureViewDescriptor,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, WindowEvent};
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
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => state.resize(size.width, size.height),
            WindowEvent::ScaleFactorChanged { .. } => state.resize_buffer(),
            WindowEvent::ModifiersChanged(modifiers) => state.modifiers = modifiers.state(),
            WindowEvent::Ime(Ime::Commit(text)) => state.insert_text(&text),
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
    text_buffer: Buffer,
    model: EditorModel,
    modifiers: ModifiersState,
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
            text_buffer,
            model,
            modifiers: ModifiersState::empty(),
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
            match self.model.save() {
                Ok(()) => self.refresh_after_edit(),
                Err(error) => eprintln!("vanta: save failed: {error}"),
            }
            return;
        }

        match key {
            Key::Named(NamedKey::Backspace) => {
                if let Err(error) = self.model.backspace() {
                    eprintln!("vanta: edit rejected: {error}");
                } else {
                    self.refresh_after_edit();
                }
            }
            Key::Named(NamedKey::ArrowLeft) => self.model.move_left(),
            Key::Named(NamedKey::ArrowRight) => self.model.move_right(),
            Key::Named(NamedKey::Enter) => self.insert_text("\n"),
            Key::Named(NamedKey::Tab) => self.insert_text("\t"),
            _ if !command_modifier => {
                if let Some(text) = text.filter(|text| !text.chars().any(char::is_control)) {
                    self.insert_text(text);
                }
            }
            _ => {}
        }
    }

    fn render(&mut self) {
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
            if let Err(error) = self
                .text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
            {
                eprintln!("vanta: text rendering failed: {error}");
                return;
            }
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.atlas.trim();
    }
}
