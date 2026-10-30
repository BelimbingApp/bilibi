//! The pure dwindle tree behind a tiled workspace (Bilimbi
//! `Tiling.Layout`).
//!
//! A layout is a binary space partition: a leaf is one tile showing a page at
//! a path, and a split divides its rectangle between two children along one
//! axis at a ratio. Opening a page splits a tile, and the split direction
//! follows the shape of the rectangle being split (wider than tall splits
//! side by side), which is what makes the tiling automatic. This is
//! Hyprland's dwindle layout with the browser tab as the monitor.
//!
//! There is no cap on the number of tiles: the operator decides. Nothing here
//! touches a socket, a page or the database; the host page owns focus,
//! monocle and persistence, and this module owns the shape and every operation
//! on it, each a value-in, value-out method.
//!
//! Every leaf and split carries a stable id. The host renders one DOM element
//! per id, so a tile's frame keeps its element across splits, closes and
//! resizes and is never re-created by a patch, which would reload the page
//! inside it. [`Layout::reconcile`] keeps those ids when a tree arrives from the
//! URL.
//!
//! The URL form is compact and readable rather than encoded:
//!
//! ```text
//! h.5(/companies,v.6(/employees,/users))
//! ```
//!
//! `h` splits side by side and `v` top and bottom, the number is the first
//! child's share, and a leaf is its path with `(`, `)`, `,`, `>` and `%`
//! percent-encoded. A leaf that follows selections carries the route pattern
//! it follows after `>` (`/companies/42>/companies/:id`); this module only
//! keeps the pattern's shape.

