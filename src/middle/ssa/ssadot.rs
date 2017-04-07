// Copyright (c) 2015, The Radare Project. All rights reserved.
// See the COPYING file at the top-level directory of this distribution.
// Licensed under the BSD 3-Clause License:
// <http://opensource.org/licenses/BSD-3-Clause>
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Implements the `GraphDot` trait for `SSAStorage`

use petgraph::graph;
use petgraph::visit::{IntoEdgeReferences, EdgeRef};

use middle::ir::MOpcode;
use middle::dot::{DotAttrBlock, GraphDot};
use super::ssastorage::{EdgeData, NodeData, SSAStorage};
use super::ssa_traits::{SSA, SSAExtra, ValueType};
use middle::ssa::cfg_traits::CFG;

///////////////////////////////////////////////////////////////////////////////
//// Implementation of GraphDot to emit Dot for SSAStorage.
///////////////////////////////////////////////////////////////////////////////

impl GraphDot for SSAStorage {
	type NodeIndex = graph::NodeIndex;
	type EdgeIndex = graph::EdgeIndex;

    fn configure(&self) -> String {
        "digraph cfg {\nsplines=\"ortho\";\ngraph [fontsize=12 fontname=\"Verdana\" compound=true \
         rankdir=TB;]\n"
            .to_owned()
    }

    fn nodes(&self) -> Vec<Self::NodeIndex> {
        self.valid_nodes()
    }

    fn edges(&self) -> Vec<Self::EdgeIndex> {
        self.g.edge_references().map(|x| x.id()).collect()
    }

    fn node_count(&self) -> usize {
        self.g.node_count()
    }

    fn edge_count(&self) -> usize {
        self.g.edge_count()
    }

    fn node_index_new(i: usize) -> Self::NodeIndex {
        graph::NodeIndex::new(i)
    }

    fn edge_index_new(i: usize) -> Self::EdgeIndex {
        graph::EdgeIndex::new(i)
    }

    fn node_cluster(&self, i: &Self::NodeIndex) -> Option<usize> {
        match self.g.node_weight(*i) {
            Some(&NodeData::BasicBlock(_)) | Some(&NodeData::DynamicAction) => Some(i.index()),
            _ => Some(self.get_block(i).index()),
        }
    }

    fn edge_source(&self, i: &Self::EdgeIndex) -> Self::NodeIndex {
        let edge = &self.g.edge_references().find(|x| x.id() == *i).expect("Invalid edge index");
        match *edge.weight() {
            EdgeData::Data(_) => edge.target(),
            _ => edge.source(),
        }
    }

    fn edge_target(&self, i: &Self::EdgeIndex) -> Self::NodeIndex {
        let edge = &self.g.edge_references().find(|x| x.id() == *i).expect("Invalid edge index");
         match *edge.weight() {
            EdgeData::Data(_) => edge.source(),
            _ => edge.target(),
        }
    }

    fn edge_skip(&self, i: &Self::EdgeIndex) -> bool {
        let edge = &self.g.edge_references().find(|x| x.id() == *i).expect("Invalid edge index");
        match *edge.weight() {
            EdgeData::ContainedInBB(_) | EdgeData::RegisterState => true,
            _ => false,
        }
    }

