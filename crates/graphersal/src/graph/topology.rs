//! The slot store of [`TraversalGraph`](crate::TraversalGraph): vertices and edges in two
//! vectors of slots with stable indices, free lists, and the adjacency as linked lists through
//! the edge slots (book `for_developers/storage_internals.md`).
//!
//! It replaces petgraph's `StableGraph` and keeps its observable behaviour exactly, because the
//! order of `g.V()` (slot order) and of `out()`/`in()` (list order) is what queries see:
//!
//! - a removed slot stays (vacant) so that every other index stays valid; the next insertion
//!   reuses the most recently freed slot first (LIFO free lists), else appends;
//! - every edge is in two lists, the outgoing list of its source and the incoming list of its
//!   target; a new edge is put at the FRONT of both, so a list iterates newest first; a self-loop
//!   is in both lists of its vertex;
//! - removing an edge unlinks it from both lists; removing a vertex removes its edges first.
//!
//! What is new over `StableGraph`: the capacity is under our control ([`Topology::reserve_exact`],
//! [`Topology::shrink_to_fit`]), and the slot figures the memory report needs are exact
//! ([`Topology::vacant_nodes`], [`Topology::vacant_edges`]).

/// The sentinel index: "no slot" (the end of a list, an empty free list).
const END: u32 = u32::MAX;

/// The index of a vertex slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct NodeIndex(u32);

/// The index of an edge slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct EdgeIndex(u32);

impl NodeIndex {
    /// The slot at `index` (truncated to 32 bits, like a handle's slot).
    #[inline]
    pub(crate) fn new(index: usize) -> Self {
        NodeIndex(index as u32)
    }

    /// The slot number.
    #[inline]
    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }

    /// The index of no slot: never occupied, so an operation on it finds nothing.
    #[inline]
    pub(crate) fn end() -> Self {
        NodeIndex(END)
    }
}

impl EdgeIndex {
    /// The slot at `index` (truncated to 32 bits, like a handle's slot).
    #[inline]
    pub(crate) fn new(index: usize) -> Self {
        EdgeIndex(index as u32)
    }

    /// The slot number.
    #[inline]
    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }

    /// The index of no slot.
    #[inline]
    pub(crate) fn end() -> Self {
        EdgeIndex(END)
    }

    #[inline]
    fn is_end(self) -> bool {
        self.0 == END
    }
}

/// The two adjacency lists of a vertex, or the two link directions of an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    /// Edges leaving the vertex (list 0).
    Outgoing,
    /// Edges arriving at the vertex (list 1).
    Incoming,
}

impl Direction {
    #[inline]
    fn list(self) -> usize {
        match self {
            Direction::Outgoing => 0,
            Direction::Incoming => 1,
        }
    }
}

/// A vertex slot. Occupied: `next` holds the heads of the outgoing and incoming lists. Vacant:
/// `next[0]` / `next[1]` are the next and previous slots of the free list (as raw indices).
#[derive(Debug, Clone)]
pub(crate) struct NodeSlot<N> {
    weight: Option<N>,
    next: [u32; 2],
}

/// An edge slot: its weight, its source and target, and the next edge of the source's outgoing
/// list and of the target's incoming list. Vacant: `next[0]` is the next slot of the free list.
#[derive(Debug, Clone)]
pub(crate) struct EdgeSlot<E> {
    weight: Option<E>,
    node: [NodeIndex; 2],
    next: [EdgeIndex; 2],
}

/// Vertices `N` and directed edges `E` in slots with stable indices (module docs).
#[derive(Debug, Clone)]
pub(crate) struct Topology<N, E> {
    nodes: Vec<NodeSlot<N>>,
    edges: Vec<EdgeSlot<E>>,
    node_count: usize,
    edge_count: usize,
    /// The most recently freed vertex slot (head of a doubly linked free list).
    free_node: u32,
    /// The most recently freed edge slot (head of a singly linked free list).
    free_edge: EdgeIndex,
}

