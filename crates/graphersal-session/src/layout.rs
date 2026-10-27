//! The force layout of the Graph panel: a ForceAtlas2-style layout with a Barnes-Hut quadtree,
//! computed in the engine (the playground's Web Worker, the dev server's session thread) instead
//! of on the page's main thread.
//!
//! Input is the topology of a drawing ([`LayoutInput`]: a vertex count and the edge
//! source/target columns of a [`crate::display::CompactGraph`]); output is one `(x, y)` pair per
//! vertex ([`ForceLayout::positions`], `f32`, interleaved). The layout is deterministic: the
//! same input and seed give the same positions on the same platform (one thread, a fixed
//! iteration order, a seeded generator for the start positions).
//!
//! Forces (Jacomy et al., "ForceAtlas2", PLoS ONE 2014), with mass `deg + 1`:
//! - repulsion `kr * m_i * m_j / d` between every pair, approximated with a Barnes-Hut quadtree
//!   (a cell of total mass `M` at distance `d` acts as one body when `size / d < theta`);
//! - linear attraction `d` along every edge (a multi edge counts once per edge, a self loop not);
//! - strong gravity `kg * m_i * d` towards the origin, which keeps disconnected parts together;
//! - adaptive speed: global speed from the swinging/traction ratio, per vertex damped by its own
//!   swinging.
//!
//! Runs in slices ([`ForceLayout::run_slice`]): iterations until a time budget is spent or the
//! cancellation token is set (wasm32 has no threads; the worker answers other requests between
//! slices). Degenerate inputs are handled: no vertex, one vertex, coinciding positions (a
//! deterministic tiny push apart; the quadtree stops subdividing at [`MAX_DEPTH`]), self loops,
//! multi edges; positions are always finite.
//!
//! [`LayoutJobs`] holds the running jobs of a session and a small cache of finished layouts,
//! keyed by the input (topology and options), so drawing the same view again costs nothing.

use crate::error::SessionError;
use std::collections::VecDeque;
use std::time::Duration;

use graphersal::exec::CancelToken;
use serde::Deserialize;
use serde_json::{Value, json};

/// Most vertices a layout takes (the Fast tier's upper end in the design).
pub const MAX_LAYOUT_VERTICES: usize = 200_000;

/// Most edges a layout takes.
pub const MAX_LAYOUT_EDGES: usize = 1_000_000;

/// Largest time budget of one slice, in milliseconds.
pub const MAX_SLICE_MS: f64 = 500.0;

/// Deepest quadtree level; bodies that still share a cell there (coinciding positions) form one
/// leaf and act on each other directly.
pub const MAX_DEPTH: u32 = 40;

/// The length the median edge gets in the returned positions (Cytoscape pixels at zoom 1; the
/// browser layout `cose` aims at 70 too).
pub const TARGET_EDGE_LENGTH: f64 = 70.0;

/// Barnes-Hut accuracy: a cell acts as one body when `size / distance < THETA`.
const THETA: f64 = 1.0;

/// Two bodies closer than this are pushed apart in a fixed direction instead.
const MIN_DISTANCE: f64 = 0.01;

/// The topology to lay out: vertex `i` for `i < vertex_count`, edge `j` from
/// `edge_source[j]` to `edge_target[j]` (vertex indices).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutInput {
    /// Number of vertices.
    pub vertex_count: usize,
    /// Per edge: the index of its out-vertex.
    #[serde(default)]
    pub edge_source: Vec<u32>,
    /// Per edge: the index of its in-vertex.
    #[serde(default)]
    pub edge_target: Vec<u32>,
}

impl LayoutInput {
    /// Checks the bounds: equal edge columns, indices below `vertex_count`, at most
    /// [`MAX_LAYOUT_VERTICES`] vertices and [`MAX_LAYOUT_EDGES`] edges.
    pub fn validate(&self) -> Result<(), SessionError> {
        if self.vertex_count > MAX_LAYOUT_VERTICES {
            return Err(format!(
                "a layout takes at most {MAX_LAYOUT_VERTICES} vertices, got {}",
                self.vertex_count
            )
            .into());
        }
        if self.edge_source.len() != self.edge_target.len() {
            return Err(format!(
                "edgeSource ({}) and edgeTarget ({}) must have the same length",
                self.edge_source.len(),
                self.edge_target.len()
            )
            .into());
        }
        if self.edge_source.len() > MAX_LAYOUT_EDGES {
            return Err(format!(
                "a layout takes at most {MAX_LAYOUT_EDGES} edges, got {}",
                self.edge_source.len()
            )
            .into());
        }
        if let Some(bad) = self
            .edge_source
            .iter()
            .chain(&self.edge_target)
            .find(|i| **i as usize >= self.vertex_count)
        {
            return Err(format!(
                "edge endpoint {bad} is not a vertex index (vertexCount {})",
                self.vertex_count
            )
            .into());
        }
        Ok(())
    }
}

