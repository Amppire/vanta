//! Context-aware, platform-independent keybinding resolution.

use std::collections::HashMap;
use std::fmt;

use vanta_commands::CommandId;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KeyStroke {
    pub key: String,
    modifiers: u8,
}

const CONTROL: u8 = 1;
const ALT: u8 = 2;
const SHIFT: u8 = 4;
const COMMAND: u8 = 8;

impl KeyStroke {
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into().to_ascii_lowercase(),
            modifiers: 0,
        }
    }

    #[must_use]
    pub const fn command(mut self) -> Self {
        self.modifiers |= COMMAND;
        self
    }

    #[must_use]
    pub const fn shift(mut self) -> Self {
        self.modifiers |= SHIFT;
        self
    }

    #[must_use]
    pub const fn control(mut self) -> Self {
        self.modifiers |= CONTROL;
        self
    }

    #[must_use]
    pub const fn alt(mut self) -> Self {
        self.modifiers |= ALT;
        self
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KeyChord(pub Vec<KeyStroke>);

impl From<KeyStroke> for KeyChord {
    fn from(value: KeyStroke) -> Self {
        Self(vec![value])
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum When {
    Always,
    Flag(String),
    NotFlag(String),
}

impl When {
    fn matches(&self, context: &KeyContext) -> bool {
        match self {
            Self::Always => true,
            Self::Flag(flag) => context.flag(flag),
            Self::NotFlag(flag) => !context.flag(flag),
        }
    }

    const fn specificity(&self) -> usize {
        match self {
            Self::Always => 0,
            Self::Flag(_) | Self::NotFlag(_) => 1,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct KeyContext {
    flags: HashMap<String, bool>,
}

impl KeyContext {
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
pub struct Binding {
    pub chord: KeyChord,
    pub command: CommandId,
    pub when: When,
    pub priority: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Conflict {
    pub chord: KeyChord,
    pub commands: Vec<CommandId>,
}

#[derive(Clone, Debug, Default)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Keymap {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    pub fn bind(&mut self, binding: Binding) {
        self.bindings.push(binding);
    }

    #[must_use]
    pub fn resolve(&self, chord: &KeyChord, context: &KeyContext) -> Option<&CommandId> {
        self.bindings
            .iter()
            .enumerate()
            .filter(|(_, binding)| binding.chord == *chord && binding.when.matches(context))
            .max_by_key(|(index, binding)| (binding.priority, binding.when.specificity(), *index))
            .map(|(_, binding)| &binding.command)
    }

    #[must_use]
    pub fn conflicts(&self, context: &KeyContext) -> Vec<Conflict> {
        let mut by_chord: HashMap<&KeyChord, Vec<&Binding>> = HashMap::new();
        for binding in self
            .bindings
            .iter()
            .filter(|item| item.when.matches(context))
        {
            by_chord.entry(&binding.chord).or_default().push(binding);
        }
        by_chord
            .into_iter()
            .filter_map(|(chord, bindings)| {
                let highest = bindings.iter().map(|item| item.priority).max()?;
                let commands: Vec<_> = bindings
                    .into_iter()
                    .filter(|item| item.priority == highest)
                    .map(|item| item.command.clone())
                    .collect();
                (commands.len() > 1).then(|| Conflict {
                    chord: chord.clone(),
                    commands,
                })
            })
            .collect()
    }
}

impl fmt::Display for KeyStroke {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.modifiers & COMMAND != 0 {
            formatter.write_str("Cmd+")?;
        }
        if self.modifiers & CONTROL != 0 {
            formatter.write_str("Ctrl+")?;
        }
        if self.modifiers & ALT != 0 {
            formatter.write_str("Alt+")?;
        }
        if self.modifiers & SHIFT != 0 {
            formatter.write_str("Shift+")?;
        }
        formatter.write_str(&self.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(name: &str) -> CommandId {
        CommandId::new(name).unwrap()
    }

    #[test]
    fn contextual_binding_beats_global_binding() {
        let chord = KeyChord::from(KeyStroke::new("k").command());
        let mut map = Keymap::new();
        map.bind(Binding {
            chord: chord.clone(),
            command: command("workspace.quick_open"),
            when: When::Always,
            priority: 0,
        });
        map.bind(Binding {
            chord: chord.clone(),
            command: command("palette.next"),
            when: When::Flag("palette.open".into()),
            priority: 0,
        });
        let context = KeyContext::default().with_flag("palette.open", true);
        assert_eq!(
            map.resolve(&chord, &context),
            Some(&command("palette.next"))
        );
    }

    #[test]
    fn reports_equal_priority_conflicts() {
        let chord = KeyChord::from(KeyStroke::new("s").command());
        let mut map = Keymap::new();
        for name in ["file.save", "file.save_all"] {
            map.bind(Binding {
                chord: chord.clone(),
                command: command(name),
                when: When::Always,
                priority: 0,
            });
        }
        assert_eq!(map.conflicts(&KeyContext::default()).len(), 1);
    }
}
