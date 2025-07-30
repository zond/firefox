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

#[no_mangle]
pub unsafe extern "C" fn jxl_rust_decoder_new() -> *mut JxlRustDecoder {
    Box::into_raw(Box::new(JxlRustDecoder::new()))
}

#[no_mangle]
pub unsafe extern "C" fn jxl_rust_decoder_free(decoder: *mut JxlRustDecoder) {
    if !decoder.is_null() {
        let _ = Box::from_raw(decoder);
    }
}

#[no_mangle]
pub unsafe extern "C" fn jxl_rust_decoder_process_data(
    decoder: *mut JxlRustDecoder,
    data: *const u8,
    len: usize,
) -> JxlRustStatus {
    if decoder.is_null() || data.is_null() {
        return JxlRustStatus::Error;
    }

    let decoder = &mut *decoder;
    let data_slice = std::slice::from_raw_parts(data, len);

    match decoder.process_data(data_slice) {
        Ok(true) => JxlRustStatus::Ok,
        Ok(false) => JxlRustStatus::NeedMoreData,
        Err(_) => JxlRustStatus::InvalidData,
    }
}

#[no_mangle]
pub unsafe extern "C" fn jxl_rust_decoder_get_info(
    decoder: *const JxlRustDecoder,
    info: *mut JxlRustImageInfo,
) -> JxlRustStatus {
    if decoder.is_null() || info.is_null() {
        return JxlRustStatus::Error;
    }

    let decoder = &*decoder;

    if !decoder.header_validated {
        return JxlRustStatus::Error;
    }

    (*info).width = decoder.width;
    (*info).height = decoder.height;

    JxlRustStatus::Ok
}

#[no_mangle]
pub unsafe extern "C" fn jxl_rust_decoder_is_frame_ready(
    decoder: *const JxlRustDecoder,
) -> bool {
    if decoder.is_null() {
        return false;
    }

    let decoder = &*decoder;
    decoder.is_frame_ready()
}

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