/// The options of one layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayoutOptions {
    /// Seed of the start positions.
    pub seed: u64,
    /// Most iterations; `None`: [`default_iterations`] of the vertex count.
    pub iterations: Option<u32>,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            seed: 1,
            iterations: None,
        }
    }
}

/// The iterations a layout of `n` vertices runs by default: more for small drawings (cheap and
/// they show every detail), fewer for large ones (each iteration is `O(n log n)`).
pub fn default_iterations(n: usize) -> u32 {
    match n {
        0..=1 => 0,
        2..=1_000 => 500,
        1_001..=10_000 => 300,
        10_001..=50_000 => 200,
        _ => 120,
    }
}

/// What a slice ended with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceOutcome {
    /// The budget was spent; more iterations remain.
    Running,
    /// The layout is finished (all iterations, or converged).
    Done,
    /// The cancellation token was set; the positions are those reached so far.
    Cancelled,
}

/// One quadtree cell.
#[derive(Clone, Copy)]
struct Cell {
    mass: f64,
    cx: f64,
    cy: f64,
    size: f64,
    /// Children (`u32::MAX`: none); a leaf has none.
    children: [u32; 4],
    /// The leaf's bodies: `order[lo..hi]`.
    lo: u32,
    hi: u32,
}

const NONE: u32 = u32::MAX;

/// A ForceAtlas2-style force layout in progress (see the module docs).
pub struct ForceLayout {
    n: usize,
    /// Positions, interleaved `x, y`.
    pos: Vec<f64>,
    /// Forces of this and of the previous iteration, interleaved.
    force: Vec<f64>,
    previous: Vec<f64>,
    mass: Vec<f64>,
    /// Edges without self loops, as vertex index pairs.
    edges: Vec<(u32, u32)>,
    iteration: u32,
    total: u32,
    speed: f64,
    speed_efficiency: f64,
    scaling: f64,
    gravity: f64,
    slow_down: f64,
    converged: bool,
    // Reused buffers of the quadtree.
    order: Vec<u32>,
    cells: Vec<Cell>,
    stack: Vec<u32>,
    elapsed: Duration,
}

/// splitmix64: the seeded generator of the start positions (no dependency).
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A uniform number in `[0, 1)` from the generator.
fn unit(state: &mut u64) -> f64 {
    (splitmix(state) >> 11) as f64 / (1u64 << 53) as f64
}

/// A fixed direction for two coinciding bodies `i < j` (unit vector).
fn push_direction(i: usize, j: usize) -> (f64, f64) {
    let mut state = (i as u64).wrapping_mul(0x1000_0000_01B3) ^ (j as u64);
    let angle = unit(&mut state) * std::f64::consts::TAU;
    (angle.cos(), angle.sin())
}

impl ForceLayout {
    /// A layout of `input` (validated by the caller, see [`LayoutInput::validate`]) with the
    /// start positions drawn from `options.seed`.
    pub fn new(input: &LayoutInput, options: LayoutOptions) -> Self {
        let n = input.vertex_count;
        let mut degree = vec![0u32; n];
        let mut edges = Vec::with_capacity(input.edge_source.len());
        for (s, t) in input.edge_source.iter().zip(&input.edge_target) {
            let (s, t) = (*s as usize, *t as usize);
            if s >= n || t >= n || s == t {
                continue;
            }
            degree[s] += 1;
            degree[t] += 1;
            edges.push((s as u32, t as u32));
        }
        let mut state = options.seed;
        let side = (n as f64).sqrt().max(1.0) * 10.0;
        let mut pos = Vec::with_capacity(2 * n);
        for _ in 0..n {
            pos.push((unit(&mut state) - 0.5) * side);
            pos.push((unit(&mut state) - 0.5) * side);
        }
        let total = options.iterations.unwrap_or_else(|| default_iterations(n));
        Self {
            n,
            pos,
            force: vec![0.0; 2 * n],
            previous: vec![0.0; 2 * n],
            mass: degree.iter().map(|d| f64::from(*d) + 1.0).collect(),
            edges,
            iteration: 0,
            total: if n <= 1 { 0 } else { total },
            speed: 1.0,
            speed_efficiency: 1.0,
            scaling: if n > 100 { 2.0 } else { 10.0 },
            gravity: 0.05,
            slow_down: 1.0 + (n.max(1) as f64).ln(),
            converged: false,
            order: Vec::new(),
            cells: Vec::new(),
            stack: Vec::new(),
            elapsed: Duration::ZERO,
        }
    }

