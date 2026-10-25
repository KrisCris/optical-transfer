use std::collections::{BTreeSet, HashSet};

use crate::{OpticalError, V3_MAX_FILE_BYTES, protocol::SplitMix32};

const LN2: f64 = std::f64::consts::LN_2;
const SOLITON_C: f64 = 0.1;
const SOLITON_DELTA: f64 = 0.5;
const REPAIR_DEGREE_MIN: u32 = 4;
const REPAIR_DEGREE_MAX: u32 = 24;

/// Deterministic natural log: exact-ops range reduction plus an atanh series.
///
/// This is wire format, not a utility. The upstream baseline uses it instead of
/// the platform `log` because an implementation-approximated logarithm can
/// differ by an ulp between a sender's engine and a receiver's, which is enough
/// to shift a `soliton_cdf` boundary and desynchronise the two streams. Every
/// operation below is exactly specified by IEEE-754, so the value is identical
/// on every target.
///
/// Inputs outside `(0, inf)` are answered from `f64::ln`'s own definition
/// rather than entered into the range reduction: zero never climbs to 0.75 by
/// doubling and infinity never falls below 1.5 by halving, so an out-of-domain
/// caller would otherwise spin forever (or, in a debug build, panic on the
/// exponent counter). The guard cannot fire for anything `soliton_cdf` asks
/// for -- it only ever passes `k / 0.5` for `k >= 2` and `R / 0.5` for
/// `R >= 1` -- so the pinned upstream vectors are unaffected.
pub fn dlog(value: f64) -> f64 {
    if value.is_nan() || value < 0.0 {
        return f64::NAN;
    }
    if value == 0.0 {
        return f64::NEG_INFINITY;
    }
    if value.is_infinite() {
        return f64::INFINITY;
    }

    let mut exponent = 0;
    let mut mantissa = value;
    while mantissa >= 1.5 {
        mantissa /= 2.0;
        exponent += 1;
    }
    while mantissa < 0.75 {
        mantissa *= 2.0;
        exponent -= 1;
    }
    let z = (mantissa - 1.0) / (mantissa + 1.0);
    let z_squared = z * z;
    let mut term = z;
    let mut sum = 0.0;
    for denominator in (1..=21).step_by(2) {
        sum += term / f64::from(denominator);
        term *= z_squared;
    }
    f64::from(exponent) * LN2 + 2.0 * sum
}

pub fn soliton_cdf(k: u16) -> Vec<f64> {
    let k_usize = usize::from(k);
    let mut cdf = vec![0.0; k_usize];
    if k == 1 {
        cdf[0] = 1.0;
        return cdf;
    }

    let k_f64 = f64::from(k);
    let r = (SOLITON_C * dlog(k_f64 / SOLITON_DELTA) * k_f64.sqrt()).max(1.0);
    let spike = k_usize.min((k_f64 / r).ceil() as usize);
    let mut total = 0.0;
    for degree in 1..=k_usize {
        let degree_f64 = degree as f64;
        let rho = if degree == 1 {
            1.0 / k_f64
        } else {
            1.0 / (degree_f64 * (degree_f64 - 1.0))
        };
        let tau = if degree < spike {
            r / (degree_f64 * k_f64)
        } else if degree == spike {
            r * dlog(r / SOLITON_DELTA).max(0.0) / k_f64
        } else {
            0.0
        };
        total += rho + tau;
        cdf[degree - 1] = total;
    }
    for value in &mut cdf {
        *value /= total;
    }
    cdf[k_usize - 1] = 1.0;
    cdf
}

fn frame_seed(session_id: u16, seq: u32) -> u32 {
    let mut hash = u32::from(session_id)
        .wrapping_add(1)
        .wrapping_mul(0x9e37_79b1)
        ^ seq.wrapping_add(0x85eb_ca6b);
    hash = (hash ^ (hash >> 13)).wrapping_mul(0xc2b2_ae35);
    hash ^ (hash >> 16)
}

