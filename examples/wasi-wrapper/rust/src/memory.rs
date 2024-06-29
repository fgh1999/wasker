//! Linear Memory
use std::sync::Mutex;

const LINEAR_MEMORY_BLOCK_SIZE: i32 = 64 * 1024;    // 64KiB

static LINEAR_MEMORY_BASE: Mutex<usize> = Mutex::new(0);
static LINEAR_MEMORY_BLOCK_NUM: Mutex<i32> = Mutex::new(0);

#[inline]
pub unsafe fn get_memory_base() -> *mut u8 {
    *LINEAR_MEMORY_BASE.lock().unwrap() as *mut u8
}

unsafe fn alloc_memory(block_num: usize) -> *mut u8 {
    use std::alloc::{alloc, Layout};
    alloc(
        Layout::from_size_align(LINEAR_MEMORY_BLOCK_SIZE as usize * block_num, 8)
            .unwrap(),
    )
}

#[no_mangle]
pub extern "C" fn memory_base() -> i32 {
    unsafe { get_memory_base() as i32 }
}

#[no_mangle]
pub extern "C" fn memory_grow(block_num: i32) -> i32 {
    assert!(
        block_num >= 0,
        "block_num must be greater than or equal to 0"
    );
    let mut num = LINEAR_MEMORY_BLOCK_NUM.lock().unwrap();
    let old_val = *num;
    let wanted_block_num = *num + block_num;
    let new_base_addr = unsafe { alloc_memory(wanted_block_num as usize) };
    let mut mem_base = LINEAR_MEMORY_BASE.lock().unwrap();
    let old_base_addr = *mem_base;

    // move previous data to the newly allocated memory
    if old_val > 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(
                old_base_addr as *const u8,
                new_base_addr,
                (LINEAR_MEMORY_BLOCK_SIZE * old_val) as usize,
            );
        }
    }
    *mem_base = new_base_addr as usize;
    *num = wanted_block_num;

    // deallocte the previous memory
    if old_val > 0 {
        use std::alloc::{dealloc, Layout};
        unsafe {
            dealloc(
                old_base_addr as *mut u8,
                Layout::from_size_align((LINEAR_MEMORY_BLOCK_SIZE * old_val) as usize, 8).unwrap(),
            );
        }
    }
    old_val
}