    /// Iterations done.
    pub fn iterations(&self) -> u32 {
        self.iteration
    }

    /// Iterations planned.
    pub fn total_iterations(&self) -> u32 {
        self.total
    }

    /// Whether the layout is finished.
    pub fn is_done(&self) -> bool {
        self.converged || self.iteration >= self.total
    }

    /// Time spent in iterations.
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Runs iterations until `budget` is spent (at least one, unless finished), the layout is
    /// done, or `cancel` is set (checked before every iteration).
    pub fn run_slice(&mut self, budget: Duration, cancel: &CancelToken) -> SliceOutcome {
        let start = web_time::Instant::now();
        let mut first = true;
        let outcome = loop {
            if self.is_done() {
                break SliceOutcome::Done;
            }
            if cancel.is_cancelled() {
                break SliceOutcome::Cancelled;
            }
            if !first && start.elapsed() >= budget {
                break SliceOutcome::Running;
            }
            first = false;
            self.step();
        };
        self.elapsed += start.elapsed();
        outcome
    }

    /// One iteration.
    pub fn step(&mut self) {
        if self.is_done() {
            return;
        }
        std::mem::swap(&mut self.force, &mut self.previous);
        self.force.iter_mut().for_each(|f| *f = 0.0);
        self.repulsion();
        self.attraction();
        self.gravity_pull();
        self.apply();
        self.iteration += 1;
    }

