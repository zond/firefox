// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use jxl_rust_decoder::JxlRustDecoder;

#[repr(C)]
pub enum JxlRustStatus {
    Ok,
    NeedMoreData,
    InvalidData,
    Error,
}

#[repr(C)]
pub struct JxlRustImageInfo {
    pub width: u32,
    pub height: u32,
}

/// Create a new JXL decoder instance.
///
/// # Safety
/// The returned pointer must be freed with `jxl_rust_decoder_free`.
#[no_mangle]
pub unsafe extern "C" fn jxl_rust_decoder_new(metadata_only: bool) -> *mut JxlRustDecoder {
    let decoder = Box::new(JxlRustDecoder::new(metadata_only));
    Box::into_raw(decoder)
}

/// Free a JXL decoder instance.
///
/// # Safety
/// The decoder pointer must be valid and created by `jxl_rust_decoder_new`.
/// After calling this function, the pointer is invalid and must not be used.
#[no_mangle]
pub unsafe extern "C" fn jxl_rust_decoder_free(decoder: *mut JxlRustDecoder) {
    if !decoder.is_null() {
        let _ = Box::from_raw(decoder);
    }
}

/// Process JXL data through the decoder.
///
/// # Safety
/// - The decoder pointer must be valid and created by `jxl_rust_decoder_new`.
/// - The data pointer must be valid for `len` bytes.
/// - The data must remain valid for the duration of this call.
#[no_mangle]
pub unsafe extern "C" fn jxl_rust_decoder_process_data(
    decoder: *mut JxlRustDecoder,
    data: *const u8,
    len: usize,
    size_hint_out: *mut usize,
) -> JxlRustStatus {
    if decoder.is_null() || data.is_null() {
        return JxlRustStatus::Error;
    }

    let decoder = &mut *decoder;
    let data_slice = std::slice::from_raw_parts(data, len);

    match decoder.process_data(data_slice) {
        Ok((true, _)) => JxlRustStatus::Ok,
        Ok((false, size_hint)) => {
            if !size_hint_out.is_null() {
                *size_hint_out = size_hint;
            }
            JxlRustStatus::NeedMoreData
        }
        Err(_) => JxlRustStatus::InvalidData,
    }
}

/// Get image information from the decoder.
///
/// # Safety
/// - The decoder pointer must be valid and created by `jxl_rust_decoder_new`.
/// - The info pointer must be valid and point to writable memory.
#[no_mangle]
pub unsafe extern "C" fn jxl_rust_decoder_get_info(
    decoder: *const JxlRustDecoder,
    info: *mut JxlRustImageInfo,
) -> JxlRustStatus {
    if decoder.is_null() || info.is_null() {
        return JxlRustStatus::Error;
    }

    let decoder = &*decoder;

    if let Some(cached_info) = &decoder.cached_info {
        (*info).width = cached_info.width;
        (*info).height = cached_info.height;
    } else {
        return JxlRustStatus::Error;
    }

    JxlRustStatus::Ok
}

/// Check if a frame is ready for decoding.
///
/// # Safety
/// The decoder pointer must be valid and created by `jxl_rust_decoder_new`.
#[no_mangle]
pub unsafe extern "C" fn jxl_rust_decoder_is_frame_ready(decoder: *const JxlRustDecoder) -> bool {
    if decoder.is_null() {
        return false;
    }

    let decoder = &*decoder;
    decoder.is_frame_ready()
}

/// Decode a frame from the JXL data.
///
/// # Safety
/// - The decoder pointer must be valid and created by `jxl_rust_decoder_new`.
/// - The output_data pointer must be valid for `output_len` u32 values.
/// - The pixels_written pointer must be valid and point to writable memory.
#[no_mangle]
pub unsafe extern "C" fn jxl_rust_decoder_decode_frame(
    decoder: *mut JxlRustDecoder,
    output_data: *mut u32,
    output_len: usize,
    pixels_written: *mut usize,
) -> JxlRustStatus {
    if decoder.is_null() || output_data.is_null() || pixels_written.is_null() {
        return JxlRustStatus::Error;
    }

    let decoder = &mut *decoder;
    let output_slice = std::slice::from_raw_parts_mut(output_data, output_len);

    match decoder.decode_frame(output_slice) {
        Ok(count) => {
            *pixels_written = count;
            JxlRustStatus::Ok
        }
        Err(_) => JxlRustStatus::Error,
    }
}
