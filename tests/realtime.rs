//! One isolated test binary: count allocations only after capture setup.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Instant,
};
use tempotrack::{
    audio::{BLOCK, CaptureWriter},
    rhythm::{PulseCursor, RhythmSnapshot},
};
struct CountAlloc;
static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for CountAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: CountAlloc = CountAlloc;
#[test]
fn capture_queue_and_projection_do_not_allocate_even_on_overflow() {
    let (mut writer, mut reader, _) = CaptureWriter::new(48000, 2, None, 4).unwrap();
    let now = Instant::now();
    let mut cursor = PulseCursor::default();
    let snapshot = RhythmSnapshot::empty(now, Default::default());
    let data = [0.25_f32; BLOCK * 2];
    let mut clock = tempotrack::clock::BeatClock::new(4);
    ENABLED.store(true, Ordering::SeqCst);
    for _ in 0..100 {
        for _ in 0..8 {
            writer.push(&data, now);
        }
        while reader.pop().is_ok() {}
        cursor.poll(&snapshot, now);
    }
    for frame in 0..3000 {
        let time = frame as f64 * 0.02;
        let grid = tempotrack::rhythm::PulseGrid {
            anchor: 0.,
            period: 0.48,
            provenance: tempotrack::rhythm::Provenance::Detected,
        };
        let raw = tempotrack::backend::Estimate {
            bpm: Some(125.),
            grids: [None, Some(grid), None],
            evidence: Some(tempotrack::backend::Evidence {
                time,
                beat: if frame % 24 == 0 { 0.95 } else { 0.01 },
                downbeat: 0.,
                event: frame % 24 == 0,
            }),
            ..Default::default()
        };
        clock.update(raw, time, Some(grid));
    }
    ENABLED.store(false, Ordering::SeqCst);
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
