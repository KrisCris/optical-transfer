//! Wire v4's fountain code: RaptorQ (RFC 6330).
//!
//! Wire v3's LT code with a Gaussian-elimination finish needed 1.0 to 1.6
//! received frames per source block at 1 MiB in the loopback benchmark, and
//! 1.3 to 2.0 at 5 to 16 MiB, because its elimination only runs once fewer
//! than a thousand blocks are unsolved. RaptorQ recovers a source block from
//! any `K` of its symbols with failure odds of about 1 in 100, and from `K + 2`
//! with odds of about one in a million, whatever the loss pattern. Measured
//! with this crate's encoder and decoder over the same carousels (systematic
//! symbols, then repair symbols, with random loss at 5 to 70%): every run
//! decoded from exactly `K` symbols, and decoding a 16 MiB block at 50% loss
//! took about 60 ms natively.
//!
//! The coding itself is the `raptorq` crate's. What this module owns is the
//! part of the wire format the RFC leaves to the application:
//!
//! * **The block layout is derived, not sent.** [`Layout`] splits a payload
//!   into source blocks by a fixed rule from `(total_len, symbol_len)` alone,
//!   so a frame needs no transmission information beyond its header.
//! * **Which symbol each frame carries.** Frame `n` carries block `n mod Z`'s
//!   symbol `n div Z`: one systematic sweep of the whole payload with the
//!   blocks interleaved, then repair symbols forever. Interleaving spreads a
//!   burst of missed frames across blocks instead of starving one.
//! * **Bounds on what a hostile stream can make a receiver hold.** Decode
//!   attempts back off exponentially after a failure, and a block that has
//!   collected twice its size without decoding is given up on.

use std::collections::HashSet;

use raptorq::{
    EncodingPacket, ObjectTransmissionInformation, PayloadId, SourceBlockDecoder,
    SourceBlockEncoder, partition,
};

use crate::{
    OpticalError, container::MAX_CONTAINER_BYTES, fountain::AddFrame, protocol::MAX_SYMBOL_ID,
};

/// The most source symbols one RaptorQ block may hold.
///
/// Decoding takes a little more than linear time in a block's size: natively,
/// at 50% loss, about 45 ms for 4096 symbols of 2937 bytes and 210 ms for
/// 16 384, best of three on an otherwise busy machine. It also needs a working
/// copy of the block. Capping a block at 46 MiB bounds that copy and the pause
/// on the receiving thread; the RFC itself allows 56 403 symbols.
pub const MAX_BLOCK_SYMBOLS: u32 = 16_384;

/// The most source symbols one stream may have in total.
///
/// An honest sender never comes close: at the smallest offered frame size the
/// largest file is about 280 000 symbols. A header claiming millions of
/// one-byte symbols is a request for tens of megabytes of bookkeeping, and is
/// refused before any of it is allocated.
pub const MAX_SYMBOLS: u32 = 1 << 19;

/// How a payload splits into RaptorQ source blocks, as RFC 6330 §4.4.1.2
/// partitions it: `Z = ceil(Kt / MAX_BLOCK_SYMBOLS)` blocks whose sizes differ
/// by at most one symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    total_len: usize,
    symbol_len: usize,
    symbols: u32,
    blocks: u32,
    long_symbols: u32,
    short_symbols: u32,
    long_blocks: u32,
}

impl Layout {
    /// The layout both ends derive for these dimensions. Every argument may
    /// have come off the optical channel, so each is bounded before use.
    pub fn new(total_len: usize, symbol_len: usize) -> Result<Self, OpticalError> {
        if total_len == 0
            || total_len > MAX_CONTAINER_BYTES
            || symbol_len == 0
            || symbol_len > usize::from(u16::MAX)
        {
            return Err(OpticalError::UnsupportedDimensions);
        }
        let symbols = total_len.div_ceil(symbol_len);
        if symbols > MAX_SYMBOLS as usize {
            return Err(OpticalError::UnsupportedDimensions);
        }
        let symbols = symbols as u32;
        let blocks = symbols.div_ceil(MAX_BLOCK_SYMBOLS);
        let (long_symbols, short_symbols, long_blocks, _) = partition(symbols, blocks);
        Ok(Self {
            total_len,
            symbol_len,
            symbols,
            blocks,
            long_symbols,
            short_symbols,
            long_blocks,
        })
    }

    pub fn total_len(&self) -> usize {
        self.total_len
    }

    pub fn symbol_len(&self) -> usize {
        self.symbol_len
    }

    /// Source symbols across every block: what a receiver must collect.
    pub fn symbols(&self) -> u32 {
        self.symbols
    }

    pub fn blocks(&self) -> u32 {
        self.blocks
    }

    /// Source symbols in `block`.
    pub fn block_symbols(&self, block: u32) -> u32 {
        if block < self.long_blocks {
            self.long_symbols
        } else {
            self.short_symbols
        }
    }

    /// The payload-wide index of `block`'s first source symbol.
    pub fn block_start(&self, block: u32) -> u32 {
        let long = block.min(self.long_blocks);
        long * self.long_symbols + (block - long) * self.short_symbols
    }