impl<N, E> Default for Topology<N, E> {
    fn default() -> Self {
        Topology {
            nodes: Vec::new(),
            edges: Vec::new(),
            node_count: 0,
            edge_count: 0,
            free_node: END,
            free_edge: EdgeIndex::end(),
        }
    }
}

/// One edge as an iterator yields it: its index, its endpoints and its weight.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EdgeRef<'a, E> {
    index: EdgeIndex,
    node: [NodeIndex; 2],
    weight: &'a E,
}

impl<'a, E> EdgeRef<'a, E> {
    /// The edge's slot.
    #[inline]
    pub(crate) fn id(&self) -> EdgeIndex {
        self.index
    }

    /// The source vertex.
    #[inline]
    pub(crate) fn source(&self) -> NodeIndex {
        self.node[0]
    }

    /// The target vertex.
    #[inline]
    pub(crate) fn target(&self) -> NodeIndex {
        self.node[1]
    }

    /// The weight.
    #[inline]
    pub(crate) fn weight(&self) -> &'a E {
        self.weight
    }
}

impl<N, E> Topology<N, E> {
    /// An empty store.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Live vertices.
    #[inline]
    pub(crate) fn node_count(&self) -> usize {
        self.node_count
    }

    /// Live edges.
    #[inline]
    pub(crate) fn edge_count(&self) -> usize {
        self.edge_count
    }

    /// The allocated vertex and edge slots (live, vacant and never used).
    pub(crate) fn capacity(&self) -> (usize, usize) {
        (self.nodes.capacity(), self.edges.capacity())
    }

    /// Vacant vertex slots: removed vertices whose slots wait for reuse.
    pub(crate) fn vacant_nodes(&self) -> usize {
        self.nodes.len() - self.node_count
    }

    /// Vacant edge slots.
    pub(crate) fn vacant_edges(&self) -> usize {
        self.edges.len() - self.edge_count
    }

    /// The size of one vertex slot, in bytes.
    pub(crate) const fn node_slot_bytes() -> usize {
        std::mem::size_of::<NodeSlot<N>>()
    }

    /// The size of one edge slot, in bytes.
    pub(crate) const fn edge_slot_bytes() -> usize {
        std::mem::size_of::<EdgeSlot<E>>()
    }

    /// Tries to make room for exactly `nodes` more vertices and `edges` more edges (a load that
    /// knows its counts allocates once, without spare capacity). The counts may come from a
    /// file: a reservation the allocator refuses is skipped, never an abort.
    pub(crate) fn reserve_exact(&mut self, nodes: usize, edges: usize) {
        let _ = self.nodes.try_reserve_exact(nodes);
        let _ = self.edges.try_reserve_exact(edges);
    }

    /// Releases the spare capacity of both slot vectors (vacant slots stay: indices are stable).
    pub(crate) fn shrink_to_fit(&mut self) {
        self.nodes.shrink_to_fit();
        self.edges.shrink_to_fit();
    }

    /// Stores a vertex: in the most recently freed slot, else in a new one.
    pub(crate) fn add_node(&mut self, weight: N) -> NodeIndex {
        self.node_count += 1;
        let index = self.free_node;
        if index == END {
            let index = self.nodes.len();
            self.nodes.push(NodeSlot {
                weight: Some(weight),
                next: [END, END],
            });
            return NodeIndex::new(index);
        }
        // Take the head of the free list.
        let (next_free, previous_free) = match self.nodes.get_mut(index as usize) {
            Some(slot) => {
                let links = (slot.next[0], slot.next[1]);
                slot.weight = Some(weight);
                slot.next = [END, END];
                links
            }
            None => (END, END),
        };
        if previous_free != END
            && let Some(slot) = self.nodes.get_mut(previous_free as usize)
        {
            slot.next[0] = next_free;
        }
        if next_free != END
            && let Some(slot) = self.nodes.get_mut(next_free as usize)
        {
            slot.next[1] = previous_free;
        }
        self.free_node = next_free;
        NodeIndex(index)
    }