    /// Builds the quadtree over the current positions into `cells` / `order`.
    fn build_tree(&mut self) {
        self.cells.clear();
        self.order.clear();
        self.order.extend(0..self.n as u32);
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for i in 0..self.n {
            let (x, y) = (self.pos[2 * i], self.pos[2 * i + 1]);
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
        let size = (max_x - min_x).max(max_y - min_y).max(MIN_DISTANCE);
        self.build_cell(0, self.n, min_x, min_y, size, 0);
    }

    /// Builds the cell of `order[lo..hi]` within the square at `(x0, y0)` of side `size`;
    /// returns its index.
    fn build_cell(&mut self, lo: usize, hi: usize, x0: f64, y0: f64, size: f64, depth: u32) -> u32 {
        let index = self.cells.len() as u32;
        let (mut mass, mut sx, mut sy) = (0.0, 0.0, 0.0);
        for &b in &self.order[lo..hi] {
            let (b, m) = (b as usize, self.mass[b as usize]);
            mass += m;
            sx += m * self.pos[2 * b];
            sy += m * self.pos[2 * b + 1];
        }
        self.cells.push(Cell {
            mass,
            cx: sx / mass,
            cy: sy / mass,
            size,
            children: [NONE; 4],
            lo: lo as u32,
            hi: hi as u32,
        });
        if hi - lo <= 1 || depth >= MAX_DEPTH {
            return index;
        }
        let half = size / 2.0;
        let (mx, my) = (x0 + half, y0 + half);
        // Partition into left/right, then each half into bottom/top.
        let split_x = self.partition(lo, hi, |p| p.0 < mx);
        let split_lo = self.partition(lo, split_x, |p| p.1 < my);
        let split_hi = self.partition(split_x, hi, |p| p.1 < my);
        let quads = [
            (lo, split_lo, x0, y0),
            (split_lo, split_x, x0, my),
            (split_x, split_hi, mx, y0),
            (split_hi, hi, mx, my),
        ];
        let mut children = [NONE; 4];
        for (k, (a, b, qx, qy)) in quads.into_iter().enumerate() {
            if a < b {
                children[k] = self.build_cell(a, b, qx, qy, half, depth + 1);
            }
        }
        self.cells[index as usize].children = children;
        index
    }

    /// Reorders `order[lo..hi]` so the bodies whose position satisfies `left` come first;
    /// returns the first index of the rest.
    fn partition(&mut self, lo: usize, hi: usize, left: impl Fn((f64, f64)) -> bool) -> usize {
        let mut split = lo;
        for k in lo..hi {
            let b = self.order[k] as usize;
            if left((self.pos[2 * b], self.pos[2 * b + 1])) {
                self.order.swap(split, k);
                split += 1;
            }
        }
        split
    }

    fn repulsion(&mut self) {
        if self.n < 2 {
            return;
        }
        self.build_tree();
        let kr = self.scaling;
        let mut stack = std::mem::take(&mut self.stack);
        // In quadtree order: neighbouring bodies walk the same cells (cache friendly). Each
        // body's force is summed on its own, so the order does not change the result.
        for k in 0..self.n {
            let i = self.order[k] as usize;
            let (xi, yi, mi) = (self.pos[2 * i], self.pos[2 * i + 1], self.mass[i]);
            let (mut fx, mut fy) = (0.0, 0.0);
            stack.clear();
            stack.push(0);
            while let Some(c) = stack.pop() {
                let cell = self.cells[c as usize];
                let leaf = cell.children == [NONE; 4];
                if leaf {
                    for &j in &self.order[cell.lo as usize..cell.hi as usize] {
                        let j = j as usize;
                        if j == i {
                            continue;
                        }
                        let (mut dx, mut dy) = (xi - self.pos[2 * j], yi - self.pos[2 * j + 1]);
                        let mut d2 = dx * dx + dy * dy;
                        if d2 < MIN_DISTANCE * MIN_DISTANCE {
                            let (ux, uy) = push_direction(i.min(j), i.max(j));
                            let sign = if i < j { -1.0 } else { 1.0 };
                            dx = sign * ux * MIN_DISTANCE;
                            dy = sign * uy * MIN_DISTANCE;
                            d2 = MIN_DISTANCE * MIN_DISTANCE;
                        }
                        let f = kr * mi * self.mass[j] / d2;
                        fx += dx * f;
                        fy += dy * f;
                    }
                    continue;
                }
                let (dx, dy) = (xi - cell.cx, yi - cell.cy);
                let d2 = dx * dx + dy * dy;
                if cell.size * cell.size < THETA * THETA * d2 {
                    let f = kr * mi * cell.mass / d2;
                    fx += dx * f;
                    fy += dy * f;
                } else {
                    stack.extend(cell.children.iter().copied().filter(|c| *c != NONE));
                }
            }
            self.force[2 * i] += fx;
            self.force[2 * i + 1] += fy;
        }
        self.stack = stack;
    }

    fn attraction(&mut self) {
        for &(s, t) in &self.edges {
            let (s, t) = (s as usize, t as usize);
            let dx = self.pos[2 * s] - self.pos[2 * t];
            let dy = self.pos[2 * s + 1] - self.pos[2 * t + 1];
            self.force[2 * s] -= dx;
            self.force[2 * s + 1] -= dy;
            self.force[2 * t] += dx;
            self.force[2 * t + 1] += dy;
        }
    }

    fn gravity_pull(&mut self) {
        for i in 0..self.n {
            let g = self.gravity * self.mass[i];
            self.force[2 * i] -= g * self.pos[2 * i];
            self.force[2 * i + 1] -= g * self.pos[2 * i + 1];
        }
    }

    /// Adaptive speed (ForceAtlas2 section "Adaptive speed") and the move of every vertex.
    fn apply(&mut self) {
        let n = self.n as f64;
        let (mut swinging, mut traction) = (0.0, 0.0);
        for i in 0..self.n {
            let (fx, fy) = (self.force[2 * i], self.force[2 * i + 1]);
            let (px, py) = (self.previous[2 * i], self.previous[2 * i + 1]);
            let m = self.mass[i];
            swinging += m * ((px - fx).powi(2) + (py - fy).powi(2)).sqrt();
            traction += m * 0.5 * ((px + fx).powi(2) + (py + fy).powi(2)).sqrt();
        }
        if swinging == 0.0 || !swinging.is_finite() || !traction.is_finite() {
            // Nothing moves (all forces balanced) or a numeric fault: the layout is as good as
            // it gets.
            self.converged = true;
            return;
        }
        let optimal = 0.05 * n.sqrt();
        let min_jitter = optimal.sqrt();
        let mut jitter = min_jitter.max((optimal * traction / (n * n)).min(10.0));
        if swinging / traction > 2.0 {
            if self.speed_efficiency > 0.05 {
                self.speed_efficiency *= 0.5;
            }
            jitter = jitter.max(1.0);
        }
        let target = jitter * self.speed_efficiency * traction / swinging.max(f64::MIN_POSITIVE);
        if swinging > jitter * traction {
            if self.speed_efficiency > 0.05 {
                self.speed_efficiency *= 0.7;
            }
        } else if self.speed < 1000.0 {
            self.speed_efficiency *= 1.3;
        }
        self.speed += (target - self.speed).min(0.5 * self.speed);
        if !self.speed.is_finite() || self.speed <= 0.0 {
            self.speed = 1.0;
        }
        for i in 0..self.n {
            let (fx, fy) = (self.force[2 * i], self.force[2 * i + 1]);
            let (px, py) = (self.previous[2 * i], self.previous[2 * i + 1]);
            let swing = self.mass[i] * ((px - fx).powi(2) + (py - fy).powi(2)).sqrt();
            let factor = self.speed / (1.0 + (self.speed * swing).sqrt()) / self.slow_down;
            let (nx, ny) = (
                self.pos[2 * i] + fx * factor,
                self.pos[2 * i + 1] + fy * factor,
            );
            // Never let a numeric fault into the positions.
            if nx.is_finite() && ny.is_finite() {
                self.pos[2 * i] = nx;
                self.pos[2 * i + 1] = ny;
            }
        }
    }

    /// The positions, `x, y` per vertex (`2 * vertex_count` numbers), centred on the origin and
    /// scaled so the median edge is [`TARGET_EDGE_LENGTH`] long (without edges: about
    /// `TARGET_EDGE_LENGTH` of room per vertex). Always finite.
    pub fn positions(&self) -> Vec<f32> {
        let n = self.n;
        if n == 0 {
            return Vec::new();
        }
        let (mut sx, mut sy) = (0.0, 0.0);
        for i in 0..n {
            sx += self.pos[2 * i];
            sy += self.pos[2 * i + 1];
        }
        let (cx, cy) = (sx / n as f64, sy / n as f64);
        let mut lengths: Vec<f64> = self
            .edges
            .iter()
            .map(|&(s, t)| {
                let (s, t) = (s as usize, t as usize);
                (self.pos[2 * s] - self.pos[2 * t]).hypot(self.pos[2 * s + 1] - self.pos[2 * t + 1])
            })
            .filter(|l| *l > 0.0 && l.is_finite())
            .collect();
        let scale = if lengths.is_empty() {
            let mut extent: f64 = 0.0;
            for i in 0..n {
                extent = extent
                    .max((self.pos[2 * i] - cx).abs())
                    .max((self.pos[2 * i + 1] - cy).abs());
            }
            if extent > 0.0 {
                TARGET_EDGE_LENGTH * (n as f64).sqrt() / (2.0 * extent)
            } else {
                1.0
            }
        } else {
            let mid = lengths.len() / 2;
            let (_, median, _) = lengths.select_nth_unstable_by(mid, f64::total_cmp);
            TARGET_EDGE_LENGTH / *median
        };
        let scale = if scale.is_finite() { scale } else { 1.0 };
        let mut out = Vec::with_capacity(2 * n);
        for i in 0..n {
            for (k, c) in [(0, cx), (1, cy)] {
                let v = ((self.pos[2 * i + k] - c) * scale) as f32;
                out.push(if v.is_finite() { v } else { 0.0 });
            }
        }
        out
    }
}

/// How many layouts a session keeps running at once (more: the oldest is dropped).
const MAX_JOBS: usize = 4;

/// How many finished layouts a session keeps.
const MAX_CACHED: usize = 8;

/// A layout in progress.
struct Job {
    id: u64,
    key: (LayoutInput, LayoutOptions),
    layout: ForceLayout,
    state: SliceOutcome,
    cached: bool,
}

/// The layouts of a session: running jobs (each started with [`Self::start`], advanced with
/// [`Self::step`]) and the cache of finished ones.
#[derive(Default)]
pub struct LayoutJobs {
    next: u64,
    jobs: VecDeque<Job>,
    cache: VecDeque<((LayoutInput, LayoutOptions), Vec<f32>)>,
}

/// The JSON body of a layout start: the input and the options.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutRequest {
    /// The topology.
    #[serde(flatten)]
    pub input: LayoutInput,
    /// Seed of the start positions (default 1).
    #[serde(default)]
    pub seed: Option<u64>,
    /// Most iterations (default: [`default_iterations`]).
    #[serde(default)]
    pub iterations: Option<u32>,
}