const MIN_RATIO: f64 = 0.1;
const MAX_RATIO: f64 = 0.9;
const RESIZE_STEP: f64 = 0.05;
/// The landscape screen an `open` assumes until the browser reports its own.
pub const DEFAULT_VIEWPORT: (f64, f64) = (1600.0, 900.0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    /// Side by side.
    H,
    /// Top and bottom.
    V,
}
impl Direction {
    fn other(self) -> Self {
        match self {
            Self::H => Self::V,
            Self::V => Self::H,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::H => "h",
            Self::V => "v",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Side {
    Left,
    Right,
    Up,
    Down,
}
impl Side {
    pub fn parse(side: &str) -> Option<Self> {
        Some(match side {
            "left" => Self::Left,
            "right" => Self::Right,
            "up" => Self::Up,
            "down" => Self::Down,
            _ => return None,
        })
    }
    fn axis(self) -> Direction {
        match self {
            Self::Left | Self::Right => Direction::H,
            Self::Up | Self::Down => Direction::V,
        }
    }
    fn delta(self) -> f64 {
        match self {
            Self::Right | Self::Down => RESIZE_STEP,
            Self::Left | Self::Up => -RESIZE_STEP,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Leaf {
    pub id: String,
    pub path: String,
    /// The route pattern this tile follows, such as `/companies/:id`.
    pub follow: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Split {
    pub id: String,
    pub direction: Direction,
    /// The first child's share.
    pub ratio: f64,
    pub first: Box<Node>,
    pub second: Box<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Leaf(Leaf),
    Split(Split),
}
impl Node {
    fn id(&self) -> &str {
        match self {
            Self::Leaf(leaf) => &leaf.id,
            Self::Split(split) => &split.id,
        }
    }
    fn find_mut(&mut self, id: &str) -> Option<&mut Node> {
        if self.id() == id {
            return Some(self);
        }
        match self {
            Self::Leaf(_) => None,
            Self::Split(split) => match split.first.find_mut(id) {
                Some(found) => Some(found),
                None => split.second.find_mut(id),
            },
        }
    }
    fn collect_leaves<'a>(&'a self, out: &mut Vec<&'a Leaf>) {
        match self {
            Self::Leaf(leaf) => out.push(leaf),
            Self::Split(split) => {
                split.first.collect_leaves(out);
                split.second.collect_leaves(out);
            }
        }
    }
    fn leaves_mut(&mut self, visit: &mut impl FnMut(&mut Leaf)) {
        match self {
            Self::Leaf(leaf) => visit(leaf),
            Self::Split(split) => {
                split.first.leaves_mut(visit);
                split.second.leaves_mut(visit);
            }
        }
    }
    fn splits<'a>(&'a self, out: &mut Vec<&'a Split>) {
        if let Self::Split(split) = self {
            out.push(split);
            split.first.splits(out);
            split.second.splits(out);
        }
    }
    /// The splits above the node with `id`, outermost first; empty when the
    /// node is the root or absent.
    fn ancestors<'a>(&'a self, id: &str, out: &mut Vec<&'a Split>) -> bool {
        match self {
            Self::Leaf(leaf) => leaf.id == id,
            Self::Split(split) if split.id == id => true,
            Self::Split(split) => {
                out.push(split);
                if split.first.ancestors(id, out) || split.second.ancestors(id, out) {
                    true
                } else {
                    out.pop();
                    false
                }
            }
        }
    }
    /// Removes the leaf `id`; its sibling takes the parent's place.
    fn remove_leaf(self, id: &str) -> Node {
        match self {
            Self::Leaf(leaf) => Self::Leaf(leaf),
            Self::Split(split) => match (*split.first, *split.second) {
                (Self::Leaf(first), second) if first.id == id => second,
                (first, Self::Leaf(second)) if second.id == id => first,
                (first, second) => Self::Split(Split {
                    first: Box::new(first.remove_leaf(id)),
                    second: Box::new(second.remove_leaf(id)),
                    ..split
                }),
            },
        }
    }
}

/// A rectangle as fractions of the workspace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}
impl Rect {
    pub const FULL: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };
    fn divide(self, direction: Direction, ratio: f64) -> (Rect, Rect) {
        let Rect { x, y, w, h } = self;
        match direction {
            Direction::H => {
                let cut = w * ratio;
                (
                    Rect { x, y, w: cut, h },
                    Rect {
                        x: x + cut,
                        y,
                        w: w - cut,
                        h,
                    },
                )
            }
            Direction::V => {
                let cut = h * ratio;
                (
                    Rect { x, y, w, h: cut },
                    Rect {
                        x,
                        y: y + cut,
                        w,
                        h: h - cut,
                    },
                )
            }
        }
    }
}

/// Where every tile and divider sits. `handles` pairs each split with the
/// whole rectangle it divides, so the host draws the divider at `ratio` along
/// it and can turn a pointer position back into a ratio while dragging.
pub struct Rects<'a> {
    pub leaves: Vec<(&'a Leaf, Rect)>,
    pub handles: Vec<(&'a Split, Rect)>,
}

/// Which tile an `open` halves.
#[derive(Clone, Copy, Debug)]
pub enum At<'a> {
    /// The last tile in tree order.
    Last,
    /// The tile with the most room, ties going to the later one in tree order.
    Largest,
    /// This tile, or the last one when it is unknown.
    Tile(&'a str),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    root: Option<Node>,
    next_id: u64,
}
impl Default for Layout {
    fn default() -> Self {
        Self {
            root: None,
            next_id: 1,
        }
    }
}

impl Layout {
    /// A workspace with no tile.
    pub fn empty() -> Self {
        Self::default()
    }
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }
    /// The leaves in tree order: left before right, top before bottom.
    pub fn leaves(&self) -> Vec<&Leaf> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.collect_leaves(&mut out);
        }
        out
    }
    pub fn count(&self) -> usize {
        self.leaves().len()
    }
    pub fn fetch_leaf(&self, id: &str) -> Option<&Leaf> {
        self.leaves().into_iter().find(|leaf| leaf.id == id)
    }

    fn next_id(&mut self, prefix: char) -> String {
        let id = format!("{prefix}{}", self.next_id);
        self.next_id += 1;
        id
    }
    fn new_leaf(&mut self, path: &str) -> Leaf {
        Leaf {
            id: self.next_id('t'),
            path: path.to_owned(),
            follow: None,
        }
    }

    /// Opens `path` in a new tile and returns its id.
    ///
    /// With an empty workspace the tile fills it. Otherwise the tile at `at` is
    /// split, and the direction comes from the shape of that tile on a
    /// `viewport` of pixels: wider than tall splits side by side, otherwise top
    /// and bottom. The new tile is the second child, so it appears to the right
    /// or below. [`At::Largest`] is the dwindle order a person expects from the
    /// sidebar: the second page takes the right half, the third the bottom of
    /// that half, the fourth the bottom of the left half, and so on.
    pub fn open(&mut self, path: &str, at: At<'_>, viewport: (f64, f64)) -> String {
        let leaf = self.new_leaf(path);
        let id = leaf.id.clone();
        let Some(root) = &self.root else {
            self.root = Some(Node::Leaf(leaf));
            return id;
        };
        let (vw, vh) = viewport;
        let rects = rects_of(root);
        let target = match at {
            At::Tile(tile) if rects.leaves.iter().any(|(l, _)| l.id == tile) => tile.to_owned(),
            At::Largest => rects
                .leaves
                .iter()
                .enumerate()
                .max_by(|(ia, (_, a)), (ib, (_, b))| {
                    let area = |r: &Rect| round(r.w * vw * r.h * vh, 3);
                    area(a).total_cmp(&area(b)).then(ia.cmp(ib))
                })
                .map(|(_, (leaf, _))| leaf.id.clone())
                .unwrap_or_default(),
            _ => rects
                .leaves
                .last()
                .map(|(leaf, _)| leaf.id.clone())
                .unwrap_or_default(),
        };
        let rect = rects
            .leaves
            .iter()
            .find(|(leaf, _)| leaf.id == target)
            .map_or(Rect::FULL, |(_, rect)| *rect);
        let direction = if rect.w * vw >= rect.h * vh {
            Direction::H
        } else {
            Direction::V
        };
        let split_id = self.next_id('s');
        if let Some(node) = self.root.as_mut().and_then(|root| root.find_mut(&target)) {
            let existing = std::mem::replace(node, Node::Leaf(leaf.clone()));
            *node = Node::Split(Split {
                id: split_id,
                direction,
                ratio: 0.5,
                first: Box::new(existing),
                second: Box::new(Node::Leaf(leaf)),
            });
        }
        id
    }

    /// Closes the tile with `id`; its sibling takes the whole of their parent's
    /// rectangle. Closing the only tile empties the workspace; an unknown id
    /// changes nothing.
    pub fn close(&mut self, id: &str) {
        self.root = match self.root.take() {
            Some(Node::Leaf(leaf)) if leaf.id == id => None,
            Some(root) => Some(root.remove_leaf(id)),
            None => None,
        };
    }

    /// Changes the path a tile shows, keeping its id, place and follow.
    pub fn update_path(&mut self, id: &str, path: &str) {
        if let Some(Node::Leaf(leaf)) = self.root.as_mut().and_then(|root| root.find_mut(id)) {
            leaf.path = path.to_owned();
        }
    }

    /// Marks the tile with `id` as following the route `pattern`, or clears it
    /// with `None`. A pattern that names no single record changes nothing.
    pub fn follow(&mut self, id: &str, pattern: Option<&str>) {
        if pattern.is_some_and(|pattern| !follow_pattern(pattern)) {
            return;
        }
        if let Some(Node::Leaf(leaf)) = self.root.as_mut().and_then(|root| root.find_mut(id)) {
            leaf.follow = pattern.map(str::to_owned);
        }
    }

    /// The route patterns the tiles follow, each once, in tree order.
    pub fn follows(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for pattern in self
            .leaves()
            .into_iter()
            .filter_map(|l| l.follow.as_deref())
        {
            if !out.contains(&pattern) {
                out.push(pattern);
            }
        }
        out
    }

    /// Sets a split's ratio, the first child's share, clamped to `0.1..=0.9`.
    pub fn resize(&mut self, id: &str, ratio: f64) {
        if !ratio.is_finite() {
            return;
        }
        if let Some(Node::Split(split)) = self.root.as_mut().and_then(|root| root.find_mut(id)) {
            split.ratio = clamp_ratio(ratio);
        }
    }

    /// Moves a split's divider one step towards `side`; the other axis is ignored.
    pub fn nudge(&mut self, id: &str, side: Side) {
        let Some(root) = &self.root else { return };
        let mut splits = Vec::new();
        root.splits(&mut splits);
        if let Some(ratio) = splits
            .iter()
            .find(|split| split.id == id && split.direction == side.axis())
            .map(|split| split.ratio)
        {
            self.resize(id, ratio + side.delta());
        }
    }

    /// Moves the divider nearest to the tile with `id` one step towards `side`:
    /// `Left`/`Right` the closest side-by-side divider above the tile, `Up`/`Down`
    /// the closest top-and-bottom one. No divider on that axis, no change.
    pub fn resize_step(&mut self, id: &str, side: Side) {
        let Some(found) = self.ancestors(id).into_iter().rev().find_map(|split| {
            (split.direction == side.axis()).then(|| (split.id.clone(), split.ratio))
        }) else {
            return;
        };
        self.resize(&found.0, found.1 + side.delta());
    }

    fn ancestors(&self, id: &str) -> Vec<&Split> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            if !root.ancestors(id, &mut out) {
                out.clear();
            }
        }
        out
    }

    /// Flips the direction of the split that holds the tile with `id`.
    pub fn toggle_split(&mut self, id: &str) {
        let Some(parent) = self.ancestors(id).last().map(|split| split.id.clone()) else {
            return;
        };
        if let Some(Node::Split(split)) = self.root.as_mut().and_then(|r| r.find_mut(&parent)) {
            split.direction = split.direction.other();
        }
    }

    /// Swaps the places of two tiles, keeping their ids. Unknown or equal ids
    /// change nothing.
    pub fn swap(&mut self, a: &str, b: &str) {
        if a == b {
            return;
        }
        let (Some(leaf_a), Some(leaf_b)) =
            (self.fetch_leaf(a).cloned(), self.fetch_leaf(b).cloned())
        else {
            return;
        };
        // One pass, because replacing `a` by `b` and then `b` by `a` would find
        // the copy just written and replace it back.
        if let Some(root) = &mut self.root {
            root.leaves_mut(&mut |leaf| {
                if leaf.id == a {
                    *leaf = leaf_b.clone();
                } else if leaf.id == b {
                    *leaf = leaf_a.clone();
                }
            });
        }
    }

    /// The tile beside the tile with `id` on `side`, judged by the rectangles:
    /// the neighbour that shares the longest edge with it. `None` at the edge.
    pub fn neighbor(&self, id: &str, side: Side) -> Option<String> {
        let rects = self.rects();
        let from = rects.leaves.iter().find(|(leaf, _)| leaf.id == id)?.1;
        let near = |a: f64, b: f64| (a - b).abs() < 1.0e-6;
        let mut best: Option<(&Leaf, f64)> = None;
        for (leaf, rect) in &rects.leaves {
            if leaf.id == id {
                continue;
            }
            let adjacent = match side {
                Side::Left => near(rect.x + rect.w, from.x),
                Side::Right => near(rect.x, from.x + from.w),
                Side::Up => near(rect.y + rect.h, from.y),
                Side::Down => near(rect.y, from.y + from.h),
            };
            let overlap = match side {
                Side::Left | Side::Right => {
                    (from.y + from.h).min(rect.y + rect.h) - from.y.max(rect.y)
                }
                Side::Up | Side::Down => {
                    (from.x + from.w).min(rect.x + rect.w) - from.x.max(rect.x)
                }
            };
            // A tile meeting only at a corner shares no edge.
            if adjacent && overlap > 1.0e-6 && best.is_none_or(|(_, longest)| overlap > longest) {
                best = Some((leaf, overlap));
            }
        }
        best.map(|(leaf, _)| leaf.id.clone())
    }

    /// Swaps the tile with `id` and its neighbour on `side`; nothing at the edge.
    pub fn move_tile(&mut self, id: &str, side: Side) {
        if let Some(other) = self.neighbor(id, side) {
            self.swap(id, &other);
        }
    }

    /// Where every tile and divider sits, as fractions of the workspace.
    pub fn rects(&self) -> Rects<'_> {
        match &self.root {
            Some(root) => rects_of(root),
            None => Rects {
                leaves: Vec::new(),
                handles: Vec::new(),
            },
        }
    }

    // ---- URL form ------------------------------------------------------

    /// The URL form of the tree; empty for an empty workspace.
    pub fn encode(&self) -> String {
        let mut out = String::new();
        if let Some(root) = &self.root {
            encode_node(root, &mut out);
        }
        out
    }

    /// Reads the URL form back into a tree with fresh ids. `""` is the empty
    /// workspace; anything else that is not exactly the grammar is `None`.
    pub fn decode(encoded: &str) -> Option<Self> {
        let mut layout = Self::empty();
        if encoded.is_empty() {
            return Some(layout);
        }
        let (node, rest) = parse_node(encoded, &mut layout)?;
        if !rest.is_empty() {
            return None;
        }
        layout.root = Some(node);
        Some(layout)
    }

    /// Carries `self`'s ids over to `incoming` where a leaf shows the same
    /// path, pairing in tree order, so a tree read back from the URL keeps the
    /// frames already on screen. Leaves with no match take fresh ids.
    pub fn reconcile(&self, incoming: Layout) -> Layout {
        let mut spare: Vec<&Leaf> = self.leaves();
        let mut next = self.next_id;
        fn relabel(node: Node, next: &mut u64, spare: &mut Vec<&Leaf>) -> Node {
            match node {
                Node::Leaf(mut leaf) => {
                    match spare.iter().position(|s| s.path == leaf.path) {
                        Some(at) => leaf.id = spare.remove(at).id.clone(),
                        None => {
                            leaf.id = format!("t{next}");
                            *next += 1;
                        }
                    }
                    Node::Leaf(leaf)
                }
                Node::Split(split) => {
                    let first = relabel(*split.first, next, spare);
                    let second = relabel(*split.second, next, spare);
                    let id = format!("s{next}");
                    *next += 1;
                    Node::Split(Split {
                        id,
                        first: Box::new(first),
                        second: Box::new(second),
                        ..split
                    })
                }
            }
        }
        let root = incoming
            .root
            .map(|root| relabel(root, &mut next, &mut spare));
        Layout {
            root,
            next_id: next,
        }
    }
}

