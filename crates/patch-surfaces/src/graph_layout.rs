//! Where each Router node sits.
//!
//! The provider lists processors in signal order, and that order stays: the
//! path reads downward. A source or a destination then takes the row of the
//! processor it touches, so the cable runs across the row instead of arcing
//! the height of the window. Anything with no processor keeps its own order
//! in the gaps that leaves. A source wired straight to a destination, with
//! no processor between them, shares that destination's row.
//!
//! A device block is the exception. Outputs named `Device · X` and
//! `Left · X` belong to one device, and that block stays contiguous even
//! when the program is routed at a single port. Pulling the port up to the
//! program's row is what used to scatter a device's channels down the column.

use patch_model::audio::NodeKind;

pub fn column(kind: NodeKind) -> usize {
    match kind {
        NodeKind::Source => 0,
        NodeKind::Processor => 1,
        NodeKind::Destination => 2,
    }
}

/// `(column, row)` for each node, parallel to `kinds`.
///
/// `groups[i] == 0` (or a missing entry) may share a row with what it is
/// wired to. A positive id is a device block: those nodes stay together, in
/// the order the provider listed them, packed under the free rows. Block ids
/// are packed in the order each id first appears. Route endpoints are indexes
/// into `kinds`. Indexes past the end are ignored.
pub fn place(kinds: &[NodeKind], groups: &[u32], routes: &[(usize, usize)]) -> Vec<(i32, i32)> {
    let n = kinds.len();
    let routes: Vec<(usize, usize)> = routes
        .iter()
        .copied()
        .filter(|(a, b)| *a < n && *b < n && a != b)
        .collect();
    let mut members: [Vec<usize>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    // Processors are never blocked: the signal path is the column's order.
    // A blocked source or destination is packed later, as a whole device.
    let mut free: [Vec<usize>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for i in 0..n {
        let col = column(kinds[i]);
        members[col].push(i);
        if col == 1 || groups.get(i).copied().unwrap_or(0) == 0 {
            free[col].push(i);
        }
    }
    let mut row = vec![None; n];
    for (r, &node) in free[1].iter().enumerate() {
        row[node] = Some(r as i32);
    }
    // Claim the processor's row first. A later pass pulls in nodes that only
    // touch something already placed, then direct source-to-destination pairs.
    anchor_column(0, &free, &mut row, &routes, kinds, Some(1));
    anchor_column(2, &free, &mut row, &routes, kinds, Some(1));
    settle(&free, &mut row, &routes, kinds);
    align_direct(&free, &mut row, &routes, kinds);
    settle(&free, &mut row, &routes, kinds);
    for col in 0..3 {
        fill(&mut row, &free[col]);
        pack_groups(&members[col], groups, &mut row);
    }
    (0..n)
        .map(|i| (column(kinds[i]) as i32, row[i].unwrap_or(0)))
        .collect()
}

/// Anchor unplaced nodes onto a neighbor that already has a row, a few times
/// so a chain of dependencies can walk across.
fn settle(
    members: &[Vec<usize>; 3],
    row: &mut [Option<i32>],
    routes: &[(usize, usize)],
    kinds: &[NodeKind],
) {
    for _ in 0..kinds.len() {
        let sources = anchor_column(0, members, row, routes, kinds, None);
        let dests = anchor_column(2, members, row, routes, kinds, None);
        if !sources && !dests {
            break;
        }
    }
}

fn anchor_column(
    col: usize,
    members: &[Vec<usize>; 3],
    row: &mut [Option<i32>],
    routes: &[(usize, usize)],
    kinds: &[NodeKind],
    only_col: Option<usize>,
) -> bool {
    let group = &members[col];
    let mut used = used_slots(group, row);
    let mut claims = Vec::new();
    for (orig, &node) in group.iter().enumerate() {
        if row[node].is_some() {
            continue;
        }
        let Some(prefer) = median(&neighbor_rows(node, routes, row, kinds, only_col)) else {
            continue;
        };
        claims.push((prefer, orig, node));
    }
    if claims.is_empty() {
        return false;
    }
    claims.sort_by_key(|c| (c.0, c.1));
    let mut changed = false;
    for (prefer, _, node) in claims {
        let Some(slot) = nearest_free(&used, prefer) else {
            continue;
        };
        used[slot] = true;
        row[node] = Some(slot as i32);
        changed = true;
    }
    changed
}

/// Two unplaced ends of a source-to-destination route take the same free row.
fn align_direct(
    members: &[Vec<usize>; 3],
    row: &mut [Option<i32>],
    routes: &[(usize, usize)],
    kinds: &[NodeKind],
) {
    let mut pairs = Vec::new();
    for &(a, b) in routes {
        let (source, dest) = match (column(kinds[a]), column(kinds[b])) {
            (0, 2) => (a, b),
            (2, 0) => (b, a),
            _ => continue,
        };
        if row[source].is_some() || row[dest].is_some() {
            continue;
        }
        let orig = members[0].iter().position(|n| *n == source).unwrap_or(0);
        pairs.push((orig, source, dest));
    }
    pairs.sort_by_key(|p| (p.0, p.2));
    let slots = members[0].len().min(members[2].len());
    for (_, source, dest) in pairs {
        if row[source].is_some() || row[dest].is_some() {
            continue;
        }
        let used_s = used_slots(&members[0], row);
        let used_d = used_slots(&members[2], row);
        if let Some(slot) = (0..slots).find(|&i| !used_s[i] && !used_d[i]) {
            row[source] = Some(slot as i32);
            row[dest] = Some(slot as i32);
        }
    }
}

/// Device blocks follow the free rows. Within a block, provider order is the
/// device row and then its ports. Blocks are not pulled onto a processor row:
/// that is what used to lift one port out of its device.
fn pack_groups(members: &[usize], groups: &[u32], row: &mut [Option<i32>]) {
    let mut order = Vec::new();
    for &node in members {
        let group = groups.get(node).copied().unwrap_or(0);
        if group > 0 && !order.contains(&group) {
            order.push(group);
        }
    }
    if order.is_empty() {
        return;
    }
    let mut cursor = members
        .iter()
        .filter_map(|&node| row[node])
        .max()
        .map(|r| r + 1)
        .unwrap_or(0);
    for group in order {
        for &node in members {
            if groups.get(node).copied().unwrap_or(0) != group || row[node].is_some() {
                continue;
            }
            row[node] = Some(cursor);
            cursor += 1;
        }
    }
}

fn fill(row: &mut [Option<i32>], members: &[usize]) {
    let n = members.len();
    let mut used = used_slots(members, row);
    let mut free = 0;
    for &node in members {
        if row[node].is_some() {
            continue;
        }
        while free < n && used[free] {
            free += 1;
        }
        if free >= n {
            break;
        }
        used[free] = true;
        row[node] = Some(free as i32);
    }
}

fn used_slots(members: &[usize], row: &[Option<i32>]) -> Vec<bool> {
    let mut used = vec![false; members.len()];
    for &node in members {
        if let Some(r) = row[node]
            && (r as usize) < used.len()
        {
            used[r as usize] = true;
        }
    }
    used
}

fn neighbor_rows(
    node: usize,
    routes: &[(usize, usize)],
    row: &[Option<i32>],
    kinds: &[NodeKind],
    only_col: Option<usize>,
) -> Vec<i32> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for &(a, b) in routes {
        let other = if a == node {
            b
        } else if b == node {
            a
        } else {
            continue;
        };
        if seen.contains(&other) {
            continue;
        }
        seen.push(other);
        if only_col.is_some_and(|col| column(kinds[other]) != col) {
            continue;
        }
        if let Some(r) = row[other] {
            out.push(r);
        }
    }
    out
}

fn median(rows: &[i32]) -> Option<i32> {
    if rows.is_empty() {
        return None;
    }
    let mut rows = rows.to_vec();
    rows.sort_unstable();
    Some(rows[rows.len() / 2])
}

/// How one Router row should read. `group == 0` is not part of a device block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub name: String,
    /// Set when this row is a channel under a device, so the list can say
    /// which device without repeating the name on the canvas.
    pub device: String,
    pub group: u32,
    pub nest: bool,
    pub head: bool,
}

