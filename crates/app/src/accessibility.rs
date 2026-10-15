use accesskit::{
    ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, Node, NodeId, Role,
    TreeId, TreeInfo, TreeUpdate,
};

const ROOT: NodeId = NodeId(0);
const EDITOR: NodeId = NodeId(1);

pub struct InitialTree {
    pub text: String,
}

impl ActivationHandler for InitialTree {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        Some(tree_update(&self.text, true))
    }
}

pub struct IgnoreActions;

impl ActionHandler for IgnoreActions {
    fn do_action(&mut self, _request: ActionRequest) {}
}

pub struct KeepTree;

impl DeactivationHandler for KeepTree {
    fn deactivate_accessibility(&mut self) {}
}

#[must_use]
pub fn tree_update(text: &str, initial: bool) -> TreeUpdate {
    let mut root = Node::new(Role::Application);
    root.set_label("Vanta");
    root.set_children(vec![EDITOR]);
    let mut editor = Node::new(Role::MultilineTextInput);
    editor.set_label("Editor");
    editor.set_value(text);
    TreeUpdate {
        nodes: vec![(ROOT, root), (EDITOR, editor)],
        tree: initial.then(|| TreeInfo {
            root: ROOT,
            toolkit_name: Some("Vanta".to_owned()),
            toolkit_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
        tree_id: TreeId::ROOT,
        focus: EDITOR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_editor_as_focused_multiline_input() {
        let update = tree_update("hello", true);
        assert_eq!(update.focus, EDITOR);
        assert_eq!(update.nodes[1].1.role(), Role::MultilineTextInput);
        assert_eq!(update.nodes[1].1.value(), Some("hello"));
    }
}