    /// The block holding payload-wide source symbol `index`, and the
    /// symbol's id within it.
    pub fn locate(&self, index: u32) -> (u32, u32) {
        let long_span = self.long_blocks * self.long_symbols;
        if index < long_span {
            (index / self.long_symbols, index % self.long_symbols)
        } else {
            let rest = index - long_span;
            (
                self.long_blocks + rest / self.short_symbols,
                rest % self.short_symbols,
            )
        }
    }

    /// The block and symbol id frame `seq` carries.
    pub fn position(&self, seq: u32) -> (u8, u32) {
        (
            (seq % self.blocks) as u8,
            (seq / self.blocks) % (MAX_SYMBOL_ID + 1),
        )
    }

    /// The frame number that carries `symbol` of `block`, modulo the symbol
    /// id space. Receivers use it to tell how much of the stream they saw.
    pub fn sequence(&self, block: u8, symbol: u32) -> u64 {
        u64::from(symbol) * u64::from(self.blocks) + u64::from(block)
    }

    fn config(&self) -> ObjectTransmissionInformation {
        ObjectTransmissionInformation::new(
            self.total_len as u64,
            self.symbol_len as u16,
            self.blocks as u8,
            1,
            1,
        )
    }

    fn block_bytes(&self, block: u32) -> usize {
        self.block_symbols(block) as usize * self.symbol_len
    }
}

/// The sending side: every symbol of every block, on demand.
pub struct RaptorEncoder {
    layout: Layout,
    /// The payload, zero-padded to whole symbols. Source symbols are slices
    /// of it; the crate's encoders keep only what repair symbols need.
    padded: Vec<u8>,
    blocks: Vec<SourceBlockEncoder>,
}

impl RaptorEncoder {
    pub fn new(payload: &[u8], symbol_len: usize) -> Result<Self, OpticalError> {
        let layout = Layout::new(payload.len(), symbol_len)?;
        let mut padded = payload.to_vec();
        padded.resize(layout.symbols as usize * symbol_len, 0);
        let encoder = raptorq::Encoder::new(&padded, layout.config());
        Ok(Self {
            layout,
            padded,
            blocks: encoder.get_block_encoders().clone(),
        })
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Symbol `id` of `block`: a slice of the payload below the block's size,
    /// a repair symbol at or above it.
    pub fn symbol(&self, block: u8, id: u32) -> Vec<u8> {
        let block_index = u32::from(block);
        let size = self.layout.block_symbols(block_index);
        if id < size {
            let start =
                (self.layout.block_start(block_index) + id) as usize * self.layout.symbol_len;
            return self.padded[start..start + self.layout.symbol_len].to_vec();
        }
        self.blocks[usize::from(block)]
            .repair_packets(id - size, 1)
            .pop()
            .expect("one repair packet was asked for")
            .split()
            .1
    }

    /// Frame `seq`'s block, symbol id and symbol.
    pub fn encode(&self, seq: u32) -> (u8, u32, Vec<u8>) {
        let (block, id) = self.layout.position(seq);
        (block, id, self.symbol(block, id))
    }
}

/// One source block's receive state.
struct Block {
    size: u32,
    /// Created on the block's first symbol, dropped once it decodes.
    decoder: Option<SourceBlockDecoder>,
    /// Symbols collected since the last decode attempt.
    held: Vec<EncodingPacket>,
    seen: HashSet<u32>,
    received: u32,
    /// Source symbols that arrived as themselves, for the block map.
    systematic: Vec<bool>,
    next_attempt: u32,
    failures: u32,
    decoded: Option<Vec<u8>>,
}

pub struct RaptorDecoder {
    layout: Layout,
    config: ObjectTransmissionInformation,
    blocks: Vec<Block>,
    unique_frames: usize,
    redundant_frames: usize,
    attempts: usize,
    complete_blocks: u32,
}

impl RaptorDecoder {
    pub fn new(total_len: usize, symbol_len: usize) -> Result<Self, OpticalError> {
        let layout = Layout::new(total_len, symbol_len)?;
        let blocks = (0..layout.blocks)
            .map(|block| {
                let size = layout.block_symbols(block);
                Block {
                    size,
                    decoder: None,
                    held: Vec::new(),
                    seen: HashSet::new(),
                    received: 0,
                    systematic: Vec::new(),
                    next_attempt: size,
                    failures: 0,
                    decoded: None,
                }
            })
            .collect();
        Ok(Self {
            layout,
            config: layout.config(),
            blocks,
            unique_frames: 0,
            redundant_frames: 0,
            attempts: 0,
            complete_blocks: 0,
        })
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn add_symbol(
        &mut self,
        block: u8,
        id: u32,
        symbol: &[u8],
    ) -> Result<AddFrame, OpticalError> {
        if symbol.len() != self.layout.symbol_len || u32::from(block) >= self.layout.blocks {
            return Err(OpticalError::InvalidFrame);
        }
        let state = &mut self.blocks[usize::from(block)];
        if !state.seen.insert(id) {
            return Ok(AddFrame::Duplicate);
        }
        self.unique_frames += 1;
        if self.complete_blocks == self.layout.blocks {
            return Ok(AddFrame::Complete);
        }
        if state.decoded.is_some() {
            self.redundant_frames += 1;
            return Ok(AddFrame::Redundant);
        }

        state.received += 1;
        if id < state.size {
            if state.systematic.is_empty() {
                state.systematic = vec![false; state.size as usize];
            }
            state.systematic[id as usize] = true;
        }
        state.held.push(EncodingPacket::new(
            PayloadId::new(block, id),
            symbol.to_vec(),
        ));
        if state.received >= state.next_attempt {
            self.attempts += 1;
            let config = &self.config;
            let decoder = state.decoder.get_or_insert_with(|| {
                SourceBlockDecoder::new(
                    block,
                    config,
                    self.layout.block_bytes(u32::from(block)) as u64,
                )
            });
            match decoder.decode(state.held.drain(..)) {
                Some(bytes) => {
                    state.decoded = Some(bytes);
                    state.decoder = None;
                    state.systematic = Vec::new();
                    self.complete_blocks += 1;
                }
                None => {
                    // Each received symbol raises the odds by about two orders
                    // of magnitude, so a failure is almost always cured by the
                    // next one or two. Doubling the wait keeps a stream built
                    // to defeat the decoder from buying a full attempt with
                    // every frame.
                    state.failures += 1;
                    state.next_attempt = state.received + (1 << state.failures.min(16)) - 1;
                }
            }
        }
        Ok(if self.complete_blocks == self.layout.blocks {
            AddFrame::Complete
        } else {
            AddFrame::Accepted
        })
    }

    pub fn is_complete(&self) -> bool {
        self.complete_blocks == self.layout.blocks
    }

    /// A block has collected twice its size and still not decoded. An honest
    /// stream does not get here; the receiver should stop holding its symbols.
    pub fn is_exhausted(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| block.decoded.is_none() && block.received > 2 * block.size + 64)
    }