    /// Stores an edge from `a` to `b` (both live) at the front of their lists: in the most
    /// recently freed slot, else in a new one. `None` (nothing stored) when an endpoint is not a
    /// live vertex.
    pub(crate) fn add_edge(&mut self, a: NodeIndex, b: NodeIndex, weight: E) -> Option<EdgeIndex> {
        let (Some(head_out), Some(head_in)) = (self.live_next(a, 0), self.live_next(b, 1)) else {
            return None;
        };
        let index = if self.free_edge.is_end() {
            EdgeIndex::new(self.edges.len())
        } else {
            self.free_edge
        };
        let slot = EdgeSlot {
            weight: Some(weight),
            node: [a, b],
            next: [EdgeIndex(head_out), EdgeIndex(head_in)],
        };
        if self.free_edge.is_end() {
            self.edges.push(slot);
        } else if let Some(vacant) = self.edges.get_mut(index.index()) {
            self.free_edge = vacant.next[0];
            *vacant = slot;
        }
        if let Some(source) = self.nodes.get_mut(a.index()) {
            source.next[0] = index.0;
        }
        if let Some(target) = self.nodes.get_mut(b.index()) {
            target.next[1] = index.0;
        }
        self.edge_count += 1;
        Some(index)
    }

    /// The head of list `list` of a live vertex `a`, `None` when `a` is not live.
    fn live_next(&self, a: NodeIndex, list: usize) -> Option<u32> {
        self.nodes
            .get(a.index())
            .filter(|slot| slot.weight.is_some())
            .map(|slot| slot.next[list])
    }

    /// Removes the edge `e` from both lists and frees its slot; its weight, or `None` when `e`
    /// is not a live edge.
    pub(crate) fn remove_edge(&mut self, e: EdgeIndex) -> Option<E> {
        let (node, next) = match self.edges.get(e.index()) {
            Some(slot) if slot.weight.is_some() => (slot.node, slot.next),
            _ => return None,
        };
        for direction in [Direction::Outgoing, Direction::Incoming] {
            let list = direction.list();
            let Some(head) = self
                .nodes
                .get(node[list].index())
                .map(|slot| slot.next[list])
            else {
                continue;
            };
            if head == e.0 {
                if let Some(slot) = self.nodes.get_mut(node[list].index()) {
                    slot.next[list] = next[list].0;
                }
                continue;
            }
            // Walk the list to the edge before `e` (an edge is in each list once).
            let mut current = EdgeIndex(head);
            while let Some(slot) = self.edges.get_mut(current.index()) {
                if slot.next[list] == e {
                    slot.next[list] = next[list];
                    break;
                }
                current = slot.next[list];
            }
        }
        let slot = self.edges.get_mut(e.index())?;
        slot.next = [self.free_edge, EdgeIndex::end()];
        slot.node = [NodeIndex::end(), NodeIndex::end()];
        self.free_edge = e;
        self.edge_count -= 1;
        slot.weight.take()
    }

    /// Removes the vertex `a` and every edge at it, frees its slot; its weight, or `None` when
    /// `a` is not a live vertex.
    pub(crate) fn remove_node(&mut self, a: NodeIndex) -> Option<N> {
        let weight = self.nodes.get_mut(a.index())?.weight.take()?;
        for list in 0..2 {
            loop {
                let head = self
                    .nodes
                    .get(a.index())
                    .map_or(END, |slot| slot.next[list]);
                if head == END || self.remove_edge(EdgeIndex(head)).is_none() {
                    break;
                }
            }
        }
        let free = self.free_node;
        if let Some(slot) = self.nodes.get_mut(a.index()) {
            slot.next = [free, END];
        }
        if free != END
            && let Some(slot) = self.nodes.get_mut(free as usize)
        {
            slot.next[1] = a.0;
        }
        self.free_node = a.0;
        self.node_count -= 1;
        Some(weight)
    }

