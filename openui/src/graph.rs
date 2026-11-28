//! Where a `Diagram`'s boxes go: its nodes in rows from top to bottom, so that every arrow
//! points down unless it closes a loop, and in an order within each row that keeps arrows
//! from crossing more than they must. Kept here, away from Dioxus, so it's tested natively.

/// What sits in one place of a row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    /// The node with this index.
    Node(usize),
    /// An arrow on its way to a row further down, kept clear of the boxes here.
    Pass,
}

/// One arrow's way through the rows.
#[derive(Clone, PartialEq, Debug)]
pub struct Route {
    /// The edge it draws, by index.
    pub edge: usize,
    /// The places it goes through, top to bottom: each a row and a place in that row.
    pub points: Vec<(usize, usize)>,
    /// Whether it points up: it closes a loop, from its last point to its first.
    pub up: bool,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Layout {
    pub rows: Vec<Vec<Slot>>,
    pub routes: Vec<Route>,
}

impl Layout {
    /// The most places any row has.
    pub fn width(&self) -> usize {
        self.rows.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// Where along its row a place's centre is, from 0 to 1: places share a row evenly.
    pub fn x(&self, row: usize, place: usize) -> f64 {
        let n = self.rows.get(row).map_or(1, Vec::len).max(1);
        (place as f64 + 0.5) / n as f64
    }
}

/// Lays out `nodes` nodes joined by `edges` (from, to). An edge to itself or to a node
/// that isn't there has no route.
pub fn layout(nodes: usize, edges: &[(usize, usize)]) -> Layout {
    if nodes == 0 {
        return Layout::default();
    }
    let valid = |&(a, b): &(usize, usize)| a != b && a < nodes && b < nodes;
    let mut out: Vec<Vec<(usize, usize)>> = vec![Vec::new(); nodes];
    let mut incoming = vec![0usize; nodes];
    for (e, edge) in edges.iter().enumerate().filter(|(_, e)| valid(e)) {
        out[edge.0].push((edge.1, e));
        incoming[edge.1] += 1;
    }

    // An edge that leads back to a node still being walked closes a loop: it's drawn
    // pointing up, and ranked as if it ran the other way.
    let mut up = vec![false; edges.len()];
    let mut state = vec![0u8; nodes];
    // From the nodes nothing leads to first, so a loop is broken where a reader expects.
    let starts = (0..nodes)
        .filter(|v| incoming[*v] == 0)
        .chain(0..nodes)
        .collect::<Vec<_>>();
    for start in starts {
        if state[start] != 0 {
            continue;
        }
        // (node, the next of its edges to follow)
        let mut stack = vec![(start, 0usize)];
        state[start] = 1;
        while let Some((v, next)) = stack.last().copied() {
            match out[v].get(next) {
                Some(&(w, e)) => {
                    stack.last_mut().unwrap().1 += 1;
                    match state[w] {
                        0 => {
                            state[w] = 1;
                            stack.push((w, 0));
                        }
                        1 => up[e] = true,
                        _ => {}
                    }
                }
                None => {
                    state[v] = 2;
                    stack.pop();
                }
            }
        }
    }

    // Each edge as it runs for ranking: downward.
    let down: Vec<Option<(usize, usize)>> = edges
        .iter()
        .enumerate()
        .map(|(e, edge)| valid(edge).then(|| if up[e] { (edge.1, edge.0) } else { *edge }))
        .collect();

    // A node's row is one below the lowest of the nodes that lead to it.
    let mut rank = vec![0usize; nodes];
    for _ in 0..nodes {
        let mut changed = false;
        for &(a, b) in down.iter().flatten() {
            if rank[b] < rank[a] + 1 {
                rank[b] = rank[a] + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let depth = rank.iter().max().map_or(1, |r| r + 1);

    // The rows: nodes as declared, then a place for each arrow that passes through.
    // Items are numbered: nodes first, then passes.
    let mut rows: Vec<Vec<usize>> = vec![Vec::new(); depth];
    for v in 0..nodes {
        rows[rank[v]].push(v);
    }
    let mut item_count = nodes;
    // Each edge's items, top to bottom.
    let mut chains: Vec<(usize, Vec<usize>)> = Vec::new();
    for (e, d) in down.iter().enumerate() {
        let Some((a, b)) = *d else { continue };
        let mut chain = vec![a];
        for row in rows.iter_mut().take(rank[b]).skip(rank[a] + 1) {
            row.push(item_count);
            chain.push(item_count);
            item_count += 1;
        }
        chain.push(b);
        chains.push((e, chain));
    }
    let mut above: Vec<Vec<usize>> = vec![Vec::new(); item_count];
    let mut below: Vec<Vec<usize>> = vec![Vec::new(); item_count];
    for (_, chain) in &chains {
        for pair in chain.windows(2) {
            below[pair[0]].push(pair[1]);
            above[pair[1]].push(pair[0]);
        }
    }

    // Order each row by where its neighbours in the row before sit, a few times down and
    // up: arrows cross less.
    let mut at = vec![0.5f64; item_count];
    let place = |rows: &[Vec<usize>], at: &mut [f64]| {
        for row in rows {
            let n = row.len() as f64;
            for (i, item) in row.iter().enumerate() {
                at[*item] = (i as f64 + 0.5) / n;
            }
        }
    };
    place(&rows, &mut at);
    for sweep in 0..4 {
        let order: Vec<usize> = if sweep % 2 == 0 {
            (1..depth).collect()
        } else {
            (0..depth.saturating_sub(1)).rev().collect()
        };
        for r in order {
            let near = if sweep % 2 == 0 { &above } else { &below };
            let mut keyed: Vec<(f64, usize)> = rows[r]
                .iter()
                .map(|item| {
                    let n = &near[*item];
                    let key = if n.is_empty() {
                        at[*item]
                    } else {
                        n.iter().map(|x| at[*x]).sum::<f64>() / n.len() as f64
                    };
                    (key, *item)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            rows[r] = keyed.into_iter().map(|(_, item)| item).collect();
            place(&rows, &mut at);
        }
    }

    let mut row_of = vec![(0usize, 0usize); item_count];
    for (r, row) in rows.iter().enumerate() {
        for (i, item) in row.iter().enumerate() {
            row_of[*item] = (r, i);
        }
    }
    Layout {
        rows: rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|item| {
                        if *item < nodes {
                            Slot::Node(*item)
                        } else {
                            Slot::Pass
                        }
                    })
                    .collect()
            })
            .collect(),
        routes: chains
            .into_iter()
            .map(|(e, chain)| Route {
                edge: e,
                points: chain.into_iter().map(|item| row_of[item]).collect(),
                up: up[e],
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_of(l: &Layout, node: usize) -> usize {
        l.rows
            .iter()
            .position(|r| r.contains(&Slot::Node(node)))
            .unwrap()
    }

    #[test]
    fn a_chain_is_a_column() {
        let l = layout(3, &[(0, 1), (1, 2)]);
        assert_eq!(l.rows.len(), 3);
        assert_eq!(l.width(), 1);
        assert_eq!(l.routes[0].points, [(0, 0), (1, 0)]);
        assert!(!l.routes[0].up);
    }

    #[test]
    fn a_branch_shares_a_row_and_merges() {
        // 0 → 1, 0 → 2, 1 → 3, 2 → 3
        let l = layout(4, &[(0, 1), (0, 2), (1, 3), (2, 3)]);
        assert_eq!(row_of(&l, 1), 1);
        assert_eq!(row_of(&l, 2), 1);
        assert_eq!(row_of(&l, 3), 2);
        assert_eq!(l.width(), 2);
        assert_eq!(l.x(1, 0), 0.25);
    }

    #[test]
    fn an_edge_that_closes_a_loop_points_up() {
        // 0 → 1 → 2 → 0
        let l = layout(3, &[(0, 1), (1, 2), (2, 0)]);
        assert_eq!((row_of(&l, 0), row_of(&l, 1), row_of(&l, 2)), (0, 1, 2));
        let back = l.routes.iter().find(|r| r.edge == 2).unwrap();
        assert!(back.up);
        // It runs from node 0's row to node 2's, through a place kept clear in row 1.
        assert_eq!(back.points.len(), 3);
        assert_eq!(l.rows[1].len(), 2);
        assert!(l.rows[1].contains(&Slot::Pass));
    }

    #[test]
    fn a_long_edge_passes_beside_the_boxes_between() {
        // 0 → 1 → 2, and 0 → 2 straight.
        let l = layout(3, &[(0, 1), (1, 2), (0, 2)]);
        let long = l.routes.iter().find(|r| r.edge == 2).unwrap();
        assert_eq!(long.points.len(), 3);
        assert_eq!(l.rows[1].iter().filter(|s| **s == Slot::Pass).count(), 1);
    }

    #[test]
    fn crossings_are_untangled() {
        // 0 → 3, 1 → 2: declared so that the arrows would cross.
        let l = layout(4, &[(0, 3), (1, 2)]);
        let x = |n: usize| {
            let r = row_of(&l, n);
            let i = l.rows[r].iter().position(|s| *s == Slot::Node(n)).unwrap();
            l.x(r, i)
        };
        assert!((x(0) < x(1)) == (x(3) < x(2)));
    }

    #[test]
    fn bad_edges_are_left_out() {
        let l = layout(2, &[(0, 0), (0, 7), (0, 1)]);
        assert_eq!(l.routes.len(), 1);
        assert_eq!(l.routes[0].edge, 2);
        assert_eq!(layout(0, &[]), Layout::default());
    }
}
