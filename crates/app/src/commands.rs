use vanta_commands::{
    CommandDescriptor, CommandError, CommandId, CommandRegistry, CommandTarget, Enablement,
};

pub const FILE_SAVE: &str = "file.save";
pub const EDITOR_SELECT_ALL: &str = "editor.select_all";
pub const EDITOR_COPY: &str = "editor.copy";
pub const EDITOR_CUT: &str = "editor.cut";
pub const EDITOR_PASTE: &str = "editor.paste";
pub const EDITOR_BACKSPACE: &str = "editor.backspace";
pub const EDITOR_DELETE: &str = "editor.delete";
pub const EDITOR_MOVE_LEFT: &str = "editor.move_left";
pub const EDITOR_MOVE_RIGHT: &str = "editor.move_right";
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