    /// The weight of a live vertex.
    #[inline]
    pub(crate) fn node_weight(&self, a: NodeIndex) -> Option<&N> {
        self.nodes.get(a.index())?.weight.as_ref()
    }

    /// The weight of a live vertex, mutably.
    #[inline]
    pub(crate) fn node_weight_mut(&mut self, a: NodeIndex) -> Option<&mut N> {
        self.nodes.get_mut(a.index())?.weight.as_mut()
    }

    /// The weight of a live edge.
    #[inline]
    pub(crate) fn edge_weight(&self, e: EdgeIndex) -> Option<&E> {
        self.edges.get(e.index())?.weight.as_ref()
    }

    /// The weight of a live edge, mutably.
    #[inline]
    pub(crate) fn edge_weight_mut(&mut self, e: EdgeIndex) -> Option<&mut E> {
        self.edges.get_mut(e.index())?.weight.as_mut()
    }

    /// The source and target of a live edge.
    #[inline]
    pub(crate) fn edge_endpoints(&self, e: EdgeIndex) -> Option<(NodeIndex, NodeIndex)> {
        self.edges
            .get(e.index())
            .filter(|slot| slot.weight.is_some())
            .map(|slot| (slot.node[0], slot.node[1]))
    }

    /// The live vertices in slot order, with their indices.
    pub(crate) fn node_references(&self) -> impl Iterator<Item = (NodeIndex, &N)> + '_ {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.weight.as_ref().map(|w| (NodeIndex::new(i), w)))
    }

    /// The live vertex weights in slot order.
    pub(crate) fn node_weights(&self) -> impl Iterator<Item = &N> + '_ {
        self.nodes.iter().filter_map(|slot| slot.weight.as_ref())
    }

    /// The live edges in slot order.
    pub(crate) fn edge_references(&self) -> impl Iterator<Item = EdgeRef<'_, E>> + '_ {
        self.edges.iter().enumerate().filter_map(|(i, slot)| {
            slot.weight.as_ref().map(|weight| EdgeRef {
                index: EdgeIndex::new(i),
                node: slot.node,
                weight,
            })
        })
    }

    /// The live edge weights in slot order.
    pub(crate) fn edge_weights(&self) -> impl Iterator<Item = &E> + '_ {
        self.edges.iter().filter_map(|slot| slot.weight.as_ref())
    }

    /// The indices of the live edges in slot order.
    pub(crate) fn edge_indices(&self) -> impl Iterator<Item = EdgeIndex> + '_ {
        self.edge_references().map(|edge| edge.index)
    }

    /// The edges of list `direction` of vertex `a`, newest first (nothing for a vertex that is
    /// not live).
    pub(crate) fn edges_directed(&self, a: NodeIndex, direction: Direction) -> Edges<'_, E> {
        let list = direction.list();
        Edges {
            edges: &self.edges,
            list,
            next: EdgeIndex(self.live_next(a, list).unwrap_or(END)),
        }
    }
}

/// The edges of one adjacency list ([`Topology::edges_directed`]).
pub(crate) struct Edges<'a, E> {
    edges: &'a [EdgeSlot<E>],
    list: usize,
    next: EdgeIndex,
}

