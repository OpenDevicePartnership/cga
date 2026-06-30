use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ops::RangeInclusive,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FilePosition {
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span(pub RangeInclusive<FilePosition>);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Location {
    pub path: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    pub location: Location,
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Eq, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(u32);

impl From<u32> for NodeId {
    fn from(value: u32) -> Self {
        NodeId(value)
    }
}

impl Into<u32> for NodeId {
    fn into(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeKind {
    /// Directly called a function.
    Called(NodeId),

    /// Possibly called a function as part of a vtable call.
    VTableCalled(NodeId),

    /// Created a function pointer, hence we must assume it was called.
    FnPtrCast(NodeId),

    /// A builtin Rust MIR assert was called.
    ///
    /// Useful for panic analysis.
    BuiltinAssert,

    /// The implicit panic(s) associated with any async coroutine.
    ///
    /// All async coroutines have a "poll after Ready" state, which always panics.
    /// These panics are (as of yet) unavoidable when using async.
    ///
    /// Because there is no mitigation for this, we put these panics into a separate category.
    AsyncPanic,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Edge {
    pub kind: EdgeKind,

    /// The location associated with the edge.
    ///
    /// An example would be the location of the expression which is a function call or a pointer cast.
    pub location: Location,
}

#[derive(Default, Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
pub struct Callgraph {
    pub roots: BTreeSet<NodeId>,
    pub nodes: Vec<Node>,
    pub edges: BTreeMap<NodeId, Vec<Edge>>,
}

impl EdgeKind {
    pub fn map(&self, mut f: impl FnMut(&NodeId) -> NodeId) -> EdgeKind {
        match self {
            EdgeKind::Called(node_id) => EdgeKind::Called(f(node_id)),
            EdgeKind::VTableCalled(node_id) => EdgeKind::VTableCalled(f(node_id)),
            EdgeKind::FnPtrCast(node_id) => EdgeKind::FnPtrCast(f(node_id)),
            EdgeKind::BuiltinAssert => EdgeKind::BuiltinAssert,
            EdgeKind::AsyncPanic => EdgeKind::AsyncPanic,
        }
    }
}

impl Callgraph {
    pub fn merge<'a>(others: impl Iterator<Item = Callgraph>) -> Callgraph {
        let mut result = Callgraph::default();
        let mut node_dict = HashMap::new();

        for other in others {
            for node in other.nodes.iter() {
                node_dict.entry(node.clone()).or_insert_with(|| {
                    let node_id = NodeId(result.nodes.len() as u32);
                    result.nodes.push(node.clone());
                    node_id
                });
            }

            for root in other.roots.iter() {
                result.roots.insert(node_dict[&other.nodes[root.0 as usize]]);
            }

            for (from, tos) in other.edges.iter() {
                let from = node_dict[&other.nodes[from.0 as usize]];
                let new_tos = result
                    .edges
                    .entry(from)
                    .or_insert_with(|| Vec::with_capacity(tos.len()));

                for to in tos {
                    let new_to = Edge {
                        kind: to.kind.map(|to| node_dict[&other.nodes[to.0 as usize]]),
                        location: to.location.clone(),
                    };

                    if !new_tos.contains(&new_to) {
                        new_tos.push(new_to);
                    }
                }
            }
        }

        result
    }
}
