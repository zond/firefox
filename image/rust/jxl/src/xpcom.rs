/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use crate::decoder::JxlApiDecoder;
use jxl::api::JxlBasicInfo;
use jxl::headers::extra_channels::ExtraChannel;
use log::error;
use nserror::{nsresult, NS_ERROR_FAILURE, NS_ERROR_NOT_INITIALIZED, NS_ERROR_NULL_POINTER, NS_OK};
use std::cell::RefCell;
use std::sync::Mutex;
use xpcom::{interfaces::*, RefPtr};
use xpcom::{nsIID, xpcom, xpcom_method};

// Status codes matching the IDL
const STATUS_OK: u16 = 0;
const STATUS_NEED_MORE_DATA: u16 = 1;
const STATUS_INVALID_DATA: u16 = 2;

// Image info implementation
#[xpcom(implement(nsIJXLImageInfo), atomic)]
struct JXLImageInfo {
    width: u32,
    height: u32,
    has_alpha: bool,
    cmyk: bool,
    alpha_premultiplied: bool,
}

impl JXLImageInfo {
    fn new(basic_info: &JxlBasicInfo) -> RefPtr<Self> {
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
        JXLImageInfo::allocate(InitJXLImageInfo {
            width: width as u32,
            height: height as u32,
            has_alpha: black_channel.is_none() && alpha_channel.is_some(),
            cmyk: black_channel.is_some(),
            alpha_premultiplied: alpha_channel.map_or(false, |ec| ec.alpha_associated),
        })
    }

    xpcom_method!(get_width => GetWidth() -> u32);
    fn get_width(&self) -> Result<u32, nsresult> {
        Ok(self.width)
    }

    xpcom_method!(get_height => GetHeight() -> u32);
    fn get_height(&self) -> Result<u32, nsresult> {
        Ok(self.height)
    }

    xpcom_method!(get_has_alpha => GetHasAlpha() -> bool);
    fn get_has_alpha(&self) -> Result<bool, nsresult> {
        Ok(self.has_alpha)
    }

    xpcom_method!(get_cmyk => GetCmyk() -> bool);
    fn get_cmyk(&self) -> Result<bool, nsresult> {
        Ok(self.cmyk)
    }

    xpcom_method!(get_alpha_premultiplied => GetAlphaPremultiplied() -> bool);
    fn get_alpha_premultiplied(&self) -> Result<bool, nsresult> {
        Ok(self.alpha_premultiplied)
    }
}

// Animation info implementation
#[xpcom(implement(nsIJXLAnimationInfo), atomic)]
struct JXLAnimationInfo {
    is_animated: bool,
    num_loops: u32,
    have_timecodes: bool,
}

impl JXLAnimationInfo {
    fn new(decoder: &JxlApiDecoder) -> RefPtr<Self> {
        if let Some(anim_info) = &decoder.inner.basic_info().unwrap().animation {
            JXLAnimationInfo::allocate(InitJXLAnimationInfo {
                is_animated: true,
                num_loops: anim_info.num_loops,
                have_timecodes: anim_info.have_timecodes,
            })
        } else {
            JXLAnimationInfo::allocate(InitJXLAnimationInfo {
                is_animated: false,
                num_loops: 0,
                have_timecodes: false,
            })
        }
    }

    xpcom_method!(get_is_animated => GetIsAnimated() -> bool);
    fn get_is_animated(&self) -> Result<bool, nsresult> {
        Ok(self.is_animated)
    }

    xpcom_method!(get_num_loops => GetNumLoops() -> u32);
    fn get_num_loops(&self) -> Result<u32, nsresult> {
        Ok(self.num_loops)
    }
}

// Frame info implementation
#[xpcom(implement(nsIJXLFrameInfo), atomic)]
struct JXLFrameInfo {
    duration_ms: f64,
}

impl JXLFrameInfo {
    fn new(decoder: &JxlApiDecoder) -> RefPtr<Self> {
        JXLFrameInfo::allocate(InitJXLFrameInfo {
            duration_ms: decoder.frame_duration,
        })
    }

    xpcom_method!(get_duration_ms => GetDurationMs() -> f64);
    fn get_duration_ms(&self) -> Result<f64, nsresult> {
        Ok(self.duration_ms)
    }
}

// Main decoder implementation
#[xpcom(implement(nsIJXLDecoder, nsIJXLDecoderStatus), atomic)]
struct JXLDecoder {
    inner: Mutex<RefCell<Option<JxlApiDecoder>>>,
}

impl JXLDecoder {
    fn new() -> RefPtr<Self> {
        JXLDecoder::allocate(InitJXLDecoder {
            inner: Mutex::new(RefCell::new(None)),
        })
    }

    xpcom_method!(init => Init(metadata_only: bool));
    fn init(&self, metadata_only: bool) -> Result<(), nsresult> {
        let guard = self.inner.lock().map_err(|_| NS_ERROR_FAILURE)?;
        let mut decoder_ref = guard.borrow_mut();
        *decoder_ref = Some(JxlApiDecoder::new(metadata_only));
        Ok(())
    }

    xpcom_method!(process_data => ProcessData(data: *mut *const u8, data_len: *mut u32) -> u16);
    unsafe fn process_data(
        &self,
        data: *mut *const u8,
        data_len: *mut u32,
    ) -> Result<u16, nsresult> {
        if data.is_null() {
            return Err(NS_ERROR_NULL_POINTER);
        }
        let mut data_slice = std::slice::from_raw_parts(*data, *data_len as usize);
        let guard = self.inner.lock().map_err(|_| NS_ERROR_FAILURE)?;
        let mut decoder_ref = guard.borrow_mut();
        let decoder = decoder_ref.as_mut().ok_or(NS_ERROR_NOT_INITIALIZED)?;

        let result = decoder.process_data(&mut data_slice);
        *data = data_slice.as_ptr();
        *data_len = data_slice.len() as u32;

        match result {
            Ok(true) => Ok(STATUS_OK),
            Ok(false) => Ok(STATUS_NEED_MORE_DATA),
            Err(err) => {
                error!("JXL XPCOM decoder error: {:?}", err);
                Ok(STATUS_INVALID_DATA)
            }
        }
    }