fn rects_of(root: &Node) -> Rects<'_> {
    fn walk<'a>(node: &'a Node, rect: Rect, out: &mut Rects<'a>) {
        match node {
            Node::Leaf(leaf) => out.leaves.push((leaf, rect)),
            Node::Split(split) => {
                let (first, second) = rect.divide(split.direction, split.ratio);
                walk(&split.first, first, out);
                out.handles.push((split, rect));
                walk(&split.second, second, out);
            }
        }
    }
    let mut out = Rects {
        leaves: Vec::new(),
        handles: Vec::new(),
    };
    walk(root, Rect::FULL, &mut out);
    out
}

fn round(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (value * scale).round() / scale
}

fn clamp_ratio(ratio: f64) -> f64 {
    round(ratio.clamp(MIN_RATIO, MAX_RATIO), 3)
}

fn encode_node(node: &Node, out: &mut String) {
    match node {
        Node::Leaf(leaf) => {
            encode_leaf(&leaf.path, out);
            if let Some(pattern) = &leaf.follow {
                out.push('>');
                encode_leaf(pattern, out);
            }
        }
        Node::Split(split) => {
            out.push_str(split.direction.as_str());
            // `0.5` is written `.5`: the leading zero says nothing.
            let ratio = round(split.ratio, 3).to_string();
            out.push_str(ratio.trim_start_matches('0'));
            out.push('(');
            encode_node(&split.first, out);
            out.push(',');
            encode_node(&split.second, out);
            out.push(')');
        }
    }
}

