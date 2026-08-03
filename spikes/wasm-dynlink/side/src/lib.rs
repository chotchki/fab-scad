//! The LIBRARY half. Its global allocator forwards to the host's, so both modules share ONE heap
//! over the shared linear memory — without this each links its own and they corrupt each other.
use std::alloc::{GlobalAlloc, Layout};

unsafe extern "C" {
    fn host_alloc(size: usize, align: usize) -> *mut u8;
    fn host_dealloc(ptr: *mut u8, size: usize, align: usize);
    fn host_double(x: i32) -> i32;
    fn host_sum(ptr: *const i32, len: usize) -> i32;
}

struct HostHeap;
unsafe impl GlobalAlloc for HostHeap {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 { unsafe { host_alloc(l.size(), l.align()) } }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) { unsafe { host_dealloc(p, l.size(), l.align()) } }
}
#[global_allocator]
static ALLOC: HostHeap = HostHeap;

/// Allocates a Vec on the SHARED heap, hands the pointer to the HOST to read, and calls a host
/// callback — the three crossings a real native makes.
#[unsafe(no_mangle)]
pub extern "C" fn side_roundtrip(n: i32) -> i32 {
    let v: Vec<i32> = (1..=n).collect();
    let summed = unsafe { host_sum(v.as_ptr(), v.len()) };
    unsafe { host_double(summed) }
}