impl LayoutRequest {
    /// The options of the request.
    pub fn options(&self) -> LayoutOptions {
        LayoutOptions {
            seed: self.seed.unwrap_or(1),
            iterations: self.iterations,
        }
    }
}

/// A slice budget in milliseconds as a duration: clamped to `0..=`[`MAX_SLICE_MS`].
pub fn slice_budget(ms: f64) -> Duration {
    let ms = if ms.is_finite() {
        ms.clamp(0.0, MAX_SLICE_MS)
    } else {
        0.0
    };
    Duration::from_secs_f64(ms / 1000.0)
}

impl LayoutJobs {
    /// Starts a layout of `input` (or finds it in the cache: `state` is `done` at once, `cached`
    /// true) and returns its job id.
    pub fn start(
        &mut self,
        input: LayoutInput,
        options: LayoutOptions,
    ) -> Result<u64, SessionError> {
        input.validate()?;
        self.next += 1;
        let id = self.next;
        let key = (input, options);
        let cached = self.cache.iter().position(|(k, _)| *k == key);
        let mut layout = ForceLayout::new(&key.0, options);
        let state = if let Some(found) = cached {
            // Move the hit to the back (most recently used); the job is finished.
            if let Some(entry) = self.cache.remove(found) {
                self.cache.push_back(entry);
            }
            layout.converged = true;
            SliceOutcome::Done
        } else {
            SliceOutcome::Running
        };
        if self.jobs.len() == MAX_JOBS {
            self.jobs.pop_front();
        }
        self.jobs.push_back(Job {
            id,
            key,
            layout,
            state,
            cached: cached.is_some(),
        });
        Ok(id)
    }