/// Percent-encodes everything but URI reserved and unreserved characters,
/// and always the grammar's own `(`, `)`, `,`, `>` and `%`.
fn encode_leaf(text: &str, out: &mut String) {
    for byte in text.bytes() {
        let keep = byte.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'*+;=".contains(&byte);
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
}

fn parse_node<'a>(input: &'a str, layout: &mut Layout) -> Option<(Node, &'a str)> {
    let direction = match input.as_bytes().first() {
        Some(b'h') => Direction::H,
        Some(b'v') => Direction::V,
        _ => return parse_leaf(input, layout),
    };
    let rest = input[1..].strip_prefix('.')?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let ratio: f64 = format!("0.{}", &rest[..digits]).parse().ok()?;
    if ratio <= 0.0 {
        return None;
    }
    let rest = rest[digits..].strip_prefix('(')?;
    let (first, rest) = parse_node(rest, layout)?;
    let rest = rest.strip_prefix(',')?;
    let (second, rest) = parse_node(rest, layout)?;
    let rest = rest.strip_prefix(')')?;
    Some((
        Node::Split(Split {
            id: layout.next_id('s'),
            direction,
            ratio: clamp_ratio(ratio),
            first: Box::new(first),
            second: Box::new(second),
        }),
        rest,
    ))
}