impl<'a, E> Iterator for Edges<'a, E> {
    type Item = EdgeRef<'a, E>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let index = self.next;
        let slot = self.edges.get(index.index())?;
        let weight = slot.weight.as_ref()?;
        self.next = slot.next[self.list];
        Some(EdgeRef {
            index,
            node: slot.node,
            weight,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(t: &Topology<&'static str, u32>, a: NodeIndex) -> Vec<u32> {
        t.edges_directed(a, Direction::Outgoing)
            .map(|e| *e.weight())
            .collect()
    }

    fn incoming(t: &Topology<&'static str, u32>, a: NodeIndex) -> Vec<u32> {
        t.edges_directed(a, Direction::Incoming)
            .map(|e| *e.weight())
            .collect()
    }

    #[test]
    fn lists_iterate_newest_first_and_self_loops_are_in_both() {
        let mut t = Topology::new();
        let a = t.add_node("a");
        let b = t.add_node("b");
        t.add_edge(a, b, 1).unwrap();
        t.add_edge(a, b, 2).unwrap();
        t.add_edge(b, a, 3).unwrap();
        t.add_edge(a, a, 4).unwrap();
        assert_eq!(out(&t, a), [4, 2, 1]);
        assert_eq!(incoming(&t, a), [4, 3]);
        assert_eq!(incoming(&t, b), [2, 1]);
        assert_eq!((t.node_count(), t.edge_count()), (2, 4));
        assert!(t.add_edge(a, NodeIndex::end(), 9).is_none());
        assert_eq!(t.edge_count(), 4);
    }

    #[test]
    fn freed_slots_are_reused_last_freed_first() {
        let mut t = Topology::new();
        let nodes: Vec<_> = (0..5).map(|_| t.add_node("v")).collect();
        let e0 = t.add_edge(nodes[0], nodes[1], 0).unwrap();
        let e1 = t.add_edge(nodes[1], nodes[2], 1).unwrap();
        let e2 = t.add_edge(nodes[2], nodes[3], 2).unwrap();
        assert_eq!(t.remove_edge(e1), Some(1));
        assert_eq!(t.remove_edge(e0), Some(0));
        assert_eq!(t.remove_edge(e0), None);
        assert_eq!(t.vacant_edges(), 2);
        assert_eq!(t.add_edge(nodes[0], nodes[4], 10), Some(e0));
        assert_eq!(t.add_edge(nodes[0], nodes[4], 11), Some(e1));
        assert_eq!(t.add_edge(nodes[0], nodes[4], 12), Some(EdgeIndex::new(3)));
        // Vertex free list: 1 then 3 freed -> 3 reused first, then 1, then a new slot.
        assert_eq!(t.remove_node(nodes[1]), Some("v"));
        assert_eq!(t.remove_node(nodes[3]), Some("v"));
        assert_eq!(
            t.edge_weight(e2),
            None,
            "the edges of a removed vertex go too"
        );
        assert_eq!(t.vacant_nodes(), 2);
        assert_eq!(t.add_node("x"), nodes[3]);
        assert_eq!(t.add_node("y"), nodes[1]);
        assert_eq!(t.add_node("z"), NodeIndex::new(5));
        let order: Vec<_> = t.node_weights().copied().collect();
        assert_eq!(order, ["v", "y", "v", "x", "v", "z"]);
    }

    #[test]
    fn removing_from_the_middle_of_a_list_keeps_the_rest_in_order() {
        let mut t = Topology::new();
        let a = t.add_node("a");
        let b = t.add_node("b");
        let edges: Vec<_> = (0..5).map(|i| t.add_edge(a, b, i).unwrap()).collect();
        t.remove_edge(edges[2]);
        assert_eq!(out(&t, a), [4, 3, 1, 0]);
        assert_eq!(incoming(&t, b), [4, 3, 1, 0]);
        t.remove_edge(edges[4]);
        t.remove_edge(edges[0]);
        assert_eq!(out(&t, a), [3, 1]);
        assert_eq!(t.edge_endpoints(edges[3]), Some((a, b)));
        assert_eq!(t.edge_endpoints(edges[4]), None);
    }

    #[test]
    fn capacity_is_exact_after_a_reservation_and_a_shrink() {
        let mut t: Topology<&str, u32> = Topology::new();
        t.reserve_exact(10, 20);
        // An impossible reservation is skipped.
        t.reserve_exact(usize::MAX, 0);
        let (nodes, edges) = t.capacity();
        assert!(nodes >= 10 && edges >= 20);
        for _ in 0..3 {
            t.add_node("v");
        }
        t.shrink_to_fit();
        assert_eq!(t.capacity().0, 3);
        assert_eq!(t.capacity().1, 0);
    }

    /// Random insertions and removals against a plain model: slots reused last freed first,
    /// every list newest first, counts and endpoints right.
    #[test]
    fn random_operations_match_a_plain_model() {
        // A small deterministic generator (xorshift).
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        let mut t: Topology<u32, u32> = Topology::new();
        // Model: slot -> Some(stamp) for live elements; edges carry (source, target, stamp).
        let mut nodes: Vec<Option<u32>> = Vec::new();
        let mut edges: Vec<Option<(usize, usize, u32)>> = Vec::new();
        let (mut free_nodes, mut free_edges): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
        let mut stamp = 0u32;
        for _ in 0..4000 {
            stamp += 1;
            let live: Vec<usize> = (0..nodes.len()).filter(|&i| nodes[i].is_some()).collect();
            match next(10) {
                0..=2 => {
                    let expected = free_nodes.pop().unwrap_or(nodes.len());
                    let got = t.add_node(stamp);
                    assert_eq!(got.index(), expected);
                    if expected == nodes.len() {
                        nodes.push(Some(stamp));
                    } else {
                        nodes[expected] = Some(stamp);
                    }
                }
                3..=6 if !live.is_empty() => {
                    let (a, b) = (live[next(live.len())], live[next(live.len())]);
                    let expected = free_edges.pop().unwrap_or(edges.len());
                    let got = t
                        .add_edge(NodeIndex::new(a), NodeIndex::new(b), stamp)
                        .unwrap();
                    assert_eq!(got.index(), expected);
                    if expected == edges.len() {
                        edges.push(Some((a, b, stamp)));
                    } else {
                        edges[expected] = Some((a, b, stamp));
                    }
                }
                7..=8 => {
                    let live_edges: Vec<usize> =
                        (0..edges.len()).filter(|&i| edges[i].is_some()).collect();
                    if let Some(&e) = live_edges.get(next(live_edges.len().max(1))) {
                        assert!(t.remove_edge(EdgeIndex::new(e)).is_some());
                        edges[e] = None;
                        free_edges.push(e);
                    }
                }
                _ if !live.is_empty() => {
                    let a = live[next(live.len())];
                    // The topology removes the out list first, then the in list, each newest
                    // first: that is the order their slots go to the free list.
                    let mut gone: Vec<(u32, usize)> = Vec::new();
                    for (i, edge) in edges.iter().enumerate() {
                        if let Some((s, _, st)) = edge
                            && *s == a
                        {
                            gone.push((*st, i));
                        }
                    }
                    gone.sort_by(|x, y| y.cmp(x));
                    let mut incoming: Vec<(u32, usize)> = Vec::new();
                    for (i, edge) in edges.iter().enumerate() {
                        if let Some((s, d, st)) = edge
                            && *d == a
                            && *s != a
                        {
                            incoming.push((*st, i));
                        }
                    }
                    incoming.sort_by(|x, y| y.cmp(x));
                    for (_, e) in gone.into_iter().chain(incoming) {
                        edges[e] = None;
                        free_edges.push(e);
                    }
                    assert!(t.remove_node(NodeIndex::new(a)).is_some());
                    nodes[a] = None;
                    free_nodes.push(a);
                }
                _ => {}
            }
        }
        assert_eq!(t.node_count(), nodes.iter().flatten().count());
        assert_eq!(t.edge_count(), edges.iter().flatten().count());
        for (a, node) in nodes.iter().enumerate() {
            if node.is_none() {
                continue;
            }
            for (list, direction) in [(0, Direction::Outgoing), (1, Direction::Incoming)] {
                let mut expected: Vec<u32> = edges
                    .iter()
                    .flatten()
                    .filter(|(s, d, _)| if list == 0 { *s == a } else { *d == a })
                    .map(|(_, _, st)| *st)
                    .collect();
                expected.sort_by(|x, y| y.cmp(x));
                let got: Vec<u32> = t
                    .edges_directed(NodeIndex::new(a), direction)
                    .map(|e| *e.weight())
                    .collect();
                assert_eq!(got, expected, "vertex {a} list {list}");
            }
        }
    }
}