    // TODO: Ordering of clusters for ssa is kind of hacky and may not run top to
    // bottom in some
    // cases.
    fn edge_attrs(&self, i: &Self::EdgeIndex) -> DotAttrBlock {
        // TODO: Error Handling
        let edge = self.g.edge_references().find(|x| x.id() == *i).expect("Invalid edge index");
        let mut prefix = String::new();
        let src = edge.source();
        let target = edge.target();

        prefix.push_str(&format!("n{} -> n{}",
                                 src.index(),
                                 target.index()));
        let target_is_bb = if let NodeData::BasicBlock(_) = self.g[edge.target()] {
            true
        } else {
            false
        };
        let attr = match *edge.weight() {
            EdgeData::Control(_) if !target_is_bb => {
                vec![("color".to_string(), "red".to_string())]
            }
            EdgeData::Control(i) => {
                // Determine the source and destination clusters.
                let source_cluster = edge.source().index();
                let dst_cluster = edge.target().index();
                let (color, label) = match i {
                    0 => ("red", "F"),
                    1 => ("green", "T"),
                    2 => ("blue", "U"),
                    _ => unreachable!(),
                };
                vec![("color".to_string(), color.to_string()),
                     ("xlabel".to_string(), label.to_string()),
                     ("ltail".to_string(), format!("cluster_{}", source_cluster)),
                     ("lhead".to_string(), format!("cluster_{}", dst_cluster)),
                     ("minlen".to_string(), "9".to_owned())]
            }
            EdgeData::Data(i) => {
                vec![("dir".to_string(), "back".to_string()),
                     ("xlabel".to_string(), format!("{}", i))]
            }
            EdgeData::ContainedInBB(_) => {
                vec![("color".to_string(), "gray".to_string())]
            }
            EdgeData::Selector => {
                vec![("color".to_string(), "purple".to_string())]
            }
            EdgeData::ReplacedBy => {
                vec![("color".to_string(), "brown".to_string())]
            }
            EdgeData::RegisterState => unreachable!(),
        };

        DotAttrBlock::Hybrid(prefix, attr)
    }

    fn node_attrs(&self, i: &Self::NodeIndex) -> DotAttrBlock {
        let node = &self.g[*i];
        let mut prefix = String::new();
        prefix.push_str(&format!("n{}", i.index()));

        let attr = match *node {
            NodeData::Op(opc, ValueType::Integer{width: w}) => {
                let mut attrs = Vec::new();
                let mut r = String::new();
                let addr = self.addr(i);
                if addr.is_some() {
                    r.push_str(&format!("<<font color=\"grey50\">{}: </font>",
                                        addr.as_ref().unwrap()))
                }
                r.push_str(&format!("\"[i{}] {:?}\"", w, opc));
                if addr.is_some() {
                    r.push_str(">");
                }

                if let MOpcode::OpConst(_) = opc {
                    attrs.push(("style".to_owned(), "filled".to_owned()));
                    attrs.push(("color".to_owned(), "black".to_owned()));
                    attrs.push(("fillcolor".to_owned(), "yellow".to_owned()));
                }

                if self.is_marked(i) {
                    attrs.push(("label".to_string(), r));
                    attrs.push(("style".to_owned(), "filled".to_owned()));
                    attrs.push(("color".to_owned(), "black".to_owned()));
                    attrs.push(("fillcolor".to_owned(), "green".to_owned()));
                } else {
                    attrs.push(("label".to_string(), r));
                }
                attrs
            }
            NodeData::BasicBlock(addr) => {
                let label_str = format!("<<font color=\"grey50\">Basic Block \
                                         Information<br/>Start Address: {}</font>>",
                                        addr);
                let mut attrs = Vec::new();
                if self.start_node() == *i {
                    attrs.push(("rank".to_string(), "min".to_string()));
                }

                attrs.extend([("label".to_string(), label_str),
                              ("shape".to_string(), "box".to_string()),
                              ("color".to_string(), "\"grey\"".to_string())]
                                 .iter()
                                 .cloned());
                attrs
            }
            NodeData::Comment(_, ref msg) => {
                vec![("label".to_string(),
                      format!("\"{}\"", msg.replace("\"", "\\\""))),
                     ("shape".to_owned(), "box".to_owned()),
                     ("style".to_owned(), "filled".to_owned()),
                     ("color".to_owned(), "black".to_owned()),
                     ("fillcolor".to_owned(), "greenyellow".to_owned())]
            }
            _ => {
                let mut attrs = Vec::new();
                let mut label = format!("{:?}", node);
                label = label.replace("\"", "\\\"");
                label = format!("\"{}\"", label);
                if let Some(addr) = self.addr(i) {
                    label = format!("<<font color=\"grey50\">{}: </font> {}>", addr, label);
                }
                attrs.push(("label".to_string(), label));
                if self.is_marked(i) {
                    attrs.push(("style".to_owned(), "filled".to_owned()));
                    attrs.push(("color".to_owned(), "black".to_owned()));
                    attrs.push(("fillcolor".to_owned(), "green".to_owned()));
                }
                attrs
            }
        };
        DotAttrBlock::Hybrid(prefix, attr)
    }
}
