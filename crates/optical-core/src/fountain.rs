use std::collections::{BTreeSet, HashSet};

use crate::{MAX_FILE_BYTES, OpticalError, protocol::SplitMix32};

const LN2: f64 = std::f64::consts::LN_2;
const SOLITON_C: f64 = 0.1;
const SOLITON_DELTA: f64 = 0.5;
const REPAIR_DEGREE_MIN: u32 = 4;
const REPAIR_DEGREE_MAX: u32 = 24;

pub fn dlog(value: f64) -> f64 {
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
            r * dlog((r / SOLITON_DELTA).max(0.0)) / k_f64
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
        if block_len == 0 || block_len > usize::from(u16::MAX) || payload.len() > MAX_FILE_BYTES {
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

pub struct LtDecoder {
    k: u16,
    block_len: usize,
    session_id: u16,
    total_len: usize,
    words_per_block: usize,
    solved: Vec<Option<Vec<u32>>>,
    pending: Vec<Option<Equation>>,
    by_block: Vec<BTreeSet<usize>>,
    seen: HashSet<u32>,
    solved_count: usize,
    unique_frames: usize,
    redundant_frames: usize,
}

impl LtDecoder {
    pub fn new(
        k: u16,
        block_len: usize,
        session_id: u16,
        total_len: usize,
    ) -> Result<Self, OpticalError> {
        if k == 0
            || block_len == 0
            || block_len > usize::from(u16::MAX)
            || total_len == 0
            || total_len > MAX_FILE_BYTES
            || total_len > usize::from(k).saturating_mul(block_len)
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
            by_block: (0..k_usize).map(|_| BTreeSet::new()).collect(),
            seen: HashSet::new(),
            solved_count: 0,
            unique_frames: 0,
            redundant_frames: 0,
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
            return Ok(if self.is_complete() {
                AddFrame::Complete
            } else {
                AddFrame::Accepted
            });
        }

        let equation_id = self.pending.len();
        for index in &indices {
            self.by_block[usize::from(*index)].insert(equation_id);
        }
        self.pending.push(Some(Equation { indices, words }));
        Ok(AddFrame::Accepted)
    }

    pub fn solved_count(&self) -> usize {
        self.solved_count
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
                    let remaining = equation.indices[0];
                    self.by_block[usize::from(remaining)].remove(&equation_id);
                    queue.push((remaining, equation.words));
                }
            }
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
