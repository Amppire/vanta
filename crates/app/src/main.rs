mod accessibility;
mod commands;
mod editor;
mod overlay;
mod palette;

use std::error::Error;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use accesskit_winit::Adapter as AccessibilityAdapter;
use editor::EditorModel;
use glyphon::{
    Attrs, Buffer, Cache, Color, Cursor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use overlay::{OverlayRect, OverlayRenderer};
use palette::CommandPalette;
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use vanta_commands::{CommandContext, CommandId, CommandRegistry};
use vanta_config::{ExtensionManager, ExtensionPackage, InstallOutcome, Profile, SDK_VERSION};
use vanta_file_io::RecoveryJournal;
use vanta_language::SyntaxDocument;
use vanta_lsp::{
    BrokeredProcess, Diagnostic, DiagnosticSeverity, DiagnosticStore, LanguageFeature,
    NativeProcessBroker, RequestTracker, ServerCommand, ServerState, ServerSupervisor,
    Utf16Position, Utf16Range, VersionedTextEdit, WorkspaceEdit,
};
use vanta_session::{DocumentState, Session};
use vanta_workspace::{CancellationToken, Workspace};
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
const MUTED_TEXT_COLOR: Color = Color::rgb(157, 166, 181);
const PADDING: f32 = 20.0;
const TAB_BAR_HEIGHT: f32 = 36.0;
const STATUS_BAR_HEIGHT: f32 = 26.0;
const SIDEBAR_WIDTH: f32 = 240.0;
const TAB_WIDTH: f32 = 168.0;
const SIDEBAR_LINE_HEIGHT: f32 = 21.0;
const SIDEBAR_FILE_LIMIT: usize = 18;
#[cfg(target_os = "macos")]
const COMMAND_KEY: &str = "⌘";
#[cfg(not(target_os = "macos"))]
const COMMAND_KEY: &str = "Ctrl+";

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

    fn session_path() -> PathBuf {
        std::env::temp_dir().join("vanta-v0.3-session")
    }

    fn restore_session() -> Option<Session> {
        let report = Session::load(&Self::session_path()).ok()?;
        if report.discarded_records > 0 {
            eprintln!(
                "vanta: restored session with {} discarded record(s)",
                report.discarded_records
            );
        }
        Some(report.session)
    }

    fn recovery_journal() -> RecoveryJournal {
        RecoveryJournal::new(std::env::temp_dir().join("vanta-v0.3-recovery"))
    }

    fn restore_recovery() -> Option<EditorModel> {
        let journal = Self::recovery_journal();
        for entry in journal.entries().ok()? {
            let record = match journal.read(&entry) {
                Ok(record) => record,
                Err(error) => {
                    eprintln!("vanta: ignoring invalid recovery record: {error}");
                    continue;
                }
            };
            let choice = MessageDialog::new()
                .set_level(MessageLevel::Warning)
                .set_title("Recover unsaved document")
                .set_description("Vanta found edits from an interrupted session. Restore them?")
                .set_buttons(MessageButtons::YesNo)
                .show();
            if choice == MessageDialogResult::Yes {
                return Some(EditorModel::from_recovery(record));
            }
            if let Err(error) = journal.remove(&record.document_id) {
                eprintln!("vanta: failed to discard recovery record: {error}");
            }
        }
        None
    }
}

impl ApplicationHandler for Application {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window_state.is_some() {
            return;
        }

        let recovered_model = (self.initial_path.is_none())
            .then(Self::restore_recovery)
            .flatten();
        let restored_session = (self.initial_path.is_none() && recovered_model.is_none())
            .then(Self::restore_session)
            .flatten();
        let restored_path = restored_session.as_ref().and_then(|session| {
            session
                .documents
                .get(session.active)
                .map(|state| state.path.as_path())
        });
        let requested_path = self.initial_path.as_deref().or(restored_path);
        let model = recovered_model.unwrap_or_else(|| match requested_path {
            Some(path) => match EditorModel::open(path) {
                Ok(model) => model,
                Err(error) => {
                    eprintln!("vanta: failed to open {}: {error}", path.display());
                    EditorModel::untitled()
                }
            },
            None => EditorModel::untitled(),
        });
        let attributes = Window::default_attributes()
            .with_title(model.title())
            .with_visible(false)
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
            Ok(mut state) => {
                if let Some(session) = restored_session {
                    state.restore_session_state(session);
                }
                self.window_state = Some(state);
            }
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
        state.accessibility.process_event(&state.window, &event);

