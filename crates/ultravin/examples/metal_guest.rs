//! Byte-for-byte full decoder used by the Metal experiment and its native control.
//! Each host thread or GPU instance owns its input and output until the next call.

use std::cell::RefCell;

#[derive(Default)]
struct Buffers {
    input: Vec<u8>,
    output: String,
}

thread_local! {
    static BUFFERS: RefCell<Buffers> = RefCell::default();
}

/// Reserve UTF-8 input bytes to be filled by the host before calling `decode_vin`.
#[no_mangle]
pub extern "C" fn reserve_input(bytes: u32) -> *mut u8 {
    BUFFERS.with_borrow_mut(|buffers| {
        buffers.input.resize(bytes as usize, 0);
        buffers.output.clear();
        buffers.input.as_mut_ptr()
    })
}

/// Decode one input with the ordinary full Rust API and return the JSON byte count.
#[no_mangle]
pub extern "C" fn decode_vin(bytes: u32, has_year: u32, year: i32, now_micros: i64) -> u32 {
    BUFFERS.with_borrow_mut(|buffers| {
        let vin =
            std::str::from_utf8(&buffers.input[..bytes as usize]).expect("host supplies UTF-8");
        buffers.output = ultravin::decode_json_at(vin, (has_year != 0).then_some(year), now_micros);
        u32::try_from(buffers.output.len()).expect("one result fits in linear memory")
    })
}

/// Address of the JSON from the last decode on this instance.
#[no_mangle]
pub extern "C" fn output_ptr() -> *const u8 {
    BUFFERS.with_borrow(|buffers| buffers.output.as_ptr())
}

/// Native control uses ultravin's normal parallel, full-JSON batch entry point.
#[cfg(not(target_arch = "wasm32"))]
#[no_mangle]
pub extern "C" fn decode_native_batch(now_micros: i64) -> u32 {
    BUFFERS.with_borrow_mut(|buffers| {
        let rows: Vec<(String, Option<i32>)> =
            serde_json::from_slice(&buffers.input).expect("host supplies VIN/year pairs");
        let (vins, years): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
        buffers.output = ultravin::decode_batch_json_at(&vins, Some(&years), now_micros);
        u32::try_from(buffers.output.len()).expect("benchmark chunk fits in four GiB")
    })
}

/// The shader reads both words of this WASM i64 after the guest returns.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn decode_row(bytes: u32, has_year: u32, year: i32, now_micros: i64) -> u64 {
    let count = decode_vin(bytes, has_year, year, now_micros);
    ((output_ptr() as u64) << 32) | u64::from(count)
}
