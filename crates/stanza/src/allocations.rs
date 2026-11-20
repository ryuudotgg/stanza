use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn counted() {
    let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
}

fn resized(by: isize) {
    let _ = LIVE.try_with(|live| {
        live.set(live.get() + by);
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(live.get())));
    });
}

pub fn count() -> usize {
    ALLOCATIONS.with(Cell::get)
}

pub fn peak_bytes(work: impl FnOnce()) -> usize {
    let start = LIVE.with(Cell::get);
    PEAK.with(|peak| peak.set(start));
    work();
    (PEAK.with(Cell::get) - start).unsigned_abs()
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        counted();
        resized(layout.size().cast_signed());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        counted();
        resized(layout.size().cast_signed());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        counted();
        resized(new_size.cast_signed());
        resized(-layout.size().cast_signed());
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        resized(-layout.size().cast_signed());
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;