    /// Source symbols across every block.
    pub fn source_symbols(&self) -> u32 {
        self.layout.symbols
    }

    /// Progress towards completion, in source symbols: a block counts the
    /// symbols it has collected, up to its size, since any that many decode it.
    pub fn solved_count(&self) -> usize {
        self.blocks
            .iter()
            .map(|block| {
                if block.decoded.is_some() {
                    block.size as usize
                } else {
                    block.received.min(block.size) as usize
                }
            })
            .sum()
    }

    /// Whether payload-wide source symbol `index` is known: its block has
    /// decoded, or it arrived as itself.
    pub fn is_solved(&self, index: usize) -> bool {
        let Ok(index) = u32::try_from(index) else {
            return false;
        };
        if index >= self.layout.symbols {
            return false;
        }
        let (block, local) = self.layout.locate(index);
        let state = &self.blocks[block as usize];
        state.decoded.is_some()
            || state
                .systematic
                .get(local as usize)
                .copied()
                .unwrap_or(false)
    }

    /// One byte per source symbol, 1 where [`Self::is_solved`] holds.
    pub fn solved_map(&self) -> Vec<u8> {
        let mut map = Vec::with_capacity(self.layout.symbols as usize);
        for state in &self.blocks {
            if state.decoded.is_some() {
                map.resize(map.len() + state.size as usize, 1);
            } else if state.systematic.is_empty() {
                map.resize(map.len() + state.size as usize, 0);
            } else {
                map.extend(state.systematic.iter().map(|known| u8::from(*known)));
            }
        }
        map
    }

    /// Repair symbols held for blocks that have not decoded yet.
    pub fn pending_symbols(&self) -> usize {
        self.blocks
            .iter()
            .filter(|block| block.decoded.is_none())
            .map(|block| {
                let systematic = block.systematic.iter().filter(|known| **known).count();
                block.received as usize - systematic
            })
            .sum()
    }

    /// Decode attempts run so far, successful or not.
    pub fn attempts(&self) -> usize {
        self.attempts
    }

    pub fn unique_frames(&self) -> usize {
        self.unique_frames
    }

    pub fn redundant_frames(&self) -> usize {
        self.redundant_frames
    }

    /// The payload, once every block has decoded. Moves the decoded blocks
    /// out rather than copying them: a large payload is already the biggest
    /// allocation the receiver holds.
    pub fn take_payload(&mut self) -> Result<Vec<u8>, OpticalError> {
        if !self.is_complete() {
            return Err(OpticalError::IncompleteTransfer);
        }
        let mut payload = Vec::with_capacity(self.layout.symbols as usize * self.layout.symbol_len);
        for block in &mut self.blocks {
            payload.extend_from_slice(
                &block
                    .decoded
                    .take()
                    .ok_or(OpticalError::IncompleteTransfer)?,
            );
        }
        payload.truncate(self.layout.total_len);
        Ok(payload)
    }
}
