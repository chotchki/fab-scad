//! The HOST half: owns linear memory and the heap, and exports allocation so a side module can
//! share them instead of running a second allocator over the same bytes.
use std::alloc::{GlobalAlloc, Layout, System};

#[unsafe(no_mangle)]
pub extern "C" fn host_alloc(size: usize, align: usize) -> *mut u8 {
    match Layout::from_size_align(size, align) {
        Ok(l) => unsafe { System.alloc(l) },
        Err(_) => core::ptr::null_mut(),
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn host_dealloc(ptr: *mut u8, size: usize, align: usize) {
    if let Ok(l) = Layout::from_size_align(size, align) {
        unsafe { System.dealloc(ptr, l) }
    }
}
/// A host callback the side module invokes — stands in for `FnCtx` re-entering the evaluator.
#[unsafe(no_mangle)]
pub extern "C" fn host_double(x: i32) -> i32 { x * 2 }

/// Round-trips a heap value ALLOCATED BY THE SIDE MODULE, to prove one heap.
#[unsafe(no_mangle)]
pub extern "C" fn host_sum(ptr: *const i32, len: usize) -> i32 {
    unsafe { std::slice::from_raw_parts(ptr, len) }.iter().sum()
}