    fn job(&mut self, id: u64) -> Result<&mut Job, SessionError> {
        self.jobs
            .iter_mut()
            .find(|j| j.id == id)
            .ok_or_else(|| {
                format!("no layout job {id} (finished long ago, or replaced by newer ones)")
            })
            .map_err(SessionError::from)
    }

    /// Runs one slice of job `id` within `budget`; `cancel` ends it early as cancelled. A
    /// finished job stays finished.
    pub fn step(
        &mut self,
        id: u64,
        budget: Duration,
        cancel: &CancelToken,
    ) -> Result<Value, SessionError> {
        let job = self.job(id)?;
        if job.state == SliceOutcome::Running {
            job.state = job.layout.run_slice(budget, cancel);
        }
        let status = status_of(job);
        if job.state == SliceOutcome::Done && !job.cached {
            let entry = (job.key.clone(), job.layout.positions());
            job.cached = true;
            if self.cache.len() == MAX_CACHED {
                self.cache.pop_front();
            }
            self.cache.push_back(entry);
        }
        Ok(status)
    }

    /// Cancels job `id`: it keeps the positions reached so far.
    pub fn cancel(&mut self, id: u64) -> Result<Value, SessionError> {
        let job = self.job(id)?;
        if job.state == SliceOutcome::Running {
            job.state = SliceOutcome::Cancelled;
        }
        Ok(status_of(job))
    }

    /// The status of job `id` (see [`Self::step`]).
    pub fn status(&mut self, id: u64) -> Result<Value, SessionError> {
        Ok(status_of(self.job(id)?))
    }

    /// The positions of job `id` (`x, y` per vertex; the cached ones of a cache hit). A running
    /// job answers the positions reached so far.
    pub fn positions(&mut self, id: u64) -> Result<Vec<f32>, SessionError> {
        self.job(id)?;
        let Some(job) = self.jobs.iter().find(|j| j.id == id) else {
            return Err(format!("no layout job {id}").into());
        };
        if job.cached
            && let Some((_, positions)) = self.cache.iter().find(|(k, _)| *k == job.key)
        {
            return Ok(positions.clone());
        }
        Ok(job.layout.positions())
    }

    /// Forgets job `id` (its positions were fetched).
    pub fn finish(&mut self, id: u64) {
        self.jobs.retain(|j| j.id != id);
    }
}

