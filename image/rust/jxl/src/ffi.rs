/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use crate::decoder::JxlApiDecoder;
use std::ptr;
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
    pub valid: bool,
}

impl JxlBasicInfo {
    fn invalid() -> Self {
        Self {
            width: 0,
            height: 0,
            valid: false,
        }
    }
}

pub struct JxlDecoder {
    inner: JxlApiDecoder,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn jxl_decoder_new(metadata_only: bool) -> *mut JxlDecoder {
    let decoder = Box::new(JxlDecoder {
        inner: JxlApiDecoder::new(metadata_only),
    });
    Box::into_raw(decoder)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn jxl_decoder_destroy(decoder: *mut JxlDecoder) {
    unsafe {
        if !decoder.is_null() {
            let _ = Box::from_raw(decoder);
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn jxl_decoder_process_data(
    decoder: *mut JxlDecoder,
    data: *mut *const u8,
    data_len: *mut usize,
) -> JxlDecoderStatus {
    unsafe {
        if decoder.is_null() || data.is_null() || data_len.is_null() {
            return JxlDecoderStatus::Error;
        }

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
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn jxl_decoder_get_basic_info(decoder: *const JxlDecoder) -> JxlBasicInfo {
    unsafe {
        if decoder.is_null() {
            return JxlBasicInfo::invalid();
        }

        let decoder = &*decoder;
        let Some(basic_info) = decoder.inner.inner.basic_info() else {
            return JxlBasicInfo::invalid();
        };

        let (width, height) = if basic_info.orientation.is_transposing() {
            (basic_info.size.1, basic_info.size.0)
        } else {
            (basic_info.size.0, basic_info.size.1)
        };

        JxlBasicInfo {
            width: width as u32,
            height: height as u32,
            valid: true,
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn jxl_decoder_get_icc_size(decoder: *const JxlDecoder) -> u32 {
    unsafe {
        if decoder.is_null() {
            return 0;
        }

        let decoder = &*decoder;
        let Some(profile) = decoder.inner.inner.output_color_profile() else {
            return 0;
        };

        profile.as_icc().len() as u32
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn jxl_decoder_get_icc(
    decoder: *const JxlDecoder,
    buffer: *mut u8,
    length: u32,
) -> bool {
    unsafe {
        if decoder.is_null() || buffer.is_null() {
            return false;
        }

        let decoder = &*decoder;
        let Some(profile) = decoder.inner.inner.output_color_profile() else {
            return false;
        };

        let icc = profile.as_icc();
        if icc.len() != length as usize {
            return false;
        }

        ptr::copy_nonoverlapping(icc.as_ptr(), buffer, length as usize);
        true
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn jxl_decoder_is_frame_ready(decoder: *const JxlDecoder) -> bool {
    unsafe {
        if decoder.is_null() {
            return false;
        }

        let decoder = &*decoder;
        decoder.inner.frame_ready
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn jxl_decoder_decode_frame(
    decoder: *mut JxlDecoder,
    output_data: *mut u32,
    output_len: usize,
    pixels_written: *mut usize,
) -> JxlDecoderStatus {
    unsafe {
        if decoder.is_null() || output_data.is_null() || pixels_written.is_null() {
            return JxlDecoderStatus::Error;
        }

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
}
