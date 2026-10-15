use std::path::PathBuf;
use vanta_commands::{CommandContext, CommandId, CommandRegistry};

#[derive(Debug, Default)]
pub struct CommandPalette {
    query: String,
    selected: usize,
    files: Option<Vec<PathBuf>>,
    search_input: bool,
}

impl CommandPalette {
    #[must_use]
    pub fn files(files: Vec<PathBuf>) -> Self {
        Self {
            files: Some(files),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn search() -> Self {
        Self {
            search_input: true,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn insert(&mut self, text: &str) {
        self.query.push_str(text);
        self.selected = 0;
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
    }

    pub fn move_selection(&mut self, delta: isize, result_count: usize) {
        if result_count == 0 {
            self.selected = 0;
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(result_count - 1);
    }

    #[must_use]
    pub fn results(
        &self,
        registry: &CommandRegistry,
        context: &CommandContext,
        limit: usize,
    ) -> Vec<(CommandId, String)> {
        registry
            .search(&self.query, context)
            .into_iter()
            .take(limit)
            .map(|command| (command.id.clone(), command.title.clone()))
            .collect()
    }

    #[must_use]
    pub fn selected_command(
        &self,
        registry: &CommandRegistry,
        context: &CommandContext,
    ) -> Option<CommandId> {
        if self.files.is_some() || self.search_input {
            return None;
        }
        self.results(registry, context, usize::MAX)
            .get(self.selected)
            .map(|(id, _)| id.clone())
    }

    #[must_use]
    pub fn display_text(&self, registry: &CommandRegistry, context: &CommandContext) -> String {
        let mut text = format!("> {}\n", self.query);
        if self.search_input {
            text.push_str("  Type a literal query and press Enter\n");
            return text;
        }
        if let Some(files) = &self.files {
            for (index, path) in filtered_files(files, &self.query).take(8).enumerate() {
                text.push_str(if index == self.selected { "› " } else { "  " });
                text.push_str(&path.to_string_lossy());
                text.push('\n');
            }
            return text;
        }
        for (index, (_, title)) in self.results(registry, context, 8).into_iter().enumerate() {
            text.push_str(if index == self.selected { "› " } else { "  " });
            text.push_str(&title);
            text.push('\n');
        }
        text
    }

    #[must_use]
    pub fn result_count(&self, registry: &CommandRegistry, context: &CommandContext) -> usize {
        self.files.as_ref().map_or_else(
            || self.results(registry, context, 8).len(),
            |files| filtered_files(files, &self.query).take(8).count(),
        )
    }

    #[must_use]
    pub fn selected_file(&self) -> Option<PathBuf> {
        filtered_files(self.files.as_ref()?, &self.query)
            .nth(self.selected)
            .cloned()
    }

    #[must_use]
    pub const fn is_search_input(&self) -> bool {
        self.search_input
    }
}

fn filtered_files<'a>(files: &'a [PathBuf], query: &'a str) -> impl Iterator<Item = &'a PathBuf> {
    let query = query.to_ascii_lowercase();
    files.iter().filter(move |path| {
        let candidate = path.to_string_lossy().to_ascii_lowercase();
        let mut characters = candidate.chars();
        query
            .chars()
            .all(|wanted| characters.any(|value| value == wanted))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands;

    #[test]
    fn filters_navigates_and_selects_commands() {
        let registry = commands::built_in_registry().unwrap();
        let context = CommandContext::default();
        let mut palette = CommandPalette::default();
        palette.insert("move");
        let count = palette.results(&registry, &context, 20).len();
        assert!(count >= 2);
        let first = palette.selected_command(&registry, &context).unwrap();
        palette.move_selection(1, count);
        assert_ne!(palette.selected_command(&registry, &context), Some(first));
        palette.backspace();
        assert_eq!(palette.query(), "mov");
    }
}