/// Collapse `Device · X` and `Left · X` into one block per device.
///
/// The device row keeps the device name. A channel under it keeps only the
/// channel (`Left`, `Left + Right`). Anything else is unchanged. Group ids
/// follow the first time each device appears, which is the provider's device
/// order because the whole-device rows are listed before the ports.
pub fn device_rows(nodes: &[(NodeKind, &str)]) -> Vec<DeviceRow> {
    let split: Vec<Option<(&str, &str, bool)>> = nodes
        .iter()
        .map(|(kind, name)| {
            if *kind == NodeKind::Destination {
                split_output(name)
            } else {
                None
            }
        })
        .collect();
    let mut order: Vec<&str> = Vec::new();
    for part in split.iter().flatten() {
        if !order.contains(&part.1) {
            order.push(part.1);
        }
    }
    let count = |key: &str| split.iter().flatten().filter(|part| part.1 == key).count();
    let has_device = |key: &str| {
        split
            .iter()
            .flatten()
            .any(|part| part.1 == key && part.2)
    };
    let mut seen_head = Vec::new();
    split
        .iter()
        .enumerate()
        .map(|(i, part)| {
            let Some((left, key, is_device)) = *part else {
                return DeviceRow {
                    name: nodes[i].1.to_string(),
                    device: String::new(),
                    group: 0,
                    nest: false,
                    head: false,
                };
            };
            let group = order.iter().position(|k| *k == key).unwrap_or(0) as u32 + 1;
            let multi = count(key) > 1;
            let nest = multi && !is_device;
            let head = is_device
                || !multi
                || (!has_device(key) && !seen_head.contains(&group));
            if head {
                seen_head.push(group);
            }
            let name = if is_device {
                key.to_string()
            } else if multi {
                left.to_string()
            } else {
                nodes[i].1.to_string()
            };
            DeviceRow {
                name,
                device: key.to_string(),
                group,
                nest,
                head,
            }
        })
        .collect()
}

