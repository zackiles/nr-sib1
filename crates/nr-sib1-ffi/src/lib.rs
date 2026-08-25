use std::ptr;

use nr_sib1::{Config, decode};
use num_complex::Complex32;

pub const OK: i32 = 0;
pub const INVALID_ARGUMENT: i32 = 1;
pub const INVALID_UTF8: i32 = 2;
pub const INVALID_CONFIG: i32 = 3;
pub const SERIALIZATION_FAILED: i32 = 4;
pub const PANICKED: i32 = 5;

fn run(iq: &[f32], config: &[u8]) -> Result<Vec<u8>, i32> {
    if !iq.len().is_multiple_of(2) {
        return Err(INVALID_ARGUMENT);
    }
    let config = std::str::from_utf8(config).map_err(|_| INVALID_UTF8)?;
    let config: Config = serde_json::from_str(config).map_err(|_| INVALID_CONFIG)?;
    let samples = iq
        .as_chunks::<2>()
        .0
        .iter()
        .map(|sample| Complex32::new(sample[0], sample[1]))
        .collect::<Vec<_>>();
    serde_json::to_vec(&decode(&samples, &config)).map_err(|_| SERIALIZATION_FAILED)
}

/// Decodes interleaved `f32` IQ and allocates a JSON array of `Event` values.
///
/// `iq_len` counts floats, not complex samples. On success, the caller owns `*output` and must call
/// `nr_sib1_free(*output, *output_len)`. All non-null pointers must address readable or writable
/// ranges of the supplied lengths.
///
/// # Safety
///
/// The caller must uphold the pointer validity requirements above.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nr_sib1_decode(
    iq: *const f32,
    iq_len: usize,
    config: *const u8,
    config_len: usize,
    output: *mut *mut u8,
    output_len: *mut usize,
) -> i32 {
    if output.is_null() || output_len.is_null() {
        return INVALID_ARGUMENT;
    }
    unsafe {
        output.write(ptr::null_mut());
        output_len.write(0);
    }
    if (iq.is_null() && iq_len != 0) || config.is_null() || config_len == 0 {
        return INVALID_ARGUMENT;
    }
    let result = std::panic::catch_unwind(|| {
        let iq = if iq_len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(iq, iq_len) }
        };
        let config = unsafe { std::slice::from_raw_parts(config, config_len) };
        run(iq, config)
    });
    let bytes = match result {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(status)) => return status,
        Err(_) => return PANICKED,
    };
    let mut bytes = bytes.into_boxed_slice();
    unsafe {
        output_len.write(bytes.len());
        output.write(bytes.as_mut_ptr());
    }
    std::mem::forget(bytes);
    OK
}

/// Releases a JSON buffer returned by `nr_sib1_decode`.
///
/// # Safety
///
/// `data` and `len` must be an unchanged pair returned by `nr_sib1_decode`, or null and zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nr_sib1_free(data: *mut u8, len: usize) {
    if data.is_null() {
        return;
    }
    let raw = ptr::slice_from_raw_parts_mut(data, len);
    drop(unsafe { Box::from_raw(raw) });
}

#[cfg(test)]
mod tests {
    use super::{INVALID_ARGUMENT, nr_sib1_decode};

    #[test]
    fn rejects_missing_output_pointers() {
        let status = unsafe {
            nr_sib1_decode(
                std::ptr::null(),
                0,
                b"{}".as_ptr(),
                2,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, INVALID_ARGUMENT);
    }
}
