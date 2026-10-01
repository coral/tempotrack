extern crate block;

use block::ConcreteBlock;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[test]
fn native_heap_copy_preserves_captured_arguments_and_lifetime() {
    let prefix = String::from("tempo");
    let block = ConcreteBlock::new(move |value: usize| prefix.len() + value);
    assert_eq!(unsafe { block.call((7,)) }, 12);
    let copied = block.copy();
    assert_eq!(unsafe { copied.call((8,)) }, 13);
    let retained = copied.clone();
    drop(copied);
    assert_eq!(unsafe { retained.call((9,)) }, 14);
}

#[test]
fn native_release_disposes_capture_exactly_once() {
    struct Capture(Arc<AtomicUsize>);
    impl Drop for Capture {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let capture = Capture(drops.clone());
    let heap = ConcreteBlock::new(move || capture.0.load(Ordering::SeqCst)).copy();
    let retained = heap.clone();
    assert_eq!(unsafe { heap.call(()) }, 0);
    drop(heap);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(retained);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