/// `Device · Focusrite` or `Left + Right · Focusrite`.
/// The bool is the whole-device row.
fn split_output(name: &str) -> Option<(&str, &str, bool)> {
    let (left, right) = name.split_once(" · ")?;
    if left.is_empty() || right.is_empty() {
        return None;
    }
    Some((left, right, left == "Device"))
}

/// The preferred slot when it is open, otherwise the next open slot below it,
/// then above. A second cable into the same processor sits just under the first.
fn nearest_free(used: &[bool], prefer: i32) -> Option<usize> {
    let n = used.len();
    if n == 0 {
        return None;
    }
    let prefer = prefer.clamp(0, n as i32 - 1) as usize;
    if !used[prefer] {
        return Some(prefer);
    }
    for dist in 1..n {
        let down = prefer + dist;
        if down < n && !used[down] {
            return Some(down);
        }
        if dist <= prefer && !used[prefer - dist] {
            return Some(prefer - dist);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use NodeKind::{Destination, Processor, Source};

    fn rows_of(placed: &[(i32, i32)], col: i32) -> Vec<i32> {
        placed
            .iter()
            .filter(|p| p.0 == col)
            .map(|p| p.1)
            .collect()
    }

    fn assert_packed(placed: &[(i32, i32)]) {
        for col in 0..3 {
            let mut rows = rows_of(placed, col);
            let n = rows.len();
            rows.sort();
            rows.dedup();
            assert_eq!(rows, (0..n as i32).collect::<Vec<_>>(), "column {col}");
        }
    }

    #[test]
    fn signal_path_shares_a_row() {
        // Provider order from a live Router: system and the decks, then the
        // hardware sources; bus, booth, program, cue, mics; devices, then the
        // port the program actually plays to.
        let kinds = [
            Source, Source, Source, Source, Source, Source,
            Processor, Processor, Processor, Processor, Processor, Processor,
            Destination, Destination, Destination, Destination,
            Destination, Destination, Destination, Destination,
        ];
        let routes = [
            (0, 6),  // system → bus
            (1, 7),  // deck A → booth
            (2, 7),  // deck B → booth
            (6, 8),  // bus → program
            (7, 8),  // booth → program
            (10, 8), // mic → program
            (11, 8),
            (8, 18), // program → the routed port
        ];
        // Four devices, then the ports of the first two. The program plays
        // to a port of the first device; that port stays in the block.
        let groups = [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 1, 1, 1, 2,
        ];
        let placed = place(&kinds, &groups, &routes);
        assert_packed(&placed);
        // Processors stay listed as the provider gave them.
        assert_eq!(rows_of(&placed, 1), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(placed[0].1, placed[6].1, "system sounds beside the bus");
        assert_eq!(placed[1].1, placed[7].1, "deck A beside the booth");
        assert_eq!(placed[2].1, placed[7].1 + 1, "deck B just under the booth");
        // Device, then its ports, then the next device. The routed port is
        // the last channel of the first device, not the program's row.
        assert_eq!(
            [placed[12].1, placed[16].1, placed[17].1, placed[18].1],
            [0, 1, 2, 3]
        );
        assert_eq!([placed[13].1, placed[19].1], [4, 5]);
        assert_eq!(placed[14].1, 6);
        assert_eq!(placed[15].1, 7);
        assert_ne!(placed[18].1, placed[8].1);
        assert!(placed[3].1 < placed[4].1 && placed[4].1 < placed[5].1);
    }

    #[test]
    fn a_direct_route_shares_a_row_when_nothing_else_claims_it() {
        let kinds = [Source, Destination];
        let placed = place(&kinds, &[], &[(0, 1)]);
        assert_packed(&placed);
        assert_eq!(placed[0].1, placed[1].1);
    }

    #[test]
    fn a_second_source_sits_beside_the_first() {
        let kinds = [Source, Source, Destination];
        let placed = place(&kinds, &[], &[(0, 2), (1, 2)]);
        assert_packed(&placed);
        assert_eq!(placed[0].1, placed[2].1);
        assert_eq!(placed[1].1, placed[2].1 + 1);
    }

    #[test]
    fn an_empty_graph_and_a_lone_node() {
        assert!(place(&[], &[], &[]).is_empty());
        assert_eq!(place(&[Processor], &[], &[]), vec![(1, 0)]);
        assert_eq!(place(&[Source], &[], &[(0, 4)]), vec![(0, 0)]);
    }

    #[test]
    fn output_channels_collapse_into_the_device() {
        let rows = device_rows(&[
            (Destination, "Device · Focusrite Scarlett"),
            (Destination, "Device · BLUE USB"),
            (Destination, "Left + Right · Focusrite Scarlett"),
            (Destination, "Left · Focusrite Scarlett"),
            (Destination, "Right · Focusrite Scarlett"),
            (Destination, "Left + Right · BLUE USB"),
            (Source, "Deck A"),
            (Destination, "Headphones"),
            (Destination, "Center · Only"),
        ]);
        assert_eq!(rows[0].name, "Focusrite Scarlett");
        assert!(rows[0].head && !rows[0].nest);
        assert_eq!(rows[2].name, "Left + Right");
        assert!(rows[2].nest && !rows[2].head);
        assert_eq!(rows[3].name, "Left");
        assert_eq!(rows[0].group, rows[2].group);
        assert_eq!(rows[0].group, rows[4].group);
        assert_ne!(rows[0].group, rows[1].group);
        assert_eq!(rows[1].group, rows[5].group);
        assert_eq!(rows[5].name, "Left + Right");
        assert!(rows[5].nest);
        assert_eq!(rows[6].name, "Deck A");
        assert_eq!(rows[6].group, 0);
        assert_eq!(rows[7].name, "Headphones");
        assert_eq!(rows[7].group, 0);
        // A port with no sibling keeps the device in the name.
        assert_eq!(rows[8].name, "Center · Only");
        assert_eq!(rows[8].group, 3);
        assert!(rows[8].head && !rows[8].nest);
    }
}
