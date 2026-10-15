//! The command catalog shared by Vanta UI surfaces and future extensions.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::str::FromStr;

/// A stable dotted identifier such as `file.save` or `editor.select_all`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CommandId(String);

impl CommandId {
    /// Validates and constructs a command identifier.
    ///
    /// # Errors
    ///
    /// Returns an error unless the ID contains at least two non-empty dotted
    /// segments made from lowercase ASCII letters, digits, `_`, or `-`.
    pub fn new(id: impl Into<String>) -> Result<Self, CommandError> {
        let id = id.into();
        let mut segments = id.split('.');
        let valid_segment = |segment: &str| {
            !segment.is_empty()
                && segment.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
        };
        let first = segments.next();
        let second = segments.next();
        if first.is_none_or(|segment| !valid_segment(segment))
            || second.is_none_or(|segment| !valid_segment(segment))
            || segments.any(|segment| !valid_segment(segment))
        {
            return Err(CommandError::InvalidId(id));
        }
        Ok(Self(id))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for CommandId {
    type Err = CommandError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandTarget {
    Application,
    Workspace,
    Document,
    View,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandValue {
    Boolean(bool),
    Integer(i64),
    String(String),
}

pub type CommandArguments = BTreeMap<String, CommandValue>;

/// Boolean command enablement evaluated against named context flags.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Enablement {
    #[default]
    Always,
    Flag(String),
    Not(Box<Self>),
    All(Vec<Self>),
    Any(Vec<Self>),
}

impl Enablement {
    #[must_use]
    pub fn evaluate(&self, context: &CommandContext) -> bool {
        match self {
            Self::Always => true,
            Self::Flag(flag) => context.flag(flag),
            Self::Not(expression) => !expression.evaluate(context),
            Self::All(expressions) => expressions.iter().all(|item| item.evaluate(context)),
            Self::Any(expressions) => expressions.iter().any(|item| item.evaluate(context)),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommandContext {
    flags: BTreeMap<String, bool>,
}

impl CommandContext {
    #[must_use]
    pub fn with_flag(mut self, name: impl Into<String>, value: bool) -> Self {
        self.flags.insert(name.into(), value);
        self
    }

    #[must_use]
    pub fn flag(&self, name: &str) -> bool {
        self.flags.get(name).copied().unwrap_or(false)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandDescriptor {
    pub id: CommandId,
    pub title: String,
    pub category: String,
    pub target: CommandTarget,
    pub repeatable: bool,
    pub enablement: Enablement,
}

impl CommandDescriptor {
    #[must_use]
    pub fn new(
        id: CommandId,
        title: impl Into<String>,
        category: impl Into<String>,
        target: CommandTarget,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            category: category.into(),
            target,
            repeatable: false,
            enablement: Enablement::Always,
        }
    }
}

/// Metadata registry used by keybindings, menus, palettes, tests, and plugins.
#[derive(Clone, Debug, Default)]
pub struct CommandRegistry {
    commands: HashMap<CommandId, CommandDescriptor>,
}

impl CommandRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one unique command.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is already registered.
    pub fn register(&mut self, descriptor: CommandDescriptor) -> Result<(), CommandError> {
        if self.commands.contains_key(&descriptor.id) {
            return Err(CommandError::Duplicate(descriptor.id));
        }
        self.commands.insert(descriptor.id.clone(), descriptor);
        Ok(())
    }

    #[must_use]
    pub fn get(&self, id: &CommandId) -> Option<&CommandDescriptor> {
        self.commands.get(id)
    }

    #[must_use]
    pub fn is_enabled(&self, id: &CommandId, context: &CommandContext) -> bool {
        self.get(id)
            .is_some_and(|descriptor| descriptor.enablement.evaluate(context))
    }

    /// Returns commands ranked by simple ordered-character matching across the
    /// title, category, and ID. This is deterministic and sufficient for the
    /// first command-palette implementation.
    #[must_use]
    pub fn search(&self, query: &str, context: &CommandContext) -> Vec<&CommandDescriptor> {
        let query = query.to_ascii_lowercase();
        let mut matches: Vec<_> = self
            .commands
            .values()
            .filter(|command| command.enablement.evaluate(context))
            .filter_map(|command| {
                let haystack = format!(
                    "{} {} {}",
                    command.title.to_ascii_lowercase(),
                    command.category.to_ascii_lowercase(),
                    command.id.as_str()
                );
                fuzzy_score(&haystack, &query).map(|score| (score, command))
            })
            .collect();
        matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.title.cmp(&right.title))
        });
        matches.into_iter().map(|(_, command)| command).collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

fn fuzzy_score(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let mut score = 0;
    let mut previous = None;
    let mut characters = haystack.char_indices();
    for wanted in needle.chars() {
        let (index, _) = characters.find(|(_, candidate)| *candidate == wanted)?;
        score += 1;
        if previous.is_some_and(|previous| index == previous + 1) {
            score += 3;
        }
        previous = Some(index);
    }
    Some(score)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandError {
    InvalidId(String),
    Duplicate(CommandId),
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(id) => write!(formatter, "`{id}` is not a valid command identifier"),
            Self::Duplicate(id) => write!(formatter, "command `{id}` is already registered"),
        }
    }
}

impl std::error::Error for CommandError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(id: &str, title: &str) -> CommandDescriptor {
        CommandDescriptor::new(
            CommandId::new(id).unwrap(),
            title,
            "Editor",
            CommandTarget::View,
        )
    }

    #[test]
    fn validates_stable_command_ids() {
        assert!(CommandId::new("file.save").is_ok());
        assert!(CommandId::new("editor.select_all").is_ok());
        assert!(CommandId::new("Save").is_err());
        assert!(CommandId::new("single").is_err());
        assert!(CommandId::new("editor..save").is_err());
    }

    #[test]
    fn rejects_duplicate_registration() {
        let mut registry = CommandRegistry::new();
        registry.register(descriptor("file.save", "Save")).unwrap();
        assert!(matches!(
            registry.register(descriptor("file.save", "Save Again")),
            Err(CommandError::Duplicate(_))
        ));
    }

    #[test]
    fn enablement_uses_explicit_context_flags() {
        let id = CommandId::new("editor.copy").unwrap();
        let mut copy = descriptor("editor.copy", "Copy");
        copy.enablement = Enablement::Flag("editor.has_selection".into());
        let mut registry = CommandRegistry::new();
        registry.register(copy).unwrap();

        assert!(!registry.is_enabled(&id, &CommandContext::default()));
        assert!(registry.is_enabled(
            &id,
            &CommandContext::default().with_flag("editor.has_selection", true)
        ));
    }

    #[test]
    fn search_is_deterministic_and_hides_disabled_commands() {
        let mut registry = CommandRegistry::new();
        registry
            .register(descriptor("editor.select_all", "Select All"))
            .unwrap();
        let mut copy = descriptor("editor.copy", "Copy Selection");
        copy.enablement = Enablement::Flag("editor.has_selection".into());
        registry.register(copy).unwrap();

        let context = CommandContext::default();
        let results = registry.search("sal", &context);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.as_str(), "editor.select_all");
    }
}
