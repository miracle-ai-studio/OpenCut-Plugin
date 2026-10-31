#![no_std]
// One instance is private to a JavaScript realm. The caller resets and finishes
// synchronously with no await, callback or imported function between them.
// The only external byte count is checked before constructing a slice.
const CAPACITY: usize = 64 * 1024;
static mut INPUT: [u8; CAPACITY] = [0; CAPACITY];
static mut OUTPUT: [u8; 32] = [0; 32];
static mut STATE: Option<blake3::Hasher> = None;
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    core::arch::wasm32::unreachable()
}
#[unsafe(no_mangle)]
pub extern "C" fn input_ptr() -> *mut u8 {
    (&raw mut INPUT).cast()
}
#[unsafe(no_mangle)]
pub extern "C" fn reset() {
    unsafe {
        STATE = Some(blake3::Hasher::new());
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn update(length: usize) {
    if length > CAPACITY {
        core::arch::wasm32::unreachable();
    }
    unsafe {
        (&mut *(&raw mut STATE))
            .as_mut()
            .unwrap()
            .update(&(&*(&raw const INPUT))[..length]);
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn finish() -> *const u8 {
    unsafe {
        OUTPUT = *(&*(&raw const STATE))
            .as_ref()
            .unwrap()
            .finalize()
            .as_bytes();
    }
    (&raw const OUTPUT).cast()
}
