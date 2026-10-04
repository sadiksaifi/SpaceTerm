// Licensed under the Apache License, Version 2.0 or the MIT license.

use crate::NodeId;
use alloc::{string::String, sync::Arc, vec::Vec};

#[derive(Debug)]
pub(crate) struct TextIndex {
    children: Option<(Arc<Self>, Arc<Self>)>,
    leaf: Option<(NodeId, String)>,
    pub(crate) count: usize,
    pub(crate) scalars: usize,
}

impl TextIndex {
    pub(crate) fn build(runs: &[(NodeId, &str)]) -> Arc<Self> {
        if runs.len() == 1 {
            return Arc::new(Self {
                children: None,
                leaf: Some((runs[0].0, runs[0].1.into())),
                count: 1,
                scalars: runs[0].1.chars().count(),
            });
        }
        let middle = runs.len() / 2;
        Self::branch(Self::build(&runs[..middle]), Self::build(&runs[middle..]))
    }

    fn branch(left: Arc<Self>, right: Arc<Self>) -> Arc<Self> {
        Arc::new(Self {
            count: left.count + right.count,
            scalars: left.scalars + right.scalars,
            children: Some((left, right)),
            leaf: None,
        })
    }

    pub(crate) fn replace(self: &Arc<Self>, position: usize, id: NodeId, value: &str) -> Arc<Self> {
        if let Some((old_id, old_value)) = &self.leaf {
            return if *old_id == id && old_value == value {
                Arc::clone(self)
            } else {
                Self::build(&[(id, value)])
            };
        }
        let (left, right) = self.children.as_ref().expect("text index branch");
        let (new_left, new_right) = if position < left.count {
            (left.replace(position, id, value), Arc::clone(right))
        } else {
            (
                Arc::clone(left),
                right.replace(position - left.count, id, value),
            )
        };
        if Arc::ptr_eq(left, &new_left) && Arc::ptr_eq(right, &new_right) {
            Arc::clone(self)
        } else {
            Self::branch(new_left, new_right)
        }
    }

    pub(crate) fn offset(&self, position: usize) -> usize {
        let Some((left, right)) = &self.children else {
            return 0;
        };
        if position < left.count {
            left.offset(position)
        } else {
            left.scalars + right.offset(position - left.count)
        }
    }

    pub(crate) fn changed(&self, old: &Self, offset: usize, changes: &mut Vec<(NodeId, usize)>) {
        if core::ptr::eq(self, old) {
            return;
        }
        if let (Some((left, right)), Some((old_left, old_right))) = (&self.children, &old.children)
        {
            left.changed(old_left, offset, changes);
            right.changed(old_right, offset + left.scalars, changes);
        } else if let Some((id, _)) = &self.leaf {
            changes.push((*id, offset));
        }
    }
}
