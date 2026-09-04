//! Invariants raised by the core milestone review that the upstream oracle
//! does not itself enforce.
//!
//! Every assertion here is a hardening decision, not a wire-format vector: the
//! JavaScript baseline runs in a browser tab a user closes, while this crate
//! also runs in a long-lived desktop binary where an adversarial or merely
//! noisy stream gets far more time to work with.

use optical_core::{
    OpticalError,
    fountain::{LtDecoder, dlog, soliton_cdf},
    protocol::fnv1a,
};

#[test]
fn dlog_terminates_outside_its_reachable_domain() {
    // `dlog` is public, and its range reduction is two unbounded `while` loops:
    // zero never reaches 0.75 by doubling and infinity never falls under 1.5 by
    // halving, so an out-of-domain caller hangs the process rather than getting
    // an answer. Match `f64::ln` at those points instead.
    assert_eq!(dlog(0.0), f64::NEG_INFINITY);
    assert_eq!(dlog(-0.0), f64::NEG_INFINITY);
    assert!(dlog(-1.0).is_nan());
    assert!(dlog(f64::NAN).is_nan());
    assert_eq!(dlog(f64::INFINITY), f64::INFINITY);
    assert!(dlog(f64::NEG_INFINITY).is_nan());
}

#[test]
fn dlog_guard_does_not_disturb_the_pinned_domain() {
    // The guard must be unreachable for every input `soliton_cdf` produces, so
    // the exhaustive upstream digest still has to hold byte for byte.
    let mut bytes = Vec::new();
    for k in 1_u16..=65535 {
        bytes.extend_from_slice(&dlog(f64::from(u32::from(k) * 2)).to_le_bytes());
    }
    for i in 64..(64 * 4096) {
        bytes.extend_from_slice(&dlog(f64::from(i) / 64.0).to_le_bytes());
    }
    assert_eq!(fnv1a(&bytes), 0x27b0_f3cc);
    assert_eq!(
        fnv1a(
            &soliton_cdf(179)
                .into_iter()
                .flat_map(f64::to_le_bytes)
                .collect::<Vec<_>>()
        ),
        0xe8b6_340a
    );
}

#[test]
fn decoder_requires_k_to_be_the_minimal_block_count() {
    // `k` and `block_len` are attacker-controlled u16s off the wire, and the
    // decoder allocates one `block_len` buffer per solved block. Bounding only
    // `total_len` at 64 MiB leaves k*block_len free to reach ~4 GiB of solved
    // storage for a payload that claims to be tiny. An honest sender always
    // sends k = ceil(total_len / block_len), so anything looser is a lie about
    // capacity and is rejected before a single block is allocated.
    assert!(LtDecoder::new(10, 1445, 1, 1445 * 10).is_ok());
    assert!(LtDecoder::new(10, 1445, 1, 1445 * 9 + 1).is_ok());
    assert_eq!(
        LtDecoder::new(10, 1445, 1, 1445 * 9).err(),
        Some(OpticalError::UnsupportedDimensions)
    );
    assert_eq!(
        LtDecoder::new(u16::MAX, usize::from(u16::MAX), 1, 1024).err(),
        Some(OpticalError::UnsupportedDimensions)
    );
    assert!(LtDecoder::new(1, 1445, 1, 1).is_ok());
}