/// `{job, state, iterations, totalIterations, progress, elapsedMs, vertexCount, cached}`;
/// `state` is `running`, `done` or `cancelled`.
fn status_of(job: &Job) -> Value {
    let layout = &job.layout;
    let total = layout.total_iterations();
    let progress = match job.state {
        SliceOutcome::Done => 1.0,
        _ if total == 0 => 1.0,
        _ => f64::from(layout.iterations()) / f64::from(total),
    };
    json!({
        "job": job.id,
        "state": match job.state {
            SliceOutcome::Running => "running",
            SliceOutcome::Done => "done",
            SliceOutcome::Cancelled => "cancelled",
        },
        "iterations": layout.iterations(),
        "totalIterations": total,
        "progress": progress,
        "elapsedMs": layout.elapsed().as_secs_f64() * 1000.0,
        "vertexCount": layout.n,
        "cached": job.cached && layout.iterations() == 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(n: usize, edges: &[(u32, u32)]) -> LayoutInput {
        LayoutInput {
            vertex_count: n,
            edge_source: edges.iter().map(|e| e.0).collect(),
            edge_target: edges.iter().map(|e| e.1).collect(),
        }
    }

    fn run(input: &LayoutInput, options: LayoutOptions) -> Vec<f32> {
        let mut layout = ForceLayout::new(input, options);
        let outcome = layout.run_slice(Duration::from_secs(60), &CancelToken::new());
        assert_eq!(outcome, SliceOutcome::Done);
        layout.positions()
    }

    fn assert_finite(positions: &[f32], n: usize) {
        assert_eq!(positions.len(), 2 * n);
        assert!(positions.iter().all(|v| v.is_finite()), "{positions:?}");
    }

    /// A ring of `n` vertices plus chords: connected, deterministic.
    fn ring(n: u32) -> LayoutInput {
        let mut edges: Vec<(u32, u32)> = (0..n).map(|i| (i, (i + 1) % n)).collect();
        edges.extend((0..n).step_by(7).map(|i| (i, (i * 3 + 5) % n)));
        input(n as usize, &edges)
    }

    #[test]
    fn the_same_seed_gives_the_same_positions() {
        let g = ring(300);
        let a = run(&g, LayoutOptions::default());
        let b = run(&g, LayoutOptions::default());
        assert_eq!(a, b);
        let c = run(
            &g,
            LayoutOptions {
                seed: 2,
                ..LayoutOptions::default()
            },
        );
        assert_ne!(a, c, "another seed starts elsewhere");
        assert_finite(&a, 300);
    }

    #[test]
    fn slices_give_the_same_result_as_one_run() {
        let g = ring(120);
        let whole = run(&g, LayoutOptions::default());
        let mut layout = ForceLayout::new(&g, LayoutOptions::default());
        let token = CancelToken::new();
        let mut slices = 0;
        while layout.run_slice(Duration::ZERO, &token) == SliceOutcome::Running {
            slices += 1;
        }
        // A zero budget still makes progress: exactly one iteration per slice.
        assert_eq!(slices + 1, layout.total_iterations() as usize);
        assert_eq!(layout.positions(), whole);
    }

    #[test]
    fn degenerate_inputs_stay_finite() {
        // No vertex, one vertex.
        assert!(run(&input(0, &[]), LayoutOptions::default()).is_empty());
        let one = run(&input(1, &[(0, 0)]), LayoutOptions::default());
        assert_eq!(one, vec![0.0, 0.0]);
        // Two vertices, isolated: pushed apart, finite.
        assert_finite(&run(&input(2, &[]), LayoutOptions::default()), 2);
        // Self loops and multi edges.
        let loops = input(4, &[(0, 0), (0, 1), (0, 1), (0, 1), (1, 2), (2, 2), (3, 3)]);
        let p = run(&loops, LayoutOptions::default());
        assert_finite(&p, 4);
        // Disconnected components: every vertex placed, apart from each other.
        let parts = input(6, &[(0, 1), (1, 2), (3, 4), (4, 5)]);
        let p = run(&parts, LayoutOptions::default());
        assert_finite(&p, 6);
        for i in 0..6 {
            for j in i + 1..6 {
                let d = (p[2 * i] - p[2 * j]).hypot(p[2 * i + 1] - p[2 * j + 1]);
                assert!(d > 1.0, "{i} and {j} overlap: {p:?}");
            }
        }
    }

    #[test]
    fn coinciding_vertices_are_pushed_apart() {
        let g = ring(50);
        let mut layout = ForceLayout::new(&g, LayoutOptions::default());
        layout.pos.iter_mut().for_each(|v| *v = 3.0); // all at one point
        let token = CancelToken::new();
        assert_eq!(
            layout.run_slice(Duration::from_secs(60), &token),
            SliceOutcome::Done
        );
        let p = layout.positions();
        assert_finite(&p, 50);
        let distinct: std::collections::HashSet<(u32, u32)> = (0..50)
            .map(|i| (p[2 * i].to_bits(), p[2 * i + 1].to_bits()))
            .collect();
        assert_eq!(distinct.len(), 50, "every vertex has its own place");
        // Many coinciding vertices without edges: the quadtree stops at its depth limit.
        let mut crowd = ForceLayout::new(&input(500, &[]), LayoutOptions::default());
        crowd.pos.iter_mut().for_each(|v| *v = 0.0);
        crowd.step();
        assert!(crowd.pos.iter().all(|v| v.is_finite()));
        assert_finite(&crowd.positions(), 500);
    }

    #[test]
    fn the_median_edge_gets_the_target_length() {
        let p = run(&ring(200), LayoutOptions::default());
        let g = ring(200);
        let mut lengths: Vec<f32> = g
            .edge_source
            .iter()
            .zip(&g.edge_target)
            .map(|(s, t)| {
                let (s, t) = (*s as usize, *t as usize);
                (p[2 * s] - p[2 * t]).hypot(p[2 * s + 1] - p[2 * t + 1])
            })
            .collect();
        lengths.sort_by(f32::total_cmp);
        let median = lengths[lengths.len() / 2];
        assert!((median - TARGET_EDGE_LENGTH as f32).abs() < 1.0, "{median}");
        // Neighbours end up closer than the average pair: the layout shows structure.
        let mean_edge: f32 = lengths.iter().sum::<f32>() / lengths.len() as f32;
        let mut pair_sum = 0.0f32;
        let mut pairs = 0;
        for i in (0..200).step_by(13) {
            for j in (1..200).step_by(17) {
                pair_sum += (p[2 * i] - p[2 * j]).hypot(p[2 * i + 1] - p[2 * j + 1]);
                pairs += 1;
            }
        }
        assert!(
            mean_edge < pair_sum / pairs as f32,
            "{mean_edge} vs {}",
            pair_sum / pairs as f32
        );
    }

    #[test]
    fn a_slice_is_bounded_by_its_budget_and_the_token() {
        let g = ring(5000);
        let mut layout = ForceLayout::new(&g, LayoutOptions::default());
        let token = CancelToken::new();
        // Time one iteration, then check that a 30 ms slice ends within the budget plus one
        // iteration (and a generous margin for a loaded machine).
        let started = web_time::Instant::now();
        assert_eq!(
            layout.run_slice(Duration::ZERO, &token),
            SliceOutcome::Running
        );
        let one = started.elapsed();
        let started = web_time::Instant::now();
        assert_eq!(
            layout.run_slice(Duration::from_millis(30), &token),
            SliceOutcome::Running
        );
        let slice = started.elapsed();
        assert!(
            slice <= Duration::from_millis(30) + one * 3 + Duration::from_millis(50),
            "slice {slice:?}, one iteration {one:?}"
        );
        token.cancel();
        let before = layout.iterations();
        assert_eq!(
            layout.run_slice(Duration::from_secs(1), &token),
            SliceOutcome::Cancelled
        );
        assert_eq!(
            layout.iterations(),
            before,
            "a cancelled slice runs no iteration"
        );
        assert_finite(&layout.positions(), 5000);
    }

    #[test]
    fn invalid_inputs_are_refused() {
        assert!(
            input(2, &[(0, 2)])
                .validate()
                .unwrap_err()
                .to_string()
                .contains("not a vertex index")
        );
        let mut uneven = input(2, &[(0, 1)]);
        uneven.edge_target.clear();
        assert!(
            uneven
                .validate()
                .unwrap_err()
                .to_string()
                .contains("same length")
        );
        assert!(input(MAX_LAYOUT_VERTICES + 1, &[]).validate().is_err());
    }

    #[test]
    fn jobs_run_in_slices_cache_and_cancel() {
        let mut jobs = LayoutJobs::default();
        let token = CancelToken::new();
        let g = ring(100);
        let id = jobs.start(g.clone(), LayoutOptions::default()).unwrap();
        let mut status = jobs.step(id, Duration::ZERO, &token).unwrap();
        assert_eq!(status["state"], "running");
        assert_eq!(status["iterations"], 1);
        while status["state"] == "running" {
            status = jobs.step(id, Duration::from_millis(50), &token).unwrap();
        }
        assert_eq!(status["state"], "done");
        assert_eq!(status["progress"], 1.0);
        let first = jobs.positions(id).unwrap();
        jobs.finish(id);
        assert!(jobs.status(id).is_err(), "a finished job is forgotten");
        // The same input again: answered from the cache at once.
        let again = jobs.start(g.clone(), LayoutOptions::default()).unwrap();
        let status = jobs.status(again).unwrap();
        assert_eq!(status["state"], "done");
        assert_eq!(status["cached"], true);
        assert_eq!(jobs.positions(again).unwrap(), first);
        // Cancel keeps the positions reached so far.
        let other = jobs
            .start(
                g,
                LayoutOptions {
                    seed: 9,
                    iterations: None,
                },
            )
            .unwrap();
        jobs.step(other, Duration::ZERO, &token).unwrap();
        let status = jobs.cancel(other).unwrap();
        assert_eq!(status["state"], "cancelled");
        assert_finite(&jobs.positions(other).unwrap(), 100);
        let status = jobs.step(other, Duration::from_secs(1), &token).unwrap();
        assert_eq!(
            status["state"], "cancelled",
            "a cancelled job stays cancelled"
        );
        // Unknown jobs and bad inputs are errors, never panics.
        assert!(jobs.step(12345, Duration::ZERO, &token).is_err());
        assert!(
            jobs.start(input(1, &[(0, 1)]), LayoutOptions::default())
                .is_err()
        );
    }

    #[test]
    fn slice_budgets_are_clamped() {
        assert_eq!(slice_budget(f64::NAN), Duration::ZERO);
        assert_eq!(slice_budget(-5.0), Duration::ZERO);
        assert_eq!(
            slice_budget(1e12),
            Duration::from_millis(MAX_SLICE_MS as u64)
        );
    }
}
