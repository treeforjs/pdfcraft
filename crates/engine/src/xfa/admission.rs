//! Check borrowed form/event strings before constructing a second form tree.
use super::FormNode;

pub(super) fn fits(root: &FormNode, event: &[&str]) -> bool {
    let mut remaining = 4usize * 1024 * 1024;
    for text in event {
        let Some(left) = remaining.checked_sub(text.len().saturating_add(32)) else { return false };
        remaining = left;
    }
    let mut stack = vec![std::slice::from_ref(root).iter()];
    while let Some(level) = stack.last_mut() {
        let Some(node) = level.next() else {
            stack.pop();
            continue;
        };
        let Some(left) = remaining.checked_sub(256) else { return false };
        remaining = left;
        for text in [&node.name, &node.som, &node.value, &node.presence, &node.access] {
            let Some(left) = remaining.checked_sub(text.len().saturating_add(32)) else { return false };
            remaining = left;
        }
        if !node.children.is_empty() {
            if stack.len() >= 64 {
                return false;
            }
            stack.push(node.children.iter());
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_oversized_nodes_event_strings_and_deep_trees_before_copying() {
        let mut node = FormNode { value: "x".repeat(4 * 1024 * 1024), ..Default::default() };
        assert!(!fits(&node, &[]));
        node.value.clear();
        assert!(!fits(&node, &[&"s".repeat(4 * 1024 * 1024)]));
        for _ in 0..64 {
            node = FormNode { children: vec![node], ..Default::default() };
        }
        assert!(!fits(&node, &[]));
        assert!(fits(&FormNode::default(), &["click", "form[0].button[0]", "1+1"]));
    }

    #[test]
    fn empty_node_metadata_also_consumes_the_aggregate_budget() {
        let node = FormNode { children: vec![FormNode::default(); 20_000], ..Default::default() };
        assert!(!fits(&node, &[]));
    }
}
