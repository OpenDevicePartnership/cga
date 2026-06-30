use cga_data::callgraph::{Callgraph, EdgeKind, NodeId};
use std::io::Write;

pub fn emit_dot(callgraph: &Callgraph, mut out: impl Write) -> anyhow::Result<()> {
    out.write_fmt(format_args!("digraph {{\n"))?;
    out.write_fmt(format_args!("\tgraph [ splines=true overlap=false ]\n"))?;

    for (node_id, node) in callgraph.nodes.iter().enumerate() {
        let node_id = NodeId::from(node_id as u32);

        const NAME_MAX_LEN: usize = 100;
        let name = if node.name.len() > NAME_MAX_LEN {
            format!("{}...", &node.name[0..NAME_MAX_LEN])
        } else {
            node.name.clone()
        };

        let is_root = callgraph.roots.contains(&node_id);
        let decoration = if is_root {
            format!("penwidth=\"3\" ")
        } else {
            String::new()
        };

        out.write_fmt(format_args!(
            "\t{} [ label = {:?} {decoration}]\n",
            Into::<u32>::into(node_id),
            name
        ))?;
    }

    for (from, tos) in callgraph.edges.iter() {
        let from = Into::<u32>::into(*from);
        for to in tos {
            let edge = match to.kind {
                EdgeKind::Called(node_id) | EdgeKind::FnPtrCast(node_id) => {
                    let to = Into::<u32>::into(node_id);
                    format!("{from} -> {to}")
                }
                EdgeKind::VTableCalled(node_id) => {
                    let to = Into::<u32>::into(node_id);
                    format!("{from} -> {to} [style=dashed]")
                }
                EdgeKind::BuiltinAssert => format!("{from} -> assert"),
                EdgeKind::AsyncPanic => format!("{from} -> async_panic"),
            };

            out.write_fmt(format_args!("\t{edge}\n"))?;
        }
    }

    out.write_fmt(format_args!("}}\n"))?;

    Ok(())
}