        match event {
            WindowEvent::CloseRequested => {
                if state.confirm_close() {
                    state.save_session();
                    state.clipboard.take();
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(size) => state.resize(size.width, size.height),
            WindowEvent::ScaleFactorChanged { .. } => state.resize_buffer(),
            WindowEvent::ModifiersChanged(modifiers) => state.modifiers = modifiers.state(),
            WindowEvent::Ime(Ime::Commit(text)) => {
                state.set_preedit("");
                state.insert_text(&text);
            }
            WindowEvent::Ime(Ime::Preedit(text, _)) => state.set_preedit(&text),
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

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = &mut self.window_state {
            state.poll_workspace_search();
            state.poll_language_server();
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
    tabs_buffer: Buffer,
    sidebar_buffer: Buffer,
    status_buffer: Buffer,
    palette_buffer: Buffer,
    ime_buffer: Buffer,
    ime_preedit: String,
    palette: Option<CommandPalette>,
    workspace: Option<Workspace>,
    recovery_journal: RecoveryJournal,
    model: EditorModel,
    tabs: Vec<EditorModel>,
    split_view: bool,
    sidebar_visible: bool,
    search_receiver: Option<Receiver<Vec<PathBuf>>>,
    search_cancellation: Option<CancellationToken>,
    command_registry: CommandRegistry,
    lsp_supervisor: ServerSupervisor,
    lsp_broker: NativeProcessBroker,
    lsp_process: Option<BrokeredProcess>,
    lsp_command: Option<ServerCommand>,
    lsp_receiver: Option<Receiver<LspEvent>>,
    lsp_requests: RequestTracker,
    lsp_log: Vec<String>,
    lsp_open_uri: Option<String>,
    diagnostics: DiagnosticStore,
    extension_manager: ExtensionManager,
    profile: Profile,
    pending_prompt: Option<PromptAction>,
    clipboard: Option<arboard::Clipboard>,
    modifiers: ModifiersState,
    pointer_position: (f32, f32),
    selecting: bool,
    accessibility: AccessibilityAdapter,
    window: Arc<Window>,
}

enum LspEvent {
    Message(serde_json::Value),
    Log(String),
    Closed,
}

enum PromptAction {
    Rename,
}

impl WindowState {
    #[allow(clippy::too_many_lines)]
    async fn new(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        model: EditorModel,
    ) -> Result<Self, String> {
        let size = window.inner_size();
        let accessibility = AccessibilityAdapter::with_direct_handlers(
            event_loop,
            &window,
            accessibility::InitialTree {
                text: model.text().to_owned(),
            },
            accessibility::IgnoreActions,
            accessibility::KeepTree,
        );
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
        let text_buffer = Buffer::new(&mut font_system, Metrics::new(15.0, 22.0));
        let tabs_buffer = Buffer::new(&mut font_system, Metrics::new(13.0, 20.0));
        let sidebar_buffer = Buffer::new(&mut font_system, Metrics::new(13.0, SIDEBAR_LINE_HEIGHT));
        let status_buffer = Buffer::new(&mut font_system, Metrics::new(12.0, 18.0));
        let mut palette_buffer = Buffer::new(&mut font_system, Metrics::new(15.0, 24.0));
        palette_buffer.set_size(Some(640.0), Some(240.0));
        let mut ime_buffer = Buffer::new(&mut font_system, Metrics::new(15.0, 22.0));
        ime_buffer.set_size(Some(480.0), Some(28.0));
        let mut state = Self {
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
            tabs_buffer,
            sidebar_buffer,
            status_buffer,
            palette_buffer,
            ime_buffer,
            ime_preedit: String::new(),
            palette: None,
            workspace: None,
            recovery_journal: Application::recovery_journal(),
            model,
            tabs: Vec::new(),
            split_view: false,
            sidebar_visible: true,
            search_receiver: None,
            search_cancellation: None,
            command_registry: commands::built_in_registry().map_err(|error| error.to_string())?,
            lsp_supervisor: ServerSupervisor::default(),
            lsp_broker: NativeProcessBroker::default(),
            lsp_process: None,
            lsp_command: None,
            lsp_receiver: None,
            lsp_requests: RequestTracker::default(),
            lsp_log: Vec::new(),
            lsp_open_uri: None,
            diagnostics: DiagnosticStore::default(),
            extension_manager: ExtensionManager::default(),
            profile: Profile::empty("Default"),
            pending_prompt: None,
            clipboard: arboard::Clipboard::new().ok(),
            modifiers: ModifiersState::empty(),
            pointer_position: (PADDING, PADDING),
            selecting: false,
            accessibility,
            window,
        };
        state.refresh_chrome();
        state.resize_buffer();
        state.window.set_visible(true);
        Ok(state)
    }

    fn editor_left(&self) -> f32 {
        if self.sidebar_visible {
            SIDEBAR_WIDTH + PADDING
        } else {
            PADDING
        }
    }

    fn editor_top() -> f32 {
        TAB_BAR_HEIGHT + PADDING
    }

    #[allow(clippy::cast_precision_loss)]
    fn editor_size(&self) -> (f32, f32) {
        let sidebar = if self.sidebar_visible {
            SIDEBAR_WIDTH
        } else {
            0.0
        };
        let width = (self.surface_config.width as f32 - sidebar - PADDING * 2.0).max(1.0);
        let height = (self.surface_config.height as f32
            - TAB_BAR_HEIGHT
            - STATUS_BAR_HEIGHT
            - PADDING * 2.0)
            .max(1.0);
        (width, height)
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
        let (mut width, height) = self.editor_size();
        if self.split_view {
            width = (width - PADDING) / 2.0;
        }
        Self::update_text_buffer(
            &mut self.text_buffer,
            &mut self.font_system,
            &self.model,
            width,
            height,
        );
        self.refresh_chrome();
        self.window.request_redraw();
    }

    // Window dimensions are bounded far below the precision limit that matters
    // for physical pixels, while glyphon intentionally consumes `f32` sizes.
    #[allow(clippy::cast_precision_loss)]
    fn update_text_buffer(
        buffer: &mut Buffer,
        font_system: &mut FontSystem,
        model: &EditorModel,
        width: f32,
        height: f32,
    ) {
        buffer.set_size(Some(width.max(1.0)), Some(height.max(1.0)));
        let base = Attrs::new().family(Family::Monospace);
        let is_rust = model
            .path()
            .and_then(|path| path.extension())
            .is_some_and(|extension| extension == "rs");
        if is_rust {
            let mut syntax = SyntaxDocument::rust(model.text().len()).ok();
            let highlights = syntax.as_mut().map_or_else(Vec::new, |syntax| {
                syntax.parse(model.text());
                syntax.highlight(0, model.text().len())
            });
            let mut cursor = 0;
            let mut rich = Vec::new();
            for span in highlights {
                if span.start < cursor || span.end > model.text().len() {
                    continue;
                }
                if span.start > cursor {
                    rich.push((&model.text()[cursor..span.start], base.clone()));
                }
                let color = match span.token {
                    "comment" => Color::rgb(106, 153, 85),
                    "string" => Color::rgb(206, 145, 120),
                    "number" => Color::rgb(181, 206, 168),
                    "function" => Color::rgb(220, 220, 170),
                    "type" => Color::rgb(78, 201, 176),
                    "keyword" => Color::rgb(197, 134, 192),
                    _ => TEXT_COLOR,
                };
                rich.push((
                    &model.text()[span.start..span.end],
                    base.clone().color(color),
                ));
                cursor = span.end;
            }
            if cursor < model.text().len() {
                rich.push((&model.text()[cursor..], base.clone()));
            }
            buffer.set_rich_text(rich, &base, Shaping::Advanced, None);
        } else {
            buffer.set_text(model.text(), &base, Shaping::Advanced, None);
        }
        buffer.shape_until_scroll(font_system, false);
    }

    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    fn refresh_chrome(&mut self) {
        let mut tabs = format!("●  {}", self.model.display_name());
        for model in &self.tabs {
            write!(tabs, "\t{}", model.display_name()).expect("writing to a string cannot fail");
        }
        self.tabs_buffer
            .set_size(Some(self.surface_config.width as f32), Some(TAB_BAR_HEIGHT));
        self.tabs_buffer.set_text(
            &tabs,
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );
        self.tabs_buffer
            .shape_until_scroll(&mut self.font_system, false);

        let mut sidebar = if let Some(workspace) = &self.workspace {
            let root_name = workspace.root().file_name().map_or_else(
                || workspace.root().display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            let mut text = format!("EXPLORER\n{root_name}\n\n");
            for path in workspace.files().iter().take(SIDEBAR_FILE_LIMIT) {
                let relative = path.strip_prefix(workspace.root()).unwrap_or(path);
                writeln!(text, "  {}", relative.display())
                    .expect("writing to a string cannot fail");
            }
            text
        } else {
            format!(
                "EXPLORER\n\nNo folder open\n\n{COMMAND_KEY}Shift+O  Open folder\n{COMMAND_KEY}P        Quick open\n{COMMAND_KEY}Shift+P  Commands"
            )
        };
        let is_rust = self
            .model
            .path()
            .and_then(|path| path.extension())
            .is_some_and(|extension| extension == "rs");
        if is_rust && let Ok(mut syntax) = SyntaxDocument::rust(self.model.text().len()) {
            syntax.parse(self.model.text());
            let symbols = syntax.outline(self.model.text());
            if !symbols.is_empty() {
                sidebar.push_str("\n\nOUTLINE\n");
                for symbol in symbols.into_iter().take(12) {
                    writeln!(sidebar, "  ◇ {}", symbol.name)
                        .expect("writing to a string cannot fail");
                }
            }
        }
        self.sidebar_buffer.set_size(
            Some(SIDEBAR_WIDTH - 24.0),
            Some(self.surface_config.height as f32),
        );
        self.sidebar_buffer.set_text(
            &sidebar,
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );
        self.sidebar_buffer
            .shape_until_scroll(&mut self.font_system, false);

        let point = self.model.cursor_point();
        let language = self
            .model
            .path()
            .and_then(|path| path.extension())
            .and_then(|ext| ext.to_str())
            .map_or("Plain Text", |ext| match ext {
                "rs" => "Rust",
                "js" | "mjs" | "cjs" => "JavaScript",
                "ts" | "tsx" => "TypeScript",
                "json" => "JSON",
                "md" => "Markdown",
                "toml" => "TOML",
                "yaml" | "yml" => "YAML",
                _ => "Plain Text",
            });
        let dirty = if self.model.is_dirty() {
            "Modified  •  "
        } else {
            ""
        };
        let status = format!(
            "{dirty}Ln {}, Col {}    UTF-8    {language}    {} tab{}    {}    {COMMAND_KEY}B Explorer",
            point.line + 1,
            point.byte_column + 1,
            self.tabs.len() + 1,
            if self.tabs.is_empty() { "" } else { "s" },
            match self.lsp_supervisor.state() {
                ServerState::Disabled => "LSP Off",
                ServerState::Stopped => "LSP Ready",
                ServerState::Running => "LSP Running",
                ServerState::Failed => "LSP Failed",
            }
        );
        self.status_buffer.set_size(
            Some(self.surface_config.width as f32 - 24.0),
            Some(STATUS_BAR_HEIGHT),
        );
        self.status_buffer.set_text(
            &status,
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );
        self.status_buffer
            .shape_until_scroll(&mut self.font_system, false);
    }

    fn refresh_after_edit(&mut self) {
        let mut title = self.model.title();
        if !self.tabs.is_empty() {
            write!(title, " [{} tabs]", self.tabs.len() + 1)
                .expect("writing to a string cannot fail");
        }
        if self.split_view {
            title.push_str(" [split]");
        }
        self.window.set_title(&title);
        let text = self.model.text().to_owned();
        self.accessibility
            .update_if_active(|| accessibility::tree_update(&text, false));
        if self.model.is_dirty() {
            if let Err(error) = self.recovery_journal.write(&self.model.recovery_record()) {
                eprintln!("vanta: failed to write recovery record: {error}");
            }
        } else if let Err(error) = self.recovery_journal.remove(&self.model.recovery_id()) {
            eprintln!("vanta: failed to clear recovery record: {error}");
        }
        self.sync_language_document(false);
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

    fn set_preedit(&mut self, text: &str) {
        self.ime_preedit.clear();
        self.ime_preedit.push_str(text);
        self.ime_buffer.set_text(
            text,
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );
        self.ime_buffer
            .shape_until_scroll(&mut self.font_system, false);
        self.request_redraw();
    }

    #[allow(clippy::too_many_lines)]
    fn keyboard_input(&mut self, key: &Key, text: Option<&str>) {
        let command_modifier = self.modifiers.super_key() || self.modifiers.control_key();
        if self.palette.is_some() {
            self.palette_input(key, text);
            return;
        }
        if command_modifier
            && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("n"))
        {
            self.execute_command(commands::FILE_NEW);
            return;
        }
        if self.modifiers.control_key() && matches!(key, Key::Named(NamedKey::Tab)) {
            self.execute_command(commands::WORKBENCH_NEXT_TAB);
            return;
        }
        if command_modifier && matches!(key, Key::Character(character) if character == "\\") {
            self.execute_command(commands::VIEW_TOGGLE_SPLIT);
            return;
        }
        if command_modifier
            && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("b"))
        {
            self.execute_command(commands::VIEW_TOGGLE_SIDEBAR);
            return;
        }
        if command_modifier && matches!(key, Key::Character(character) if character == " ") {
            self.execute_command(commands::LANGUAGE_COMPLETION);
            return;
        }
        if command_modifier
            && self.modifiers.shift_key()
            && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("s"))
        {
            self.execute_command(commands::FILE_SAVE_AS);
            return;
        }
        if command_modifier
            && self.modifiers.shift_key()
            && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("o"))
        {
            self.execute_command(commands::WORKSPACE_OPEN);
            return;
        }
        if command_modifier
            && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("o"))
        {
            self.execute_command(commands::FILE_OPEN);
            return;
        }
        if command_modifier
            && self.modifiers.shift_key()
            && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("f"))
        {
            self.execute_command(commands::WORKSPACE_SEARCH);
            return;
        }
        if command_modifier
            && !self.modifiers.shift_key()
            && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("p"))
        {
            self.execute_command(commands::WORKSPACE_QUICK_OPEN);
            return;
        }
        if command_modifier
            && self.modifiers.shift_key()
            && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("p"))
        {
            self.execute_command(commands::WORKBENCH_COMMAND_PALETTE);
            return;
        }
        if command_modifier
            && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("z"))
        {
            self.execute_command(if self.modifiers.shift_key() {
                commands::EDITOR_REDO
            } else {
                commands::EDITOR_UNDO
            });
            return;
        }
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
            Key::Named(NamedKey::ArrowUp) => self.execute_command(commands::EDITOR_MOVE_UP),
            Key::Named(NamedKey::ArrowDown) => self.execute_command(commands::EDITOR_MOVE_DOWN),
            Key::Named(NamedKey::Home) => self.execute_command(commands::EDITOR_MOVE_LINE_START),
            Key::Named(NamedKey::End) => self.execute_command(commands::EDITOR_MOVE_LINE_END),
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

    #[allow(clippy::too_many_lines)]
    fn execute_command(&mut self, name: &str) {
        let id = CommandId::new(name).expect("built-in command IDs are validated during startup");
        let context =
            CommandContext::default().with_flag("editor.has_selection", self.model.has_selection());
        if !self.command_registry.is_enabled(&id, &context) {
            return;
        }

        match name {
            commands::FILE_NEW => self.open_in_tab(EditorModel::untitled()),
            commands::WORKBENCH_NEXT_TAB => self.next_tab(),
            commands::VIEW_TOGGLE_SPLIT => {
                self.split_view = !self.split_view;
                self.resize_buffer();
            }
            commands::VIEW_TOGGLE_SIDEBAR => {
                self.sidebar_visible = !self.sidebar_visible;
                self.resize_buffer();
            }
            commands::FILE_OPEN => self.open_file_dialog(),
            commands::FILE_SAVE_AS => self.save_as_dialog(),
            commands::WORKSPACE_OPEN => self.open_workspace_dialog(),
            commands::WORKSPACE_QUICK_OPEN => self.open_quick_open(),
            commands::WORKSPACE_SEARCH => self.open_workspace_search(),
            commands::WORKBENCH_COMMAND_PALETTE => {
                self.palette = Some(CommandPalette::default());
                self.refresh_palette();
            }
            commands::LANGUAGE_TOGGLE => self.toggle_language_intelligence(),
            commands::LANGUAGE_CONFIGURE => self.configure_language_server(),
            commands::LANGUAGE_COMPLETION => {
                self.request_language_feature(LanguageFeature::Completion);
            }
            commands::LANGUAGE_HOVER => self.request_language_feature(LanguageFeature::Hover),
            commands::LANGUAGE_DEFINITION => {
                self.request_language_feature(LanguageFeature::Definition);
            }
            commands::LANGUAGE_REFERENCES => {
                self.request_language_feature(LanguageFeature::References);
            }
            commands::LANGUAGE_DOCUMENT_SYMBOLS => {
                self.request_language_feature(LanguageFeature::DocumentSymbols);
            }
            commands::LANGUAGE_WORKSPACE_SYMBOLS => {
                self.request_language_feature(LanguageFeature::WorkspaceSymbols);
            }
            commands::LANGUAGE_RENAME => {
                self.pending_prompt = Some(PromptAction::Rename);
                self.palette = Some(CommandPalette::prompt("RENAME SYMBOL"));
                self.refresh_palette();
            }
            commands::LANGUAGE_FORMAT => {
                self.request_language_feature(LanguageFeature::Formatting);
            }
            commands::LANGUAGE_CODE_ACTIONS => {
                self.request_language_feature(LanguageFeature::CodeActions);
            }
            commands::LANGUAGE_SHOW_STATUS => self.show_language_status(),
            commands::LANGUAGE_SHOW_PROBLEMS => self.show_problems(),
            commands::EXTENSIONS_INSPECT => self.inspect_extensions(),
            commands::EXTENSIONS_INSTALL_OFFLINE => self.install_offline_extension(),
            commands::EXTENSIONS_TOGGLE => self.toggle_extension(),
            commands::EXTENSIONS_ROLLBACK => self.rollback_extension(),
            commands::PROFILE_EXPORT => self.export_profile(),
            commands::PROFILE_IMPORT => self.import_profile(),
            commands::FILE_SAVE => match self.model.save() {
                Ok(()) => self.refresh_after_edit(),
                Err(editor::EditorSaveError::Untitled) => self.save_as_dialog(),
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
                self.refresh_chrome();
                self.request_redraw();
            }
            commands::EDITOR_MOVE_RIGHT => {
                self.model.move_right(self.modifiers.shift_key());
                self.refresh_chrome();
                self.request_redraw();
            }
            commands::EDITOR_MOVE_UP => {
                self.model.move_vertical(-1, self.modifiers.shift_key());
                self.refresh_chrome();
                self.request_redraw();
            }
            commands::EDITOR_MOVE_DOWN => {
                self.model.move_vertical(1, self.modifiers.shift_key());
                self.refresh_chrome();
                self.request_redraw();
            }
            commands::EDITOR_MOVE_LINE_START => {
                self.model
                    .move_line_boundary(false, self.modifiers.shift_key());
                self.refresh_chrome();
                self.request_redraw();
            }
            commands::EDITOR_MOVE_LINE_END => {
                self.model
                    .move_line_boundary(true, self.modifiers.shift_key());
                self.refresh_chrome();
                self.request_redraw();
            }
            commands::EDITOR_UNDO => match self.model.undo() {
                Ok(true) => self.refresh_after_edit(),
                Ok(false) => {}
                Err(error) => eprintln!("vanta: undo failed: {error}"),
            },
            commands::EDITOR_REDO => match self.model.redo() {
                Ok(true) => self.refresh_after_edit(),
                Ok(false) => {}
                Err(error) => eprintln!("vanta: redo failed: {error}"),
            },
            commands::EDITOR_INSERT_NEWLINE => self.insert_text("\n"),
            commands::EDITOR_INSERT_TAB => self.insert_text("\t"),
            _ => eprintln!("vanta: command `{name}` has no dispatcher"),
        }
    }

    fn save_as_dialog(&mut self) {
        let mut dialog = FileDialog::new().set_title("Save Vanta document");
        if let Some(path) = self.model.path() {
            if let Some(parent) = path.parent() {
                dialog = dialog.set_directory(parent);
            }
            if let Some(name) = path.file_name() {
                dialog = dialog.set_file_name(name.to_string_lossy());
            }
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        match self.model.save_as(&path) {
            Ok(()) => self.refresh_after_edit(),
            Err(error) => eprintln!("vanta: save as failed: {error}"),
        }
    }

    fn show_info(&mut self, text: impl Into<String>) {
        self.palette = Some(CommandPalette::info(text));
        self.refresh_palette();
    }

    fn toggle_language_intelligence(&mut self) {
        if self.lsp_supervisor.state() == ServerState::Running {
            let result = self
                .lsp_process
                .as_mut()
                .map_or(Ok(()), |process| self.lsp_supervisor.disable(Some(process)));
            if let Err(error) = result {
                self.show_info(format!("LANGUAGE INTELLIGENCE\n\nFailed to stop: {error}"));
                return;
            }
            self.lsp_process = None;
            self.lsp_receiver = None;
            self.lsp_open_uri = None;
            self.lsp_requests.cancel_all();
            self.diagnostics.clear();
            self.show_info("LANGUAGE INTELLIGENCE\n\nDisabled. Diagnostics cleared; no server process remains.");
        } else if self.lsp_command.is_some() {
            self.lsp_supervisor.enable();
            self.start_language_server();
        } else {
            self.configure_language_server();
        }
        self.refresh_chrome();
    }

    fn configure_language_server(&mut self) {
        let Some(workspace_root) = self
            .workspace
            .as_ref()
            .map(|workspace| workspace.root().to_path_buf())
        else {
            self.show_info(
                "LANGUAGE SERVER CONFIGURATION\n\nOpen a workspace folder before selecting a server executable.",
            );
            return;
        };
        let Some(executable) = FileDialog::new()
            .set_title("Select a language server executable")
            .pick_file()
        else {
            return;
        };
        let trusted = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("Trust this workspace and executable?")
            .set_description(format!(
                "Workspace: {}\nExecutable: {}\n\nThe process will run without inherited secrets or environment variables.",
                workspace_root.display(),
                executable.display()
            ))
            .set_buttons(MessageButtons::YesNo)
            .show();
        if trusted != MessageDialogResult::Yes {
            self.show_info("LANGUAGE SERVER CONFIGURATION\n\nNo executable was approved.");
            return;
        }
        self.lsp_broker.set_workspace_trusted(true);
        self.lsp_broker.approvals_mut().approve(&executable);
        self.lsp_command = Some(ServerCommand {
            executable,
            arguments: Vec::new(),
            working_directory: workspace_root,
        });
        self.lsp_supervisor.enable();
        self.start_language_server();
    }

    fn start_language_server(&mut self) {
        let Some(command) = self.lsp_command.clone() else {
            return;
        };
        let mut process = match self.lsp_broker.spawn(&command) {
            Ok(process) => process,
            Err(error) => {
                self.lsp_log.push(format!("launch failed: {error}"));
                self.show_info(format!("LANGUAGE SERVER FAILED\n\n{error}"));
                return;
            }
        };
        let Some(stdout) = process.take_stdout() else {
            self.show_info("LANGUAGE SERVER FAILED\n\nServer stdout was unavailable.");
            return;
        };
        let stderr = process.take_stderr();
        let (sender, receiver) = mpsc::channel();
        let output_sender = sender.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match vanta_lsp::read_message(&mut reader) {
                    Ok(message) => {
                        if output_sender.send(LspEvent::Message(message)).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = output_sender.send(LspEvent::Log(error.to_string()));
                        let _ = output_sender.send(LspEvent::Closed);
                        return;
                    }
                }
            }
        });
        if let Some(stderr) = stderr {
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if sender.send(LspEvent::Log(line)).is_err() {
                        return;
                    }
                }
            });
        }
        let root_uri = format!("file://{}", command.working_directory.to_string_lossy());
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "synchronization": { "didSave": true },
                        "completion": {},
                        "hover": {}
                    },
                    "workspace": { "workspaceEdit": { "documentChanges": true } }
                }
            }
        });
        if let Some(stdin) = process.stdin()
            && let Err(error) = vanta_lsp::write_message(stdin, &initialize)
        {
            self.show_info(format!("LANGUAGE SERVER FAILED\n\n{error}"));
            return;
        }
        self.lsp_log.push(format!("started pid {}", process.id()));
        self.lsp_receiver = Some(receiver);
        self.lsp_process = Some(process);
        self.lsp_supervisor.mark_running();
        self.show_info(
            "LANGUAGE SERVER STARTED\n\nInitializing in the background. Editing remains available.",
        );
        self.refresh_chrome();
    }

    fn request_language_feature(&mut self, feature: LanguageFeature) {
        self.request_language_feature_with_argument(feature, None);
    }

    fn request_language_feature_with_argument(
        &mut self,
        feature: LanguageFeature,
        argument: Option<&str>,
    ) {
        if self.lsp_supervisor.state() != ServerState::Running {
            self.show_info(
                "LANGUAGE INTELLIGENCE\n\nNo server is running. Use “Configure Language Server Executable” first.",
            );
            return;
        }
        let Some(path) = self.model.path() else {
            self.show_info(
                "LANGUAGE INTELLIGENCE\n\nSave the document before requesting language features.",
            );
            return;
        };
        let uri = format!("file://{}", path.to_string_lossy());
        let position = match vanta_lsp::offset_to_utf16(self.model.text(), self.model.cursor()) {
            Ok(position) => position,
            Err(error) => {
                self.show_info(format!("LANGUAGE REQUEST FAILED\n\n{error}"));
                return;
            }
        };
        let method = match feature {
            LanguageFeature::Completion => "textDocument/completion",
            LanguageFeature::Hover => "textDocument/hover",
            LanguageFeature::Definition => "textDocument/definition",
            LanguageFeature::References => "textDocument/references",
            LanguageFeature::DocumentSymbols => "textDocument/documentSymbol",
            LanguageFeature::WorkspaceSymbols => "workspace/symbol",
            LanguageFeature::Rename => "textDocument/rename",
            LanguageFeature::Formatting => "textDocument/formatting",
            LanguageFeature::CodeActions => "textDocument/codeAction",
            LanguageFeature::Diagnostics => return,
        };
        let text_document = serde_json::json!({ "uri": uri });
        let lsp_position = serde_json::json!({
            "line": position.line,
            "character": position.character
        });
        let params = match feature {
            LanguageFeature::DocumentSymbols => {
                serde_json::json!({ "textDocument": text_document })
            }
            LanguageFeature::WorkspaceSymbols => serde_json::json!({ "query": "" }),
            LanguageFeature::References => serde_json::json!({
                "textDocument": text_document,
                "position": lsp_position,
                "context": { "includeDeclaration": true }
            }),
            LanguageFeature::Rename => serde_json::json!({
                "textDocument": text_document,
                "position": lsp_position,
                "newName": argument.unwrap_or_default()
            }),
            LanguageFeature::Formatting => serde_json::json!({
                "textDocument": text_document,
                "options": { "tabSize": 4, "insertSpaces": true }
            }),
            LanguageFeature::CodeActions => serde_json::json!({
                "textDocument": text_document,
                "range": { "start": lsp_position.clone(), "end": lsp_position },
                "context": { "diagnostics": [] }
            }),
            _ => serde_json::json!({
                "textDocument": text_document,
                "position": lsp_position
            }),
        };
        let request = self.lsp_requests.begin(feature, self.model.version());
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "method": method,
            "params": params
        });
        let result = self
            .lsp_process
            .as_mut()
            .and_then(BrokeredProcess::stdin)
            .ok_or("server input is unavailable")
            .and_then(|stdin| {
                vanta_lsp::write_message(stdin, &message).map_err(|_| "request write failed")
            });
        if let Err(error) = result {
            self.lsp_requests.cancel(request.id);
            self.show_info(format!("LANGUAGE REQUEST FAILED\n\n{error}"));
        }
    }

    fn show_language_status(&mut self) {
        let state = match self.lsp_supervisor.state() {
            ServerState::Disabled => "Disabled",
            ServerState::Stopped => "Ready; no process running",
            ServerState::Running => "Running",
            ServerState::Failed => "Stopped after restart limit",
        };
        let resource = self.lsp_process.as_ref().map_or_else(
            || "Process: none".to_owned(),
            |process| {
                format!(
                    "Process: pid {}  elapsed {:.1?}",
                    process.id(),
                    process.elapsed()
                )
            },
        );
        let recent_logs =
            self.lsp_log
                .iter()
                .rev()
                .take(4)
                .rev()
                .fold(String::new(), |mut output, line| {
                    write!(output, "\n  {line}").expect("writing to a string cannot fail");
                    output
                });
        self.show_info(format!(
            "LANGUAGE SERVER STATUS\n\nState: {state}\n{resource}\nWorkspace execution: explicit trust required\nExecutable launch: explicit approval required\nRestart limit: 3 per minute\nRecent log:{recent_logs}"
        ));
    }

    fn poll_language_server(&mut self) {
        let events: Vec<_> = self
            .lsp_receiver
            .as_ref()
            .map_or_else(Vec::new, |receiver| receiver.try_iter().collect());
        let mut closed = false;
        for event in events {
            match event {
                LspEvent::Message(message) => self.handle_language_message(&message),
                LspEvent::Log(message) => {
                    self.lsp_log.push(message);
                    if self.lsp_log.len() > 100 {
                        self.lsp_log.remove(0);
                    }
                }
                LspEvent::Closed => closed = true,
            }
        }
        if closed {
            self.lsp_process = None;
            self.lsp_receiver = None;
            self.lsp_open_uri = None;
            self.lsp_requests.cancel_all();
            self.diagnostics.clear();
            if self.lsp_supervisor.allow_restart(std::time::Instant::now()) {
                self.lsp_log.push("server stopped; restarting".to_owned());
                self.start_language_server();
            } else {
                self.show_info(
                    "LANGUAGE SERVER FAILED\n\nRestart limit reached. Editing remains available; language intelligence is stopped.",
                );
                self.refresh_chrome();
            }
        }
    }

    fn handle_language_message(&mut self, message: &serde_json::Value) {
        if message.get("method").and_then(serde_json::Value::as_str)
            == Some("textDocument/publishDiagnostics")
        {
            self.publish_diagnostics(message);
            return;
        }
        let Some(id) = message.get("id").and_then(serde_json::Value::as_u64) else {
            return;
        };
        if id == 0 {
            self.finish_language_initialization();
            return;
        }
        let Some(request) = self.lsp_requests.take_if_current(id, self.model.version()) else {
            self.lsp_log.push(format!("discarded stale response {id}"));
            return;
        };
        let null = serde_json::Value::Null;
        let result = message.get("result").unwrap_or(&null);
        match request.feature {
            LanguageFeature::Completion => {
                let items = result
                    .as_array()
                    .or_else(|| result.get("items").and_then(serde_json::Value::as_array));
                let labels: Vec<_> = items
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item.get("label").and_then(serde_json::Value::as_str))
                    .take(8)
                    .collect();
                if labels.is_empty() {
                    self.show_info("COMPLETIONS\n\nNo completion items returned.");
                } else {
                    self.show_info(format!("COMPLETIONS\n\n  {}", labels.join("\n  ")));
                }
            }
            LanguageFeature::Hover => {
                let contents = result.get("contents").unwrap_or(result);
                let text = contents
                    .as_str()
                    .or_else(|| contents.get("value").and_then(serde_json::Value::as_str))
                    .unwrap_or("No hover information returned.");
                self.show_info(format!("HOVER\n\n{text}"));
            }
            LanguageFeature::Formatting | LanguageFeature::Rename => {
                self.apply_language_edits(request.document_version, result);
            }
            LanguageFeature::CodeActions => {
                let titles: Vec<_> = result
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item.get("title").and_then(serde_json::Value::as_str))
                    .take(8)
                    .collect();
                self.show_info(if titles.is_empty() {
                    "CODE ACTIONS\n\nNo actions available.".to_owned()
                } else {
                    format!("CODE ACTIONS\n\n  {}", titles.join("\n  "))
                });
            }
            _ => {
                let rendered =
                    serde_json::to_string_pretty(result).unwrap_or_else(|_| "No result".to_owned());
                let rendered: String = rendered.chars().take(2_000).collect();
                self.show_info(format!("LANGUAGE RESPONSE\n\n{rendered}"));
            }
        }
    }

    fn apply_language_edits(
        &mut self,
        document_version: vanta_document::DocumentVersion,
        result: &serde_json::Value,
    ) {
        let Some(path) = self.model.path() else {
            return;
        };
        let uri = format!("file://{}", path.to_string_lossy());
        let edits = result.as_array().or_else(|| {
            result
                .get("changes")
                .and_then(|changes| changes.get(&uri))
                .and_then(serde_json::Value::as_array)
        });
        let Some(edits) = edits else {
            self.show_info(
                "WORKSPACE EDIT PREVIEW\n\nThe server returned a multi-document or unsupported edit. Nothing was changed.",
            );
            return;
        };
        let edits: Vec<_> = edits
            .iter()
            .filter_map(|edit| {
                let range = edit.get("range")?;
                Some(VersionedTextEdit {
                    range: Utf16Range {
                        start: Self::decode_position(range.get("start")?)?,
                        end: Self::decode_position(range.get("end")?)?,
                    },
                    new_text: edit.get("newText")?.as_str()?.to_owned(),
                })
            })
            .collect();
        let edit = WorkspaceEdit {
            document_version,
            edits,
        };
        match vanta_lsp::workspace_edit_transaction(
            self.model.text(),
            self.model.version(),
            edit,
            "language-server",
        )
        .and_then(|transaction| {
            self.model
                .apply_external_transaction(transaction)
                .map_err(|error| vanta_lsp::LspError::Process(error.to_string()))
        }) {
            Ok(()) => {
                self.refresh_after_edit();
                self.show_info(
                    "WORKSPACE EDIT APPLIED\n\nThe current document was updated atomically.",
                );
            }
            Err(error) => self.show_info(format!(
                "WORKSPACE EDIT REJECTED\n\n{error}\nThe document was not changed."
            )),
        }
    }

    fn finish_language_initialization(&mut self) {
        let initialized = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        });
        let sent = self
            .lsp_process
            .as_mut()
            .and_then(BrokeredProcess::stdin)
            .is_some_and(|stdin| vanta_lsp::write_message(stdin, &initialized).is_ok());
        if !sent {
            return;
        }
        self.lsp_open_uri = None;
        self.sync_language_document(true);
        self.lsp_log.push("initialized".to_owned());
        self.refresh_chrome();
    }

    fn sync_language_document(&mut self, force_open: bool) {
        if self.lsp_supervisor.state() != ServerState::Running {
            return;
        }
        let current = self.model.path().map(|path| {
            let language_id = path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("plaintext")
                .to_owned();
            (
                format!("file://{}", path.to_string_lossy()),
                language_id,
                self.model.version().0,
                self.model.text().to_owned(),
            )
        });
        let document_changed = self.lsp_open_uri.as_ref() != current.as_ref().map(|item| &item.0);
        let mut messages = Vec::new();
        if (force_open || document_changed)
            && let Some(previous) = &self.lsp_open_uri
        {
            messages.push(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didClose",
                "params": { "textDocument": { "uri": previous } }
            }));
        }
        if let Some((uri, language_id, version, text)) = &current {
            if force_open || document_changed {
                messages.push(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didOpen",
                    "params": {
                        "textDocument": {
                            "uri": uri,
                            "languageId": language_id,
                            "version": version,
                            "text": text
                        }
                    }
                }));
            } else {
                messages.push(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didChange",
                    "params": {
                        "textDocument": { "uri": uri, "version": version },
                        "contentChanges": [{ "text": text }]
                    }
                }));
            }
        }
        if let Some(stdin) = self.lsp_process.as_mut().and_then(BrokeredProcess::stdin) {
            for message in &messages {
                if let Err(error) = vanta_lsp::write_message(stdin, message) {
                    self.lsp_log.push(format!("document sync failed: {error}"));
                    break;
                }
            }
        }
        self.lsp_open_uri = current.map(|item| item.0);
    }

    fn publish_diagnostics(&mut self, message: &serde_json::Value) {
        let Some(params) = message.get("params") else {
            return;
        };
        let Some(uri) = params.get("uri").and_then(serde_json::Value::as_str) else {
            return;
        };
        let Some(version) = params.get("version").and_then(serde_json::Value::as_u64) else {
            self.lsp_log
                .push("discarded unversioned diagnostics".to_owned());
            return;
        };
        let diagnostics = params
            .get("diagnostics")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Self::decode_diagnostic)
            .collect();
        let published = vanta_document::DocumentVersion(version);
        if !self
            .diagnostics
            .publish(uri, published, self.model.version(), diagnostics)
        {
            self.lsp_log
                .push(format!("discarded stale diagnostics v{version}"));
        }
    }

    fn decode_diagnostic(value: &serde_json::Value) -> Option<Diagnostic> {
        let range = value.get("range")?;
        let severity = match value
            .get("severity")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(3)
        {
            1 => DiagnosticSeverity::Error,
            2 => DiagnosticSeverity::Warning,
            4 => DiagnosticSeverity::Hint,
            _ => DiagnosticSeverity::Information,
        };
        Some(Diagnostic {
            range: Utf16Range {
                start: Self::decode_position(range.get("start")?)?,
                end: Self::decode_position(range.get("end")?)?,
            },
            severity,
            message: value.get("message")?.as_str()?.to_owned(),
            source: value
                .get("source")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        })
    }

    fn decode_position(value: &serde_json::Value) -> Option<Utf16Position> {
        Some(Utf16Position {
            line: usize::try_from(value.get("line")?.as_u64()?).ok()?,
            character: usize::try_from(value.get("character")?.as_u64()?).ok()?,
        })
    }

    fn show_problems(&mut self) {
        let uri = self.model.path().map_or_else(
            || "untitled".to_owned(),
            |path| format!("file://{}", path.to_string_lossy()),
        );
        let diagnostics = self.diagnostics.for_document(&uri);
        let mut text = format!(
            "PROBLEMS\n\n{} problem(s) for {uri}\nDocument version: {}",
            diagnostics.len(),
            self.model.version().0
        );
        for diagnostic in diagnostics.iter().take(8) {
            write!(
                text,
                "\n  {:?}  {}:{}  {}",
                diagnostic.severity,
                diagnostic.range.start.line + 1,
                diagnostic.range.start.character + 1,
                diagnostic.message
            )
            .expect("writing to a string cannot fail");
        }
        self.show_info(text);
    }

    fn inspect_extensions(&mut self) {
        let installed: Vec<_> = self.extension_manager.installed().collect();
        let mut text = format!(
            "EXTENSIONS AND PERMISSIONS\n\nSDK: {}.{}\n{} extension(s) installed offline",
            SDK_VERSION.major,
            SDK_VERSION.minor,
            installed.len()
        );
        for (id, extension) in installed.into_iter().take(8) {
            write!(
                text,
                "\n  {id} {}  {}  {:?}",
                extension.active.version,
                if extension.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                extension.approved_permissions
            )
            .expect("writing to a string cannot fail");
        }
        self.show_info(text);
    }

    fn install_offline_extension(&mut self) {
        let Some(path) = FileDialog::new()
            .set_title("Install offline Vanta extension manifest")
            .add_filter("Vanta extension manifest", &["json"])
            .pick_file()
        else {
            return;
        };
        let package = match ExtensionPackage::from_manifest(&path) {
            Ok(package) => package,
            Err(error) => {
                self.show_info(format!("EXTENSION INSTALL FAILED\n\n{error}"));
                return;
            }
        };
        let package_id = package.id.clone();
        let package_version = package.version.clone();
        match self.extension_manager.install(package.clone()) {
            Ok(InstallOutcome::Installed) => self.show_info(format!(
                "EXTENSION INSTALLED\n\n{package_id} {package_version}\nPermissions: {:?}",
                package.permissions
            )),
            Ok(InstallOutcome::Updated) => self.show_info(format!(
                "EXTENSION UPDATED\n\n{package_id} {package_version}\nPrevious package retained for rollback."
            )),
            Ok(InstallOutcome::PermissionApprovalRequired(expansion)) => {
                let approved = MessageDialog::new()
                    .set_level(MessageLevel::Warning)
                    .set_title("Extension requests additional permissions")
                    .set_description(format!(
                        "{package_id} requests: {expansion:?}\nApprove this update?"
                    ))
                    .set_buttons(MessageButtons::YesNo)
                    .show();
                if approved == MessageDialogResult::Yes {
                    match self.extension_manager.approve_update(package) {
                        Ok(()) => self.show_info(format!(
                            "EXTENSION UPDATED\n\n{package_id} {package_version}\nAdditional permissions approved."
                        )),
                        Err(error) => {
                            self.show_info(format!("EXTENSION UPDATE FAILED\n\n{error}"));
                        }
                    }
                } else {
                    self.show_info("EXTENSION UPDATE CANCELLED\n\nThe existing package was not changed.");
                }
            }
            Err(error) => self.show_info(format!("EXTENSION INSTALL FAILED\n\n{error}")),
        }
    }

    fn toggle_extension(&mut self) {
        let selected = self
            .extension_manager
            .installed()
            .next()
            .map(|(id, extension)| (id.to_owned(), extension.enabled));
        let Some((id, enabled)) = selected else {
            self.show_info("EXTENSIONS\n\nNo extension is installed.");
            return;
        };
        match self.extension_manager.set_enabled(&id, !enabled) {
            Ok(()) => self.show_info(format!(
                "EXTENSION {}\n\n{id}",
                if enabled { "DISABLED" } else { "ENABLED" }
            )),
            Err(error) => self.show_info(format!("EXTENSION UPDATE FAILED\n\n{error}")),
        }
    }

    fn rollback_extension(&mut self) {
        let id = self
            .extension_manager
            .installed()
            .next()
            .map(|(id, _)| id.to_owned());
        let Some(id) = id else {
            self.show_info("EXTENSIONS\n\nNo extension is installed.");
            return;
        };
        match self.extension_manager.rollback(&id) {
            Ok(()) => self.show_info(format!("EXTENSION ROLLED BACK\n\n{id}")),
            Err(error) => self.show_info(format!("EXTENSION ROLLBACK FAILED\n\n{error}")),
        }
    }

    fn export_profile(&mut self) {
        let Some(path) = FileDialog::new()
            .set_title("Export Vanta profile")
            .set_file_name("vanta-profile.json")
            .save_file()
        else {
            return;
        };
        match self.profile.export().and_then(|source| {
            std::fs::write(&path, source)
                .map_err(|error| vanta_config::ConfigError::Io(error.to_string()))
        }) {
            Ok(()) => self.show_info(format!("PROFILE EXPORTED\n\n{}", path.display())),
            Err(error) => self.show_info(format!("PROFILE EXPORT FAILED\n\n{error}")),
        }
    }

    fn import_profile(&mut self) {
        let Some(path) = FileDialog::new()
            .set_title("Import Vanta profile")
            .add_filter("Vanta profile", &["json"])
            .pick_file()
        else {
            return;
        };
        let imported = std::fs::read_to_string(&path)
            .map_err(|error| vanta_config::ConfigError::Io(error.to_string()))
            .and_then(|source| Profile::import(&source));
        match imported {
            Ok(profile) => {
                let name = profile.name.clone();
                self.profile = profile;
                self.show_info(format!(
                    "PROFILE IMPORTED\n\n{name}\nNo secrets were present."
                ));
            }
            Err(error) => self.show_info(format!("PROFILE IMPORT FAILED\n\n{error}")),
        }
    }

    fn open_file_dialog(&mut self) {
        let Some(path) = FileDialog::new().set_title("Open a text file").pick_file() else {
            return;
        };
        match EditorModel::open(&path) {
            Ok(model) => self.open_in_tab(model),
            Err(error) => eprintln!("vanta: failed to open {}: {error}", path.display()),
        }
    }

    fn open_in_tab(&mut self, model: EditorModel) {
        let previous = std::mem::replace(&mut self.model, model);
        self.tabs.push(previous);
        self.refresh_after_edit();
    }

    fn next_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let next = self.tabs.remove(0);
        let previous = std::mem::replace(&mut self.model, next);
        self.tabs.push(previous);
        self.refresh_after_edit();
    }

    fn activate_tab(&mut self, index: usize) {
        if index == 0 {
            return;
        }
        if let Some(tab) = self.tabs.get_mut(index - 1) {
            std::mem::swap(&mut self.model, tab);
            self.refresh_after_edit();
        }
    }

    fn open_workspace_dialog(&mut self) {
        let Some(path) = FileDialog::new()
            .set_title("Open a workspace folder")
            .pick_folder()
        else {
            return;
        };
        match Workspace::open(path) {
            Ok(workspace) => {
                self.workspace = Some(workspace);
                self.sidebar_visible = true;
                self.resize_buffer();
                self.open_quick_open();
            }
            Err(error) => eprintln!("vanta: failed to index workspace: {error}"),
        }
    }

    fn open_quick_open(&mut self) {
        let Some(workspace) = &self.workspace else {
            self.open_workspace_dialog();
            return;
        };
        let files = workspace
            .files()
            .iter()
            .filter_map(|path| path.strip_prefix(workspace.root()).ok().map(PathBuf::from))
            .collect();
        self.palette = Some(CommandPalette::files(files));
        self.refresh_palette();
    }

    fn open_workspace_search(&mut self) {
        if self.workspace.is_none() {
            self.open_workspace_dialog();
            return;
        }
        self.palette = Some(CommandPalette::search());
        self.refresh_palette();
    }

    fn start_workspace_search(&mut self, query: String) {
        let Some(workspace) = self.workspace.clone() else {
            return;
        };
        if let Some(token) = self.search_cancellation.take() {
            token.cancel();
        }
        let token = CancellationToken::default();
        let worker_token = token.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut paths = Vec::new();
            let _ = workspace.search(&query, &worker_token, |found| {
                if paths.len() < 200 && !paths.contains(&found.path) {
                    paths.push(found.path);
                }
            });
            let _ = sender.send(paths);
        });
        self.search_cancellation = Some(token);
        self.search_receiver = Some(receiver);
        self.palette = Some(CommandPalette::files(Vec::new()));
        self.refresh_palette();
    }

    fn poll_workspace_search(&mut self) {
        let result = self
            .search_receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        if let Some(paths) = result {
            self.palette = Some(CommandPalette::files(paths));
            self.search_receiver = None;
            self.search_cancellation = None;
            self.refresh_palette();
        }
    }

    fn open_workspace_file(&mut self, relative: &std::path::Path) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let path = workspace.root().join(relative);
        match EditorModel::open(&path) {
            Ok(model) => self.open_in_tab(model),
            Err(error) => eprintln!("vanta: failed to open {}: {error}", path.display()),
        }
    }

    fn confirm_close(&mut self) -> bool {
        if !self.model.is_dirty() && self.tabs.iter().all(|model| !model.is_dirty()) {
            return true;
        }
        match MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("Unsaved changes")
            .set_description("Save all changed tabs before closing Vanta?")
            .set_buttons(MessageButtons::YesNoCancel)
            .show()
        {
            MessageDialogResult::Yes => self.save_all_tabs(),
            MessageDialogResult::No => {
                for model in std::iter::once(&self.model).chain(&self.tabs) {
                    let _ = self.recovery_journal.remove(&model.recovery_id());
                }
                true
            }
            _ => false,
        }
    }

    fn save_all_tabs(&mut self) -> bool {
        loop {
            if self.model.is_dirty() {
                if self.model.save().is_err() {
                    self.save_as_dialog();
                }
                if self.model.is_dirty() {
                    return false;
                }
            }
            let Some(index) = self.tabs.iter().position(EditorModel::is_dirty) else {
                return true;
            };
            std::mem::swap(&mut self.model, &mut self.tabs[index]);
        }
    }

    fn restore_session_state(&mut self, session: Session) {
        if let Some(root) = session
            .workspace
            .and_then(|root| Workspace::open(root).ok())
        {
            self.workspace = Some(root);
        }
        for (index, document) in session.documents.into_iter().enumerate() {
            if index == session.active {
                let _ = self.model.set_cursor_point(
                    vanta_document::TextPoint {
                        line: document.line,
                        byte_column: document.byte_column,
                    },
                    false,
                );
                continue;
            }
            if let Ok(mut model) = EditorModel::open(&document.path) {
                let _ = model.set_cursor_point(
                    vanta_document::TextPoint {
                        line: document.line,
                        byte_column: document.byte_column,
                    },
                    false,
                );
                self.tabs.push(model);
            }
        }
        self.refresh_after_edit();
    }

    fn save_session(&self) {
        let documents = self.model.path().map_or_else(Vec::new, |path| {
            let point = self.model.cursor_point();
            vec![DocumentState {
                path: path.to_path_buf(),
                line: point.line,
                byte_column: point.byte_column,
            }]
        });
        let mut documents = documents;
        documents.extend(self.tabs.iter().filter_map(|model| {
            let path = model.path()?;
            let point = model.cursor_point();
            Some(DocumentState {
                path: path.to_path_buf(),
                line: point.line,
                byte_column: point.byte_column,
            })
        }));
        let session = Session {
            workspace: self
                .workspace
                .as_ref()
                .map(|workspace| workspace.root().to_path_buf()),
            documents,
            active: 0,
        };
        if let Err(error) = session.save(&Application::session_path()) {
            eprintln!("vanta: failed to save session: {error}");
        }
    }

    fn command_context(&self) -> CommandContext {
        CommandContext::default().with_flag("editor.has_selection", self.model.has_selection())
    }

    #[allow(clippy::too_many_lines)]
    fn palette_input(&mut self, key: &Key, text: Option<&str>) {
        if self.palette.as_ref().is_some_and(CommandPalette::is_info) {
            if matches!(key, Key::Named(NamedKey::Escape | NamedKey::Enter)) {
                self.palette = None;
                self.request_redraw();
            }
            return;
        }
        if self.palette.as_ref().is_some_and(CommandPalette::is_prompt) {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.palette = None;
                    self.pending_prompt = None;
                    self.request_redraw();
                }
                Key::Named(NamedKey::Enter) => {
                    let value = self
                        .palette
                        .as_ref()
                        .map_or_else(String::new, |palette| palette.query().trim().to_owned());
                    let action = self.pending_prompt.take();
                    self.palette = None;
                    if !value.is_empty() && matches!(action, Some(PromptAction::Rename)) {
                        self.request_language_feature_with_argument(
                            LanguageFeature::Rename,
                            Some(&value),
                        );
                    } else {
                        self.request_redraw();
                    }
                }
                Key::Named(NamedKey::Backspace) => {
                    if let Some(palette) = &mut self.palette {
                        palette.backspace();
                    }
                    self.refresh_palette();
                }
                _ => {
                    if let Some(value) = text.filter(|value| !value.chars().any(char::is_control))
                        && let Some(palette) = &mut self.palette
                    {
                        palette.insert(value);
                    }
                    self.refresh_palette();
                }
            }
            return;
        }
        match key {
            Key::Named(NamedKey::Escape) => {
                self.palette = None;
                self.request_redraw();
                return;
            }
            Key::Named(NamedKey::Enter) => {
                if self
                    .palette
                    .as_ref()
                    .is_some_and(CommandPalette::is_search_input)
                {
                    let query = self
                        .palette
                        .as_ref()
                        .map_or_else(String::new, |palette| palette.query().to_owned());
                    self.start_workspace_search(query);
                    return;
                }
                let file = self
                    .palette
                    .as_ref()
                    .and_then(CommandPalette::selected_file);
                let command = self.palette.as_ref().and_then(|palette| {
                    palette.selected_command(&self.command_registry, &self.command_context())
                });
                self.palette = None;
                if let Some(file) = file {
                    self.open_workspace_file(&file);
                } else if let Some(command) = command {
                    self.execute_command(command.as_str());
                } else {
                    self.request_redraw();
                }
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(palette) = &mut self.palette {
                    palette.backspace();
                }
            }
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                let context = self.command_context();
                let count = self.palette.as_ref().map_or(0, |palette| {
                    palette.result_count(&self.command_registry, &context)
                });
                if let Some(palette) = &mut self.palette {
                    palette.move_selection(
                        if matches!(key, Key::Named(NamedKey::ArrowUp)) {
                            -1
                        } else {
                            1
                        },
                        count,
                    );
                }
            }
            _ => {
                if let Some(value) = text.filter(|value| !value.chars().any(char::is_control))
                    && let Some(palette) = &mut self.palette
                {
                    palette.insert(value);
                }
            }
        }
        self.refresh_palette();
    }

    fn refresh_palette(&mut self) {
        let Some(palette) = &self.palette else {
            return;
        };
        let text = palette.display_text(&self.command_registry, &self.command_context());
        self.palette_buffer.set_text(
            &text,
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );
        self.palette_buffer
            .shape_until_scroll(&mut self.font_system, false);
        self.request_redraw();
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

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    fn mouse_button(&mut self, state: ElementState) {
        self.selecting = state == ElementState::Pressed;
        if !self.selecting {
            return;
        }
        let (x, y) = self.pointer_position;
        if y < TAB_BAR_HEIGHT {
            self.selecting = false;
            self.activate_tab((x / TAB_WIDTH).floor() as usize);
            return;
        }
        if self.sidebar_visible && x < SIDEBAR_WIDTH {
            self.selecting = false;
            let first_file_top = TAB_BAR_HEIGHT + 12.0 + SIDEBAR_LINE_HEIGHT * 3.0;
            if y >= first_file_top {
                let index = ((y - first_file_top) / SIDEBAR_LINE_HEIGHT).floor() as usize;
                if index >= SIDEBAR_FILE_LIMIT {
                    return;
                }
                let relative = self.workspace.as_ref().and_then(|workspace| {
                    workspace.files().get(index).and_then(|path| {
                        path.strip_prefix(workspace.root()).ok().map(PathBuf::from)
                    })
                });
                if let Some(relative) = relative {
                    self.open_workspace_file(&relative);
                }
            }
            return;
        }
        let bottom = self.surface_config.height as f32 - STATUS_BAR_HEIGHT;
        if y < Self::editor_top() || y >= bottom || x < self.editor_left() {
            self.selecting = false;
            return;
        }
        if self.modifiers.alt_key() {
            self.add_cursor();
        } else {
            self.place_cursor(self.modifiers.shift_key());
        }
    }

    fn add_cursor(&mut self) {
        let (x, y) = self.pointer_position;
        let Some(cursor) = self
            .text_buffer
            .hit(x - self.editor_left(), y - Self::editor_top())
        else {
            return;
        };
        if let Err(error) = self.model.add_cursor_point(vanta_document::TextPoint {
            line: cursor.line,
            byte_column: cursor.index,
        }) {
            eprintln!("vanta: additional cursor rejected: {error}");
        }
        self.selecting = false;
        self.refresh_chrome();
        self.request_redraw();
    }

    fn place_cursor(&mut self, extend: bool) {
        let (x, y) = self.pointer_position;
        let Some(cursor) = self
            .text_buffer
            .hit(x - self.editor_left(), y - Self::editor_top())
        else {
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
        self.refresh_chrome();
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
                        left: self.editor_left() + left,
                        top: Self::editor_top() + run.line_top,
                        width,
                        height: run.line_height,
                    })
            })
            .collect()
    }

    fn caret_rectangles(&self) -> Vec<OverlayRect> {
        self.model
            .cursor_points()
            .into_iter()
            .filter_map(|point| {
                self.text_buffer
                    .cursor_position(&Cursor::new(point.line, point.byte_column))
            })
            .map(|(left, top)| OverlayRect {
                left: self.editor_left() + left,
                top: Self::editor_top() + top,
                width: 2.0,
                height: 22.0,
            })
            .collect()
    }

    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
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
        let caret_rectangles = self.caret_rectangles();
        let palette_rectangle = self.palette.as_ref().map(|_| OverlayRect {
            left: 60.0,
            top: 36.0,
            width: (self.surface_config.width as f32 - 120.0).min(660.0),
            height: 250.0,
        });
        let chrome_rectangles = [
            OverlayRect {
                left: 0.0,
                top: 0.0,
                width: self.surface_config.width as f32,
                height: TAB_BAR_HEIGHT,
            },
            OverlayRect {
                left: 0.0,
                top: self.surface_config.height as f32 - STATUS_BAR_HEIGHT,
                width: self.surface_config.width as f32,
                height: STATUS_BAR_HEIGHT,
            },
        ];
        let sidebar_rectangle = self.sidebar_visible.then_some(OverlayRect {
            left: 0.0,
            top: TAB_BAR_HEIGHT,
            width: SIDEBAR_WIDTH,
            height: self.surface_config.height as f32 - TAB_BAR_HEIGHT - STATUS_BAR_HEIGHT,
        });
        let (editor_width, _) = self.editor_size();
        let split_divider = self.split_view.then(|| OverlayRect {
            left: self.editor_left() + editor_width / 2.0,
            top: TAB_BAR_HEIGHT,
            width: 2.0,
            height: self.surface_config.height as f32 - TAB_BAR_HEIGHT - STATUS_BAR_HEIGHT,
        });
        let mut divider_rectangles = vec![
            OverlayRect {
                left: 0.0,
                top: TAB_BAR_HEIGHT - 1.0,
                width: self.surface_config.width as f32,
                height: 1.0,
            },
            OverlayRect {
                left: 0.0,
                top: self.surface_config.height as f32 - STATUS_BAR_HEIGHT,
                width: self.surface_config.width as f32,
                height: 1.0,
            },
        ];
        if self.sidebar_visible {
            divider_rectangles.push(OverlayRect {
                left: SIDEBAR_WIDTH - 1.0,
                top: TAB_BAR_HEIGHT,
                width: 1.0,
                height: self.surface_config.height as f32 - TAB_BAR_HEIGHT - STATUS_BAR_HEIGHT,
            });
        }
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
            self.overlay_renderer.draw_chrome(
                &mut pass,
                &chrome_rectangles,
                self.surface_config.width,
                self.surface_config.height,
            );
            if let Some(rectangle) = sidebar_rectangle {
                self.overlay_renderer.draw_chrome(
                    &mut pass,
                    &[rectangle],
                    self.surface_config.width,
                    self.surface_config.height,
                );
            }
            self.overlay_renderer.draw_divider(
                &mut pass,
                &divider_rectangles,
                self.surface_config.width,
                self.surface_config.height,
            );
            self.overlay_renderer.draw_selection(
                &mut pass,
                &selection_rectangles,
                self.surface_config.width,
                self.surface_config.height,
            );
            if let Some(rectangle) = palette_rectangle {
                self.overlay_renderer.draw_selection(
                    &mut pass,
                    &[rectangle],
                    self.surface_config.width,
                    self.surface_config.height,
                );
            }
            if let Some(rectangle) = split_divider {
                self.overlay_renderer.draw_divider(
                    &mut pass,
                    &[rectangle],
                    self.surface_config.width,
                    self.surface_config.height,
                );
            }
            if let Err(error) = self
                .text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
            {
                eprintln!("vanta: text rendering failed: {error}");
                return;
            }
            for caret_rectangle in caret_rectangles {
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

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::too_many_lines
    )]
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
        let ime_position = (!self.ime_preedit.is_empty())
            .then(|| self.caret_rectangles().into_iter().next())
            .flatten();
        let mut areas = vec![TextArea {
            buffer: &self.text_buffer,
            left: self.editor_left(),
            top: Self::editor_top(),
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: TAB_BAR_HEIGHT as i32,
                right,
                bottom: bottom - STATUS_BAR_HEIGHT as i32,
            },
            default_color: TEXT_COLOR,
            custom_glyphs: &[],
        }];
        if self.split_view {
            let (editor_width, _) = self.editor_size();
            let midpoint_f32 = self.editor_left() + editor_width / 2.0;
            let midpoint = midpoint_f32 as i32;
            areas[0].bounds.right = midpoint;
            areas.push(TextArea {
                buffer: &self.text_buffer,
                left: midpoint_f32 + PADDING,
                top: Self::editor_top(),
                scale: 1.0,
                bounds: TextBounds {
                    left: midpoint,
                    top: TAB_BAR_HEIGHT as i32,
                    right,
                    bottom: bottom - STATUS_BAR_HEIGHT as i32,
                },
                default_color: TEXT_COLOR,
                custom_glyphs: &[],
            });
        }
        areas.push(TextArea {
            buffer: &self.tabs_buffer,
            left: 12.0,
            top: 7.0,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: 0,
                right,
                bottom: TAB_BAR_HEIGHT as i32,
            },
            default_color: TEXT_COLOR,
            custom_glyphs: &[],
        });
        if self.sidebar_visible {
            areas.push(TextArea {
                buffer: &self.sidebar_buffer,
                left: 12.0,
                top: TAB_BAR_HEIGHT + 12.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: TAB_BAR_HEIGHT as i32,
                    right: SIDEBAR_WIDTH as i32,
                    bottom: bottom - STATUS_BAR_HEIGHT as i32,
                },
                default_color: MUTED_TEXT_COLOR,
                custom_glyphs: &[],
            });
        }
        areas.push(TextArea {
            buffer: &self.status_buffer,
            left: 12.0,
            top: self.surface_config.height as f32 - STATUS_BAR_HEIGHT + 4.0,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: bottom - STATUS_BAR_HEIGHT as i32,
                right,
                bottom,
            },
            default_color: MUTED_TEXT_COLOR,
            custom_glyphs: &[],
        });
        if self.palette.is_some() {
            areas.push(TextArea {
                buffer: &self.palette_buffer,
                left: 72.0,
                top: 48.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: 60,
                    top: 36,
                    right: right.min(720),
                    bottom: bottom.min(286),
                },
                default_color: TEXT_COLOR,
                custom_glyphs: &[],
            });
        }
        if let Some(position) = ime_position {
            areas.push(TextArea {
                buffer: &self.ime_buffer,
                left: position.left,
                top: position.top,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right,
                    bottom,
                },
                default_color: Color::rgb(242, 194, 109),
                custom_glyphs: &[],
            });
        }
        if let Err(error) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash_cache,
        ) {
            eprintln!("vanta: text preparation failed: {error}");
            return false;
        }
        true
    }
}