pub fn frame_indices(k: u16, cdf: &[f64], session_id: u16, seq: u32) -> Vec<u16> {
    if k == 0 || cdf.len() != usize::from(k) {
        return Vec::new();
    }

    let mut random = SplitMix32::new(frame_seed(session_id, seq));
    let sample = f64::from(random.next_u32()) * (1.0 / 4_294_967_296.0);
    let mut low = 0_usize;
    let mut high = usize::from(k) - 1;
    while low < high {
        let middle = (low + high) >> 1;
        if cdf[middle] >= sample {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    let degree = usize::from(k).min(low + 1);
    if degree > (usize::from(k) >> 3) {
        let mut scratch: Vec<u16> = (0..k).collect();
        let mut output = Vec::with_capacity(degree);
        for index in 0..degree {
            let selected = index + (random.next_u32() as usize % (usize::from(k) - index));
            scratch.swap(index, selected);
            output.push(scratch[index]);
        }
        return output;
    }

    distinct_indices(usize::from(k), degree, &mut random)
}

pub fn cycle_length(k: u16) -> u32 {
    u32::from(k) * 2
}

fn repair_indices(k: u16, session_id: u16, seq: u32) -> Vec<u16> {
    let mut random = SplitMix32::new(frame_seed(session_id, seq));
    let degree = usize::from(
        (REPAIR_DEGREE_MIN + random.next_u32() % (REPAIR_DEGREE_MAX - REPAIR_DEGREE_MIN + 1))
            .min(u32::from(k)) as u16,
    );
    distinct_indices(usize::from(k), degree, &mut random)
}

fn distinct_indices(population: usize, count: usize, random: &mut SplitMix32) -> Vec<u16> {
    let mut output = Vec::with_capacity(count);
    while output.len() < count {
        let value = (random.next_u32() as usize % population) as u16;
        if !output.contains(&value) {
            output.push(value);
        }
    }
    output
}

pub fn frame_composition(k: u16, session_id: u16, seq: u32) -> Vec<u16> {
    if k == 0 {
        return Vec::new();
    }
    let position = seq % cycle_length(k);
    if position < u32::from(k) {
        vec![position as u16]
    } else {
        repair_indices(k, session_id, seq)
    }
}

pub struct LtEncoder {
    k: u16,
    block_len: usize,
    session_id: u16,
    blocks: Vec<u32>,
    words_per_block: usize,
}

impl LtEncoder {
    pub fn new(payload: &[u8], block_len: usize, session_id: u16) -> Result<Self, OpticalError> {
        if block_len == 0 || block_len > usize::from(u16::MAX) || payload.len() > V3_MAX_FILE_BYTES
        {
            return Err(OpticalError::UnsupportedDimensions);
        }
        let k = payload.len().div_ceil(block_len).max(1);
        if k > usize::from(u16::MAX) {
            return Err(OpticalError::UnsupportedDimensions);
        }

        let words_per_block = block_len.div_ceil(4);
        let mut blocks = vec![0_u32; k * words_per_block];
        for (block_index, chunk) in payload.chunks(block_len).enumerate() {
            let start = block_index * words_per_block;
            for (word_index, bytes) in chunk.chunks(4).enumerate() {
                let mut word = [0_u8; 4];
                word[..bytes.len()].copy_from_slice(bytes);
                blocks[start + word_index] = u32::from_le_bytes(word);
            }
        }

        Ok(Self {
            k: k as u16,
            block_len,
            session_id,
            blocks,
            words_per_block,
        })
    }

    pub fn source_blocks(&self) -> u16 {
        self.k
    }

    pub fn encode(&self, seq: u32) -> Vec<u8> {
        let mut words = vec![0_u32; self.words_per_block];
        for block_index in frame_composition(self.k, self.session_id, seq) {
            let start = usize::from(block_index) * self.words_per_block;
            for (destination, source) in words
                .iter_mut()
                .zip(&self.blocks[start..start + self.words_per_block])
            {
                *destination ^= source;
            }
        }
        words_to_bytes(&words, self.block_len)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddFrame {
    Accepted,
    Duplicate,
    Redundant,
    Complete,
}

struct Equation {
    indices: Vec<u16>,
    words: Vec<u32>,
}

/// Largest unsolved-block count the decoder will hand to Gaussian elimination.
///
/// Elimination is dense: the symbolic pass is `O(u³/64)` and the payload pass
/// `O(u² · block words)`. At 1024 unknowns that is a few tens of milliseconds
/// in a phone's WebAssembly — paid once, at the moment the transfer finishes.
/// Above it the decoder keeps peeling until the next sweep brings the unknown
/// count under the limit.
pub const ELIMINATION_LIMIT: usize = 1024;

pub struct LtDecoder {
    k: u16,
    block_len: usize,
    session_id: u16,
    total_len: usize,
    words_per_block: usize,
    solved: Vec<Option<Vec<u32>>>,
    pending: Vec<Option<Equation>>,
    /// Live entries in `pending`.
    pending_count: usize,
    by_block: Vec<BTreeSet<usize>>,
    seen: HashSet<u32>,
    solved_count: usize,
    unique_frames: usize,
    redundant_frames: usize,
    /// Informative frames still to arrive before elimination can possibly
    /// succeed. Every frame raises the system's rank by at most one, so after
    /// a pass that came up `d` short, the next `d - 1` frames cannot fix it.
    elimination_wait: usize,
    eliminations: usize,
}

impl LtDecoder {
    /// Build a decoder for the dimensions a frame header declares.
    ///
    /// Every argument arrives over the optical channel, so the dimensions are
    /// checked for internal consistency before anything is allocated. In
    /// particular `k` must be the *minimal* block count for `total_len`:
    /// an honest sender always emits `ceil(total_len / block_len)`, so a larger
    /// `k` claims storage the payload cannot use. Without that bound a stream
    /// declaring a 1 KiB payload at `k = 65535`, `block_len = 65535` would
    /// still be entitled to roughly 4 GiB of solved-block buffers, even though
    /// `total_len` itself is capped at 64 MiB. With it, capacity can exceed the
    /// payload by at most one partial block.
    pub fn new(
        k: u16,
        block_len: usize,
        session_id: u16,
        total_len: usize,
    ) -> Result<Self, OpticalError> {
        let capacity = usize::from(k).saturating_mul(block_len);
        if k == 0
            || block_len == 0
            || block_len > usize::from(u16::MAX)
            || total_len == 0
            || total_len > V3_MAX_FILE_BYTES
            || total_len > capacity
            || total_len <= capacity - block_len
        {
            return Err(OpticalError::UnsupportedDimensions);
        }
        let words_per_block = block_len.div_ceil(4);
        let k_usize = usize::from(k);
        Ok(Self {
            k,
            block_len,
            session_id,
            total_len,
            words_per_block,
            solved: vec![None; k_usize],
            pending: Vec::new(),
            pending_count: 0,
            by_block: (0..k_usize).map(|_| BTreeSet::new()).collect(),
            seen: HashSet::new(),
            solved_count: 0,
            unique_frames: 0,
            redundant_frames: 0,
            elimination_wait: 0,
            eliminations: 0,
        })
    }

    pub fn add_frame(&mut self, seq: u32, block: &[u8]) -> Result<AddFrame, OpticalError> {
        if block.len() != self.block_len {
            return Err(OpticalError::InvalidFrame);
        }
        if !self.seen.insert(seq) {
            return Ok(AddFrame::Duplicate);
        }
        self.unique_frames += 1;
        if self.is_complete() {
            return Ok(AddFrame::Complete);
        }

        let mut indices = frame_composition(self.k, self.session_id, seq);
        indices.sort_unstable();
        indices.dedup();
        let mut words = bytes_to_words(block, self.words_per_block);
        for position in (0..indices.len()).rev() {
            let index = usize::from(indices[position]);
            if let Some(solved) = &self.solved[index] {
                xor_words(&mut words, solved);
                indices.remove(position);
            }
        }
        if indices.is_empty() {
            self.redundant_frames += 1;
            return Ok(AddFrame::Redundant);
        }
        if indices.len() == 1 {
            self.resolve(indices[0], words);
        } else {
            let equation_id = self.pending.len();
            for index in &indices {
                self.by_block[usize::from(*index)].insert(equation_id);
            }
            self.pending.push(Some(Equation { indices, words }));
            self.pending_count += 1;
        }

        self.elimination_wait = self.elimination_wait.saturating_sub(1);
        if !self.is_complete() && self.elimination_due() {
            self.eliminate();
        }
        Ok(if self.is_complete() {
            AddFrame::Complete
        } else {
            AddFrame::Accepted
        })
    }

    pub fn solved_count(&self) -> usize {
        self.solved_count
    }

    pub fn source_blocks(&self) -> u16 {
        self.k
    }

    /// Whether source block `index` has been recovered.
    pub fn is_solved(&self, index: usize) -> bool {
        self.solved.get(index).is_some_and(Option::is_some)
    }

    /// Equations parked waiting on blocks peeling has not reached.
    pub fn pending_equations(&self) -> usize {
        self.pending_count
    }

    /// Elimination passes run so far, successful or not.
    pub fn eliminations(&self) -> usize {
        self.eliminations
    }

    pub fn unique_frames(&self) -> usize {
        self.unique_frames
    }

    pub fn redundant_frames(&self) -> usize {
        self.redundant_frames
    }

    pub fn is_complete(&self) -> bool {
        self.solved_count == usize::from(self.k)
    }

    pub fn assemble(&self) -> Result<Vec<u8>, OpticalError> {
        if !self.is_complete() {
            return Err(OpticalError::IncompleteTransfer);
        }
        let mut output = Vec::with_capacity(self.total_len);
        for words in self.solved.iter().flatten() {
            output.extend_from_slice(&words_to_bytes(words, self.block_len));
        }
        output.truncate(self.total_len);
        Ok(output)
    }

    fn resolve(&mut self, initial_block: u16, initial_words: Vec<u32>) {
        let mut queue = vec![(initial_block, initial_words)];
        while let Some((block, words)) = queue.pop() {
            let block_index = usize::from(block);
            if self.solved[block_index].is_some() {
                continue;
            }
            self.solved[block_index] = Some(words.clone());
            self.solved_count += 1;
            let waiting = std::mem::take(&mut self.by_block[block_index]);
            for equation_id in waiting {
                let degree_one = {
                    let Some(equation) = self.pending[equation_id].as_mut() else {
                        continue;
                    };
                    xor_words(&mut equation.words, &words);
                    let position = equation
                        .indices
                        .iter()
                        .position(|index| *index == block)
                        .expect("indexed equations contain their block");
                    equation.indices.remove(position);
                    equation.indices.len() == 1
                };
                if degree_one {
                    let equation = self.pending[equation_id]
                        .take()
                        .expect("degree-one equation remains pending");
                    self.pending_count -= 1;
                    let remaining = equation.indices[0];
                    self.by_block[usize::from(remaining)].remove(&equation_id);
                    queue.push((remaining, equation.words));
                }
            }
        }
    }

    fn elimination_due(&self) -> bool {
        let unknowns = usize::from(self.k) - self.solved_count;
        self.elimination_wait == 0
            && unknowns <= ELIMINATION_LIMIT
            && self.pending_count >= unknowns
            // A block no parked equation mentions is a zero column: no amount
            // of elimination recovers it, so do not pay for trying. This is
            // the common failure near the end of a repair half.
            && self
                .solved
                .iter()
                .zip(&self.by_block)
                .all(|(solved, waiting)| solved.is_some() || !waiting.is_empty())
    }

    /// Solve every remaining block at once from the parked equations.
    ///
    /// Peeling alone stalls whenever no parked equation is down to one
    /// unknown, which under loss is most of the carousel's repair half: a
    /// degree-14 repair frame over a 40%-unsolved file still covers five or
    /// six unknowns. Those equations are not useless, only unpeelable — once
    /// there are as many independent ones as unknowns, the file is fully
    /// determined. Waiting for the next systematic sweep instead costs up to
    /// half a carousel of extra frames.
    ///
    /// Two passes, because the payload XORs dominate: a symbolic pass on
    /// bitsets finds rank and a pivot set, and only a full-rank system pays
    /// for the payload pass over that square subsystem.
    fn eliminate(&mut self) {
        self.eliminations += 1;
        let unknown: Vec<u16> = (0..self.k)
            .filter(|&block| self.solved[usize::from(block)].is_none())
            .collect();
        let columns = unknown.len();
        let mut column_of = vec![usize::MAX; usize::from(self.k)];
        for (column, &block) in unknown.iter().enumerate() {
            column_of[usize::from(block)] = column;
        }
        let row_ids: Vec<usize> = (0..self.pending.len())
            .filter(|&id| self.pending[id].is_some())
            .collect();
        let stride = columns.div_ceil(64);
        let bits_of = |equation: &Equation| {
            let mut row = vec![0_u64; stride];
            for &index in &equation.indices {
                let column = column_of[usize::from(index)];
                row[column / 64] |= 1 << (column % 64);
            }
            row
        };

        // Symbolic forward elimination. A row chosen as pivot is its original
        // equation plus earlier pivots only, so the chosen originals are
        // themselves independent and form a square full-rank subsystem.
        let mut rows: Vec<Vec<u64>> = row_ids
            .iter()
            .map(|&id| bits_of(self.pending[id].as_ref().expect("live row")))
            .collect();
        let mut order: Vec<usize> = (0..rows.len()).collect();
        let mut pivots = Vec::with_capacity(columns);
        for column in 0..columns {
            let (word, bit) = (column / 64, 1_u64 << (column % 64));
            let Some(found) =
                (pivots.len()..order.len()).find(|&r| rows[order[r]][word] & bit != 0)
            else {
                continue;
            };
            order.swap(pivots.len(), found);
            let pivot = order[pivots.len()];
            pivots.push(pivot);
            let (head, pivot_row) = (word, rows[pivot].clone());
            for &other in &order[pivots.len()..] {
                if rows[other][word] & bit != 0 {
                    for (left, right) in rows[other][head..].iter_mut().zip(&pivot_row[head..]) {
                        *left ^= right;
                    }
                }
            }
        }
        if pivots.len() < columns {
            self.elimination_wait = columns - pivots.len();
            return;
        }

        // Payload pass: Gauss-Jordan over the pivot rows only.
        let mut system: Vec<(Vec<u64>, Vec<u32>)> = pivots
            .iter()
            .map(|&row| {
                let equation = self.pending[row_ids[row]].as_ref().expect("live row");
                (bits_of(equation), equation.words.clone())
            })
            .collect();
        for column in 0..columns {
            let (word, bit) = (column / 64, 1_u64 << (column % 64));
            let found = (column..columns)
                .find(|&r| system[r].0[word] & bit != 0)
                .expect("a full-rank subsystem has a pivot in every column");
            system.swap(column, found);
            let (before, rest) = system.split_at_mut(column);
            let (pivot, after) = rest.split_first_mut().expect("pivot row exists");
            for (bits, words) in before.iter_mut().chain(after.iter_mut()) {
                if bits[word] & bit != 0 {
                    for (left, right) in bits.iter_mut().zip(&pivot.0) {
                        *left ^= right;
                    }
                    xor_words(words, &pivot.1);
                }
            }
        }
        for (column, (_, words)) in system.into_iter().enumerate() {
            self.resolve(unknown[column], words);
        }
    }
}

fn xor_words(destination: &mut [u32], source: &[u32]) {
    for (left, right) in destination.iter_mut().zip(source) {
        *left ^= right;
    }
}

fn bytes_to_words(bytes: &[u8], words_per_block: usize) -> Vec<u32> {
    let mut words = vec![0_u32; words_per_block];
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let mut word = [0_u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        words[index] = u32::from_le_bytes(word);
    }
    words
}

fn words_to_bytes(words: &[u32], length: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.truncate(length);
    bytes
}
