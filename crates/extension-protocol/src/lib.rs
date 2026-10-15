//! Versioned, bounded line protocol for the out-of-process extension host.

pub const PROTOCOL_VERSION: u16 = 1;
pub const PREVIOUS_PROTOCOL_VERSION: u16 = 0;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Permission {
    ReadDocument,
    EditDocument,
    WorkspaceRead,
    Decorations,
    StatusItem,
    DeclarativeView,
    LanguageServer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleAction {
    Disable,
    Reload,
    Inspect,
}

pub const MAX_VIEW_NODES: usize = 1_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewNode {
    Text(String),
    Status {
        label: String,
        value: String,
    },
    Action {
        label: String,
        command: String,
    },
    Group {
        title: String,
        children: Vec<ViewNode>,
    },
}

impl ViewNode {
    /// Validates a host-owned declarative view without executing extension code.
    ///
    /// # Errors
    /// Rejects control characters, invalid commands, or trees over the node budget.
    pub fn validate(&self) -> Result<(), &'static str> {
        fn visit(node: &ViewNode, count: &mut usize) -> Result<(), &'static str> {
            *count = count.saturating_add(1);
            if *count > MAX_VIEW_NODES {
                return Err("view exceeds node limit");
            }
            match node {
                ViewNode::Text(text) => validate_field(text),
                ViewNode::Status { label, value } => {
                    validate_field(label)?;
                    validate_field(value)
                }
                ViewNode::Action { label, command } => {
                    validate_field(label)?;
                    validate_field(command)?;
                    if command.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
                    }) {
                        Ok(())
                    } else {
                        Err("invalid command")
                    }
                }
                ViewNode::Group { title, children } => {
                    validate_field(title)?;
                    for child in children {
                        visit(child, count)?;
                    }
                    Ok(())
                }
            }
        }
        visit(self, &mut 0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
    Ping {
        id: u64,
    },
    Execute {
        id: u64,
        module: String,
        fuel: u64,
    },
    Cancel {
        id: u64,
    },
    Lifecycle {
        id: u64,
        extension: String,
        action: LifecycleAction,
    },
    Shutdown,
}

impl Request {
    /// Decodes one bounded tab-separated frame.
    ///
    /// # Errors
    /// Returns an error for oversized, malformed, or unsupported frames.
    pub fn decode(frame: &str) -> Result<Self, &'static str> {
        if frame.len() > MAX_FRAME_BYTES {
            return Err("frame too large");
        }
        let fields: Vec<_> = frame.trim_end().split('\t').collect();
        match fields.as_slice() {
            ["1" | "0", "ping", id] => Ok(Self::Ping {
                id: id.parse().map_err(|_| "invalid id")?,
            }),
            ["1" | "0", "execute", id, module, fuel] => {
                validate_field(module)?;
                Ok(Self::Execute {
                    id: id.parse().map_err(|_| "invalid id")?,
                    module: (*module).to_owned(),
                    fuel: fuel.parse().map_err(|_| "invalid fuel")?,
                })
            }
            ["1" | "0", "cancel", id] => Ok(Self::Cancel {
                id: id.parse().map_err(|_| "invalid id")?,
            }),
            ["1", "lifecycle", id, extension, action] => {
                validate_field(extension)?;
                let action = match *action {
                    "disable" => LifecycleAction::Disable,
                    "reload" => LifecycleAction::Reload,
                    "inspect" => LifecycleAction::Inspect,
                    _ => return Err("invalid lifecycle action"),
                };
                Ok(Self::Lifecycle {
                    id: id.parse().map_err(|_| "invalid id")?,
                    extension: (*extension).to_owned(),
                    action,
                })
            }
            ["1" | "0", "shutdown"] => Ok(Self::Shutdown),
            [version, ..] if !matches!(*version, "1" | "0") => Err("unsupported protocol version"),
            _ => Err("malformed frame"),
        }
    }
}

fn validate_field(field: &str) -> Result<(), &'static str> {
    if field.is_empty() || field.chars().any(char::is_control) {
        Err("invalid field")
    } else {
        Ok(())
    }
}

#[must_use]
pub const fn is_compatible_protocol(version: u16) -> bool {
    version == PROTOCOL_VERSION || version == PREVIOUS_PROTOCOL_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unknown_versions() {
        assert_eq!(
            Request::decode("2\tping\t1"),
            Err("unsupported protocol version")
        );
    }
    #[test]
    fn decodes_execute() {
        assert!(matches!(
            Request::decode("1\texecute\t7\tplugin.wasm\t1000"),
            Ok(Request::Execute {
                id: 7,
                fuel: 1000,
                ..
            })
        ));
    }
    #[test]
    fn decodes_lifecycle_without_allowing_field_injection() {
        assert!(matches!(
            Request::decode("1\tlifecycle\t9\tsample.extension\tdisable"),
            Ok(Request::Lifecycle {
                action: LifecycleAction::Disable,
                ..
            })
        ));
        assert!(Request::decode("1\tlifecycle\t9\tbad\nname\tdisable").is_err());
    }

    #[test]
    fn current_and_previous_protocols_are_compatible() {
        assert!(is_compatible_protocol(PROTOCOL_VERSION));
        assert!(is_compatible_protocol(PREVIOUS_PROTOCOL_VERSION));
        assert!(!is_compatible_protocol(PROTOCOL_VERSION + 1));
    }

    #[test]
    fn declarative_views_are_bounded_and_command_only() {
        let view = ViewNode::Group {
            title: "Problems".to_owned(),
            children: vec![ViewNode::Action {
                label: "Apply fix".to_owned(),
                command: "language.apply_fix".to_owned(),
            }],
        };
        assert_eq!(view.validate(), Ok(()));
        assert!(
            ViewNode::Action {
                label: "bad".to_owned(),
                command: "shell rm".to_owned(),
            }
            .validate()
            .is_err()
        );
    }
}