fn parse_leaf<'a>(input: &'a str, layout: &mut Layout) -> Option<(Node, &'a str)> {
    let end = input.find(['(', ')', ',']).unwrap_or(input.len());
    let (raw, rest) = input.split_at(end);
    let mut parts = raw.split('>');
    let raw_path = parts.next().filter(|path| !path.is_empty())?;
    let raw_follow = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let path = percent_decode(raw_path).filter(|path| page_path(path))?;
    let follow = match raw_follow {
        None => None,
        Some(raw) => Some(percent_decode(raw).filter(|pattern| follow_pattern(pattern))?),
    };
    let mut leaf = layout.new_leaf(&path);
    leaf.follow = follow;
    Some((Node::Leaf(leaf), rest))
}

/// Decodes `%XX` escapes; a malformed escape or invalid UTF-8 is `None`.
pub fn percent_decode(raw: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(raw.len());
    let mut input = raw.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = (input.next()? as char).to_digit(16)?;
            let low = (input.next()? as char).to_digit(16)?;
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).ok()
}

/// Whether `path` names a page on this origin: one leading `/`, never
/// `//host`, a backslash a browser would read as `/`, or a space or control
/// character a browser strips before it parses. Every tile path holds to it,
/// so a tile can only ever show, or link to, a page of this application.
pub fn page_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.bytes().any(|b| b <= 0x20 || b == 0x7f)
}

/// Whether `pattern` is a page path with exactly one `:param` segment.
pub fn follow_pattern(pattern: &str) -> bool {
    page_path(pattern)
        && !pattern.contains('?')
        && pattern
            .split('/')
            .filter(|segment| segment.starts_with(':'))
            .count()
            == 1
}
