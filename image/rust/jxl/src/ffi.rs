/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use crate::decoder::JxlApiDecoder;
use std::slice;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JxlDecoderStatus {
    Ok = 0,
    NeedMoreData = 1,
    Error = 2,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JxlBasicInfo {
    pub width: u32,
    pub height: u32,
    pub has_alpha: bool,
    pub alpha_premultiplied: bool,
    pub valid: bool,
}

impl JxlBasicInfo {
    fn invalid() -> Self {
        Self {
            width: 0,
            height: 0,
            has_alpha: false,
            alpha_premultiplied: false,
            valid: false,
        }
    }
}

pub struct JxlDecoderImpl {
    inner: JxlApiDecoder,
}

#[no_mangle]
pub extern "C" fn jxl_decoder_new(metadata_only: bool) -> *mut JxlDecoderImpl {
    let decoder = Box::new(JxlDecoderImpl {
        inner: JxlApiDecoder::new(metadata_only),
    });
    Box::into_raw(decoder)
}

#[no_mangle]
pub unsafe extern "C" fn jxl_decoder_destroy(decoder: *mut JxlDecoderImpl) {
    if !decoder.is_null() {
        let _ = Box::from_raw(decoder);
    }
}

#[no_mangle]
pub unsafe extern "C" fn jxl_decoder_process_data(
    decoder: *mut JxlDecoderImpl,
    data: *mut *const u8,
    data_len: *mut usize,
) -> JxlDecoderStatus {
    assert!(!decoder.is_null() && !data.is_null() && !data_len.is_null());

    let decoder = &mut *decoder;
    let mut data_slice = slice::from_raw_parts(*data, *data_len);

    let result = decoder.inner.process_data(&mut data_slice);

    *data = data_slice.as_ptr();
    *data_len = data_slice.len();

    match result {
        Ok(true) => JxlDecoderStatus::Ok,
        Ok(false) => JxlDecoderStatus::NeedMoreData,
        Err(_) => JxlDecoderStatus::Error,
    }
}

#[no_mangle]
pub unsafe extern "C" fn jxl_decoder_get_basic_info(
    decoder: *const JxlDecoderImpl,
) -> JxlBasicInfo {
    assert!(!decoder.is_null());
    let decoder = &*decoder;

    let Some(info) = decoder.inner.get_basic_info() else {
        return JxlBasicInfo::invalid();
    };

    JxlBasicInfo {
        width: info.width,
        height: info.height,
        has_alpha: info.has_alpha,
        alpha_premultiplied: info.alpha_premultiplied,
        valid: true,
    }
}

#[no_mangle]
pub unsafe extern "C" fn jxl_decoder_is_frame_ready(decoder: *const JxlDecoderImpl) -> bool {
    assert!(!decoder.is_null());
    let decoder = &*decoder;
    decoder.inner.frame_ready
}

#[no_mangle]
pub unsafe extern "C" fn jxl_decoder_decode_frame(
    decoder: *mut JxlDecoderImpl,
    output_data: *mut u32,
    output_len: usize,
    pixels_written: *mut usize,
) -> JxlDecoderStatus {
    assert!(!decoder.is_null() && !output_data.is_null() && !pixels_written.is_null());

    let decoder = &mut *decoder;
    let output_slice = slice::from_raw_parts_mut(output_data, output_len);

    match decoder.inner.decode_frame(output_slice) {
        Ok(count) => {
            *pixels_written = count;
            JxlDecoderStatus::Ok
        }
        Err(_) => JxlDecoderStatus::Error,
    }
}
