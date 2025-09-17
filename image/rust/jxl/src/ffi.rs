/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use crate::decoder::JxlApiDecoder;
use jxl::headers::extra_channels::ExtraChannel;
use std::ptr;
use std::slice;

/// Status codes for JXL decoder operations
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JxlDecoderStatus {
    Ok = 0,
    NeedMoreData = 1,
    Error = 2,
}

/// Basic information from JXL decoder (image and animation)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JxlBasicInfo {
    pub width: u32,
    pub height: u32,
    pub has_alpha: bool,
    pub cmyk: bool,
    pub alpha_premultiplied: bool,
    pub is_animated: bool,
    pub num_loops: u32,
    pub valid: bool,
}

impl JxlBasicInfo {
    fn invalid() -> Self {
        Self {
            width: 0,
            height: 0,
            has_alpha: false,
            cmyk: false,
            alpha_premultiplied: false,
            is_animated: false,
            num_loops: 0,
            valid: false,
        }
    }
}

/// Frame information from JXL decoder
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JxlFrameInfo {
    pub duration_ms: f64,
    pub valid: bool,
}

impl JxlFrameInfo {
    fn invalid() -> Self {
        Self {
            duration_ms: 0.0,
            valid: false,
        }
    }
}

/// Opaque handle to the JXL decoder
pub struct JxlDecoder {
    inner: JxlApiDecoder,
}

/// Create a new JXL decoder instance
///
/// # Safety
/// Returns a valid pointer that must be freed with jxl_decoder_destroy
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jxl_decoder_new(metadata_only: bool) -> *mut JxlDecoder {
    let decoder = Box::new(JxlDecoder {
        inner: JxlApiDecoder::new(metadata_only),
    });
    Box::into_raw(decoder)
}

/// Destroy a JXL decoder instance
///
/// # Safety
/// The decoder pointer must be valid and not used after this call
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jxl_decoder_destroy(decoder: *mut JxlDecoder) {
    unsafe {
        if !decoder.is_null() {
            let _ = Box::from_raw(decoder);
        }
    }
}

/// Process JXL data
///
/// # Safety
/// All pointers must be valid for the specified lengths
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jxl_decoder_process_data(
    decoder: *mut JxlDecoder,
    data: *mut *const u8,
    data_len: *mut u32,
) -> JxlDecoderStatus {
    unsafe {
        if decoder.is_null() || data.is_null() || data_len.is_null() {
            return JxlDecoderStatus::Error;
        }

        let decoder = &mut *decoder;
        let mut data_slice = slice::from_raw_parts(*data, *data_len as usize);

        let result = decoder.inner.process_data(&mut data_slice);

        // Update pointers after processing
        *data = data_slice.as_ptr();
        *data_len = data_slice.len() as u32;

        match result {
            Ok(true) => JxlDecoderStatus::Ok,
            Ok(false) => JxlDecoderStatus::NeedMoreData,
            Err(_) => JxlDecoderStatus::Error,
        }
    }
}

/// Get basic information (image and animation)
///
/// # Safety
/// Decoder must be valid
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

        let mut alpha_channel = None;
        let mut black_channel = None;
        for ec in &basic_info.extra_channels {
            match ec.ec_type {
                ExtraChannel::Alpha => alpha_channel = Some(ec),
                ExtraChannel::Black => black_channel = Some(ec),
                _ => {}
            }
        }

        // TODO: Remove this when jxl-rs is updated to a version that does this internally.
        let (width, height) = if basic_info.orientation.is_transposing() {
            (basic_info.size.1, basic_info.size.0)
        } else {
            (basic_info.size.0, basic_info.size.1)
        };

        let (is_animated, num_loops) = if let Some(anim) = basic_info.animation.as_ref() {
            (true, anim.num_loops)
        } else {
            (false, 0)
        };

        JxlBasicInfo {
            width: width as u32,
            height: height as u32,
            has_alpha: black_channel.is_none() && alpha_channel.is_some(),
            cmyk: black_channel.is_some(),
            alpha_premultiplied: alpha_channel.is_some_and(|ec| ec.alpha_associated),
            is_animated,
            num_loops,
            valid: true,
        }
    }
}

/// Get ICC profile size
///
/// # Safety
/// Decoder must be valid
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

/// Get ICC profile data
///
/// # Safety
/// All pointers must be valid for the specified length
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

/// Get frame information
///
/// # Safety
/// Decoder must be valid
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jxl_decoder_get_frame_info(decoder: *const JxlDecoder) -> JxlFrameInfo {
    unsafe {
        if decoder.is_null() {
            return JxlFrameInfo::invalid();
        }

        let decoder = &*decoder;
        JxlFrameInfo {
            duration_ms: decoder.inner.frame_duration,
            valid: true,
        }
    }
}

/// Check if frame is ready
///
/// # Safety
/// Decoder must be valid
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

/// Check if there are more frames
///
/// # Safety
/// Decoder must be valid
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jxl_decoder_has_more_frames(decoder: *const JxlDecoder) -> bool {
    unsafe {
        if decoder.is_null() {
            return false;
        }

        let decoder = &*decoder;
        decoder.inner.inner.has_more_frames()
    }
}

/// Decode the current frame
///
/// # Safety
/// All pointers must be valid for the specified lengths
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jxl_decoder_decode_frame(
    decoder: *mut JxlDecoder,
    output_data: *mut u32,
    output_len: u32,
    pixels_written: *mut u32,
) -> JxlDecoderStatus {
    unsafe {
        if decoder.is_null() || output_data.is_null() || pixels_written.is_null() {
            return JxlDecoderStatus::Error;
        }

        let decoder = &mut *decoder;
        let output_slice = slice::from_raw_parts_mut(output_data, output_len as usize);

        match decoder.inner.decode_frame(output_slice) {
            Ok(count) => {
                *pixels_written = count as u32;
                JxlDecoderStatus::Ok
            }
            Err(_) => JxlDecoderStatus::Error,
        }
    }
}
