use vanta_commands::{
    CommandDescriptor, CommandError, CommandId, CommandRegistry, CommandTarget, Enablement,
};

pub const FILE_SAVE: &str = "file.save";
pub const FILE_SAVE_AS: &str = "file.save_as";
pub const FILE_OPEN: &str = "file.open";
pub const FILE_NEW: &str = "file.new";
pub const WORKSPACE_OPEN: &str = "workspace.open";
pub const WORKSPACE_QUICK_OPEN: &str = "workspace.quick_open";
pub const WORKSPACE_SEARCH: &str = "workspace.search";
pub const WORKBENCH_NEXT_TAB: &str = "workbench.next_tab";
pub const VIEW_TOGGLE_SPLIT: &str = "view.toggle_split";
pub const WORKBENCH_COMMAND_PALETTE: &str = "workbench.command_palette";
pub const EDITOR_SELECT_ALL: &str = "editor.select_all";
pub const EDITOR_COPY: &str = "editor.copy";
pub const EDITOR_CUT: &str = "editor.cut";
pub const EDITOR_PASTE: &str = "editor.paste";
pub const EDITOR_BACKSPACE: &str = "editor.backspace";
pub const EDITOR_DELETE: &str = "editor.delete";
pub const EDITOR_MOVE_LEFT: &str = "editor.move_left";
pub const EDITOR_MOVE_RIGHT: &str = "editor.move_right";
pub const EDITOR_MOVE_UP: &str = "editor.move_up";
pub const EDITOR_MOVE_DOWN: &str = "editor.move_down";
pub const EDITOR_MOVE_LINE_START: &str = "editor.move_line_start";
pub const EDITOR_MOVE_LINE_END: &str = "editor.move_line_end";
pub const EDITOR_UNDO: &str = "editor.undo";
pub const EDITOR_REDO: &str = "editor.redo";
pub const EDITOR_INSERT_NEWLINE: &str = "editor.insert_newline";
pub const EDITOR_INSERT_TAB: &str = "editor.insert_tab";

pub fn built_in_registry() -> Result<CommandRegistry, CommandError> {
    let mut registry = CommandRegistry::new();
    register(
        &mut registry,
        FILE_SAVE,
        "Save",
        "File",
        CommandTarget::Document,
    )?;
    for (id, title) in [
        (FILE_SAVE_AS, "Save As"),
        (FILE_OPEN, "Open File"),
        (FILE_NEW, "New File"),
        (WORKSPACE_OPEN, "Open Folder"),
        (WORKSPACE_QUICK_OPEN, "Quick Open"),
        (WORKSPACE_SEARCH, "Search Workspace"),
    ] {
        register(&mut registry, id, title, "File", CommandTarget::Application)?;
    }
    register(
        &mut registry,
        WORKBENCH_NEXT_TAB,
        "Next Tab",
        "View",
        CommandTarget::Application,
    )?;
    register(
        &mut registry,
        VIEW_TOGGLE_SPLIT,
        "Toggle Split View",
        "View",
        CommandTarget::View,
    )?;
    register(
        &mut registry,
        WORKBENCH_COMMAND_PALETTE,
        "Show Command Palette",
        "View",
        CommandTarget::Application,
    )?;
    register(
        &mut registry,
        EDITOR_SELECT_ALL,
        "Select All",
        "Editor",
        CommandTarget::View,
    )?;
    for (id, title) in [
        (EDITOR_PASTE, "Paste"),
        (EDITOR_BACKSPACE, "Delete Left"),
        (EDITOR_DELETE, "Delete Right"),
        (EDITOR_MOVE_LEFT, "Move Left"),
        (EDITOR_MOVE_RIGHT, "Move Right"),
        (EDITOR_MOVE_UP, "Move Up"),
        (EDITOR_MOVE_DOWN, "Move Down"),
        (EDITOR_MOVE_LINE_START, "Move to Line Start"),
        (EDITOR_MOVE_LINE_END, "Move to Line End"),
        (EDITOR_UNDO, "Undo"),
        (EDITOR_REDO, "Redo"),
        (EDITOR_INSERT_NEWLINE, "Insert Newline"),
        (EDITOR_INSERT_TAB, "Insert Tab"),
    ] {
        register(&mut registry, id, title, "Editor", CommandTarget::View)?;
    }
    for (id, title) in [(EDITOR_COPY, "Copy"), (EDITOR_CUT, "Cut")] {
        let mut descriptor =
            CommandDescriptor::new(CommandId::new(id)?, title, "Editor", CommandTarget::View);
        descriptor.enablement = Enablement::Flag("editor.has_selection".into());
        registry.register(descriptor)?;
    }
    Ok(registry)
}

fn register(
    registry: &mut CommandRegistry,
    id: &str,
    title: &str,
    category: &str,
    target: CommandTarget,
) -> Result<(), CommandError> {
    registry.register(CommandDescriptor::new(
        CommandId::new(id)?,
        title,
        category,
        target,
    ))
}
