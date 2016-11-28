use flow;
use petgraph::graph::NodeIndex;
use flow::prelude::*;

use ops;
use shortcut;

macro_rules! broadcast {
    ($from:expr, $handoffs:ident, $m:expr, $children:expr) => {{
        let c = $children;
        let mut m = $m;
        m.from = $from;
        let mut m = Some(m); // so we can .take() below
        for (i, to) in c.iter().enumerate() {
            let u = if i == c.len() - 1 {
                m.take()
            } else {
                m.clone()
            };

            $handoffs.get_mut(to).unwrap().push_back(u.unwrap());
        }
    }}
}

pub struct NodeDescriptor {
    pub index: NodeIndex,
    pub addr: NodeAddress,
    pub inner: Node,
    pub children: Vec<NodeAddress>,
}

impl NodeDescriptor {
    pub fn process(&mut self,
                   m: Message,
                   state: &mut StateMap,
                   nodes: &DomainNodes)
                   -> Option<Update> {

        // i wish we could use match here, but unfortunately the borrow checker isn't quite
        // sophisticated enough to deal with match and DerefMut in a way that lets us do it
        //
        //   https://github.com/rust-lang/rust/issues/37949
        //
        // so, instead, we have to do it in two parts
        if let flow::node::Type::Ingress(..) = *self.inner {
            self.materialize(&m.data, state);
            return Some(m.data);
        }

        if let flow::node::Type::Egress(_, ref txs) = *self.inner {
            // send any queued updates to all external children
            let mut txs = txs.lock().unwrap();
            let txn = txs.len() - 1;

            let mut u = Some(m.data); // so we can use .take()
            for (txi, &mut (dst, ref mut tx)) in txs.iter_mut().enumerate() {
                if txi == txn && self.children.is_empty() {
                        tx.send(Message {
                            from: NodeAddress::make_global(self.index), // the ingress node knows where it should go
                            to: dst,
                            data: u.take().unwrap(),
                        })
                    } else {
                        tx.send(Message {
                            from: NodeAddress::make_global(self.index),
                            to: dst,
                            data: u.clone().unwrap(),
                        })
                    }
                    .unwrap();
            }

            debug_assert!(u.is_some() || self.children.is_empty());
            return u;
        }

        let u = if let flow::node::Type::Internal(_, ref mut i) = *self.inner {
            i.on_input(m, nodes, state)
        } else {
            unreachable!();
        };

        if let Some(ref u) = u {
            self.materialize(u, state);
        }
        u
    }

    fn materialize(&mut self, u: &Update, state: &mut StateMap) {
        // our output changed -- do we need to modify materialized state?

        // TODO
        // turns out, this lookup is on a serious fast-path, and the hashing is actually a
        // bottleneck. we could probably make a Vec of Option<State> instead by fiddling around
        // with indices, but that is a task for another day
        let state = state.get_mut(&self.addr.as_local());
        if state.is_none() {
            // nope
            return;
        }

        // yes!
        let mut state = state.unwrap();
        match u {
            &ops::Update::Records(ref rs) => {
                for r in rs {
                    match *r {
                        ops::Record::Positive(ref r) => state.insert(r.clone()),
                        ops::Record::Negative(ref r) => {
                            // we need a cond that will match this row.
                            let conds = r.iter()
                                .enumerate()
                                .map(|(coli, v)| {
                                    shortcut::Condition {
                                        column: coli,
                                        cmp: shortcut::Comparison::Equal(shortcut::Value::using(v)),
                                    }
                                })
                                .collect::<Vec<_>>();

                            // however, multiple rows may have the same values as this row for
                            // every column. afaict, it is safe to delete any one of these rows. we
                            // do this by returning true for the first invocation of the filter
                            // function, and false for all subsequent invocations.
                            let mut first = true;
                            state.delete_filter(&conds[..], |_| if first {
                                first = false;
                                true
                            } else {
                                false
                            });
                        }
                    }
                }
            }
        }
    }

    pub fn is_internal(&self) -> bool {
        if let flow::node::Type::Internal(..) = *self.inner {
            true
        } else {
            false
        }
    }
}

use std::ops::Deref;
impl Deref for NodeDescriptor {
    type Target = Node;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