    xpcom_method!(get_image_info => GetImageInfo() -> *const nsIJXLImageInfo);
    fn get_image_info(&self) -> Result<RefPtr<nsIJXLImageInfo>, nsresult> {
        let guard = self.inner.lock().map_err(|_| NS_ERROR_FAILURE)?;
        let decoder_ref = guard.borrow();
        let decoder = decoder_ref.as_ref().ok_or(NS_ERROR_NOT_INITIALIZED)?;

        let info = JXLImageInfo::new(decoder.inner.basic_info().unwrap());
        Ok(info.query_interface().unwrap())
    }

    xpcom_method!(get_icc_size => GetICCSize() -> u32);
    fn get_icc_size(&self) -> Result<u32, nsresult> {
        let guard = self.inner.lock().map_err(|_| NS_ERROR_FAILURE)?;
        let decoder_ref = guard.borrow();
        let decoder = decoder_ref.as_ref().ok_or(NS_ERROR_NOT_INITIALIZED)?;
        Ok(decoder.inner.output_color_profile().unwrap().as_icc().len() as u32)
    }

    xpcom_method!(get_icc => GetICC(buffer: *mut u8, length: u32));
    unsafe fn get_icc(&self, buffer: *mut u8, length: u32) -> Result<(), nsresult> {
        let guard = self.inner.lock().map_err(|_| NS_ERROR_FAILURE)?;
        let decoder_ref = guard.borrow();
        let decoder = decoder_ref.as_ref().ok_or(NS_ERROR_NOT_INITIALIZED)?;

        let icc = decoder.inner.output_color_profile().unwrap().as_icc();
        assert!(icc.len() == length as usize);

        // Copy data
        std::ptr::copy_nonoverlapping(icc.as_ptr(), buffer, length as usize);

        Ok(())
    }

    xpcom_method!(get_animation_info => GetAnimationInfo() -> *const nsIJXLAnimationInfo);
    fn get_animation_info(&self) -> Result<RefPtr<nsIJXLAnimationInfo>, nsresult> {
        let guard = self.inner.lock().map_err(|_| NS_ERROR_FAILURE)?;
        let decoder_ref = guard.borrow();
        let decoder = decoder_ref.as_ref().ok_or(NS_ERROR_NOT_INITIALIZED)?;

        let info = JXLAnimationInfo::new(decoder);
        Ok(info.query_interface().unwrap())
    }

    xpcom_method!(get_frame_info => GetFrameInfo() -> *const nsIJXLFrameInfo);
    fn get_frame_info(&self) -> Result<RefPtr<nsIJXLFrameInfo>, nsresult> {
        let guard = self.inner.lock().map_err(|_| NS_ERROR_FAILURE)?;
        let decoder_ref = guard.borrow();
        let decoder = decoder_ref.as_ref().ok_or(NS_ERROR_NOT_INITIALIZED)?;

        let info = JXLFrameInfo::new(decoder);
        Ok(info.query_interface().unwrap())
    }

    xpcom_method!(is_frame_ready => IsFrameReady() -> bool);
    fn is_frame_ready(&self) -> Result<bool, nsresult> {
        let guard = self.inner.lock().map_err(|_| NS_ERROR_FAILURE)?;
        let decoder_ref = guard.borrow();
        let decoder = decoder_ref.as_ref().ok_or(NS_ERROR_NOT_INITIALIZED)?;
        Ok(decoder.frame_ready)
    }

    xpcom_method!(has_more_frames => HasMoreFrames() -> bool);
    fn has_more_frames(&self) -> Result<bool, nsresult> {
        let guard = self.inner.lock().map_err(|_| NS_ERROR_FAILURE)?;
        let decoder_ref = guard.borrow();
        let decoder = decoder_ref.as_ref().ok_or(NS_ERROR_NOT_INITIALIZED)?;
        Ok(decoder.inner.has_more_frames())
    }

    xpcom_method!(
        decode_frame => DecodeFrame(
            output_data: *mut u32,
            output_len: u32,
            pixels_written: *mut u32
        ) -> u16
    );
    unsafe fn decode_frame(
        &self,
        output_data: *mut u32,
        output_len: u32,
        pixels_written: *mut u32,
    ) -> Result<u16, nsresult> {
        if output_data.is_null() {
            return Err(NS_ERROR_NULL_POINTER);
        }
        let output_data_slice = std::slice::from_raw_parts_mut(output_data, output_len as usize);
        let guard = self.inner.lock().map_err(|_| NS_ERROR_FAILURE)?;
        let mut decoder_ref = guard.borrow_mut();
        let decoder = decoder_ref.as_mut().ok_or(NS_ERROR_NOT_INITIALIZED)?;

        match decoder.decode_frame(output_data_slice) {
            Ok(count) => {
                *pixels_written = count as u32;
                Ok(STATUS_OK)
            }
            Err(err) => {
                error!("JXL XPCOM decoder error: {:?}", err);
                Ok(STATUS_INVALID_DATA)
            }
        }
    }
}

// Constructor function for XPCOM
#[no_mangle]
pub unsafe extern "C" fn nsJXLDecoderConstructor(
    iid: &nsIID,
    result: *mut *mut libc::c_void,
) -> nsresult {
    let decoder = JXLDecoder::new();
    decoder.QueryInterface(iid, result)
}
