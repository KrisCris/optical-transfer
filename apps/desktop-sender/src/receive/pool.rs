//! Fixed-slot decode threads.
//!
//! The native counterpart of the web receiver's `DecodePool`: one job in
//! flight per thread, and a full pool refuses work rather than queueing it.
//! A queued camera frame is a stale camera frame, and the fountain does not
//! care which frames it misses.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use crate::receive::{
    camera::LumaFrame,
    decoder::{Decoder, Reply},
    tracker::Crop,
};

/// One decode: an area of a shared frame. Crops borrow the frame rather than
/// copying it out, so a full scan and four crops of one capture share pixels.
pub struct Job {
    pub frame: Arc<LumaFrame>,
    pub area: Crop,
    pub full: bool,
    /// Session clock, milliseconds, of the capture this job came from.
    pub captured_at: f64,
}

pub struct Done {
    pub full: bool,
    pub captured_at: f64,
    pub reply: Reply,
}

struct Slot {
    busy: Arc<AtomicBool>,
    jobs: Option<mpsc::SyncSender<Job>>,
    thread: Option<thread::JoinHandle<()>>,
}

pub struct DecodePool {
    slots: Vec<Slot>,
}

impl DecodePool {
    /// `workers` threads, each reporting finished jobs through `done`.
    pub fn new(workers: usize, done: impl Fn(Done) + Send + Sync + 'static) -> Self {
        let done = Arc::new(done);
        let slots = (0..workers.max(1))
            .map(|index| {
                let busy = Arc::new(AtomicBool::new(false));
                let (jobs, inbox) = mpsc::sync_channel::<Job>(1);
                let flag = Arc::clone(&busy);
                let done = Arc::clone(&done);
                let thread = thread::Builder::new()
                    .name(format!("qr-decode-{index}"))
                    .spawn(move || {
                        let decoder = Decoder::new();
                        for job in inbox {
                            let reply = decoder.decode(&job.frame, job.area, job.full);
                            // Free before reporting, so the frame that the
                            // report unblocks can already use this slot.
                            flag.store(false, Ordering::Release);
                            done(Done {
                                full: job.full,
                                captured_at: job.captured_at,
                                reply,
                            });
                        }
                    })
                    .expect("a decode thread could not start");
                Slot {
                    busy,
                    jobs: Some(jobs),
                    thread: Some(thread),
                }
            })
            .collect();
        Self { slots }
    }

    pub fn size(&self) -> usize {
        self.slots.len()
    }

    pub fn free(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| !slot.busy.load(Ordering::Acquire))
            .count()
    }

    /// Hand a job to an idle thread. `false` when every thread is busy.
    pub fn submit(&self, job: Job) -> bool {
        for slot in &self.slots {
            if slot
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                if let Some(jobs) = &slot.jobs
                    && jobs.send(job).is_ok()
                {
                    return true;
                }
                slot.busy.store(false, Ordering::Release);
                return false;
            }
        }
        false
    }
}

impl Drop for DecodePool {
    fn drop(&mut self) {
        // Closing every inbox first lets all threads finish their current job
        // in parallel; then wait for them.
        for slot in &mut self.slots {
            slot.jobs = None;
        }
        for slot in &mut self.slots {
            if let Some(thread) = slot.thread.take() {
                let _ = thread.join();
            }
        }
    }
}
