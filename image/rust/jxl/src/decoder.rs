// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use jxl::api::{
    states::*, JxlAnimation, JxlBitstreamInput, JxlColorType, JxlDecoder, JxlDecoderOptions,
    JxlExtraChannel, JxlOutputBuffer, ProcessingResult,
};
use jxl::headers::extra_channels::ExtraChannel;
use qcms::c_bindings::{icSigCmykData, icSigGrayData, icSigRgbData, qcms_profile_get_color_space};
use qcms::{DataType, Intent, Profile, Transform};

enum DecoderState {
    Initialized(JxlDecoder<Initialized>),
    WithImageInfo(JxlDecoder<WithImageInfo>),
    WithFrameInfo(JxlDecoder<WithFrameInfo>),
    Error(String),
}

impl std::fmt::Debug for DecoderState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecoderState::Initialized(_) => write!(f, "Initialized"),
            DecoderState::WithImageInfo(_) => write!(f, "WithImageInfo"),
            DecoderState::WithFrameInfo(_) => write!(f, "WithFrameInfo"),
            DecoderState::Error(e) => write!(f, "Error({e})"),
        }
    }
}

/// Cached image information for C++ access
#[derive(Clone)]
pub struct CachedImageInfo {
    pub width: usize,
    pub height: usize,
    pub has_alpha: bool,
    pub alpha_premultiplied: bool,
}

pub struct JxlApiDecoder {
    state: Option<DecoderState>,
    // True if Firefox only wanted CachedImageInfo
    metadata_only: bool,

    // Populated after Initialized => WithImageInfo
    // Cached image info from file header
    pub cached_info: Option<CachedImageInfo>,
    // Global animation properties
    pub animation_info: Option<JxlAnimation>,
    // ICC profile of JXL image
    icc_profile: Option<Vec<u8>>,
    // Info for JXL extra channels (alpha, black, ...)
    extra_channels: Vec<JxlExtraChannel>,
    // Original number of color channels (before any conversion)
    original_color_channels: usize,

    // Populated after WithImageInfo => WithFrameInfo
    // Destination for API rendering of pixels
    output_vecs: Vec<Vec<u8>>,
    // Duration of the current frame in ms
    pub frame_duration: f64,
    // Index in output_vecs of alpha channel
    alpha_channel: Option<u8>,
    // Index in output_vecs of black channel
    black_channel: Option<u8>,

    // Populated after WithFrameInfo => WithImageInfo
    // Signal for frame ready to render
    pub frame_ready: bool,
    // True if there are more frames to decode.
    pub has_more_frames: bool,
}

impl JxlApiDecoder {
    pub fn new(metadata_only: bool) -> Self {
        Self {
            state: None,
            metadata_only,

            cached_info: None,
            animation_info: None,
            icc_profile: None,
            extra_channels: vec![],
            original_color_channels: 3,

            output_vecs: vec![],
            frame_duration: 0.0,
            alpha_channel: None,
            black_channel: None,

            frame_ready: false,
            has_more_frames: false,
        }
    }

    pub fn state_error(&self) -> Option<String> {
        if let Some(DecoderState::Error(msg)) = &self.state {
            Some(msg.clone())
        } else {
            None
        }
    }

    /// Process JXL data and advance the decoder state.
    /// Consumes as necessary from data.
    /// Returns true if new data available.
    pub fn process_data(
        &mut self,
        data: &mut impl JxlBitstreamInput,
    ) -> Result<bool, &'static str> {
        loop {
            match self.state.take() {
                None => {
                    let mut options = JxlDecoderOptions::default();
                    options.xyb_output_linear = false;
                    self.state = Some(DecoderState::Initialized(JxlDecoder::<Initialized>::new(
                        options,
                    )));
                }

                Some(DecoderState::Initialized(decoder)) => match decoder.process(data) {
                    Ok(ProcessingResult::Complete { result }) => {
                        self.cache_image_info(&result);
                        self.state = Some(DecoderState::WithImageInfo(result));

                        if self.metadata_only {
                            return Ok(true);
                        }
                    }
                    Ok(ProcessingResult::NeedsMoreInput {
                        fallback,
                        size_hint: _hint,
                    }) => {
                        self.state = Some(DecoderState::Initialized(fallback));
                        if data.available_bytes().unwrap() == 0 {
                            return Ok(false);
                        }
                    }
                    Err(e) => {
                        self.state = Some(DecoderState::Error(format!("Image info error: {e:?}")));
                        return Err("Failed to process image info");
                    }
                },

                Some(DecoderState::WithImageInfo(decoder)) => {
                    match decoder.process(data) {
                        Ok(ProcessingResult::Complete { result }) => {
                            let info = self.cached_info.as_mut().unwrap();
                            self.frame_duration = result.frame_header().duration.unwrap_or(0.0);
                            self.state = Some(DecoderState::WithFrameInfo(result));

                            // Allocate buffers based on original channel count (before any conversion)
                            // Each channel is 4 bytes (f32)
                            let bytes_per_pixel = self.original_color_channels * 4;
                            let num_pixels = info.width * info.height;
                            self.output_vecs = vec![vec![0; num_pixels * bytes_per_pixel]];

                            for ec in self.extra_channels.iter() {
                                match ec.ec_type {
                                    ExtraChannel::Alpha => {
                                        self.alpha_channel = Some(self.output_vecs.len() as u8);
                                        self.output_vecs.push(vec![0; num_pixels * 4]);
                                    }
                                    ExtraChannel::Black => {
                                        self.black_channel = Some(self.output_vecs.len() as u8);
                                        self.output_vecs.push(vec![0; num_pixels * 4]);
                                    }
                                    _ => {
                                        self.state = Some(DecoderState::Error(format!(
                                            "Unrecognized channel type {:?}",
                                            ec.ec_type
                                        )));
                                        return Err("Unrecognized channel type");
                                    }
                                }
                            }
                        }
                        Ok(ProcessingResult::NeedsMoreInput {
                            fallback,
                            size_hint: _hint,
                        }) => {
                            self.state = Some(DecoderState::WithImageInfo(fallback));
                            if data.available_bytes().unwrap() == 0 {
                                return Ok(false);
                            }
                        }
                        Err(e) => {
                            self.state =
                                Some(DecoderState::Error(format!("Frame info error: {e:?}")));
                            return Err("Failed to process frame info");
                        }
                    }
                }

                Some(DecoderState::WithFrameInfo(decoder)) => {
                    let info = self.cached_info.as_ref().unwrap();

                    let mut buffers: Vec<JxlOutputBuffer<'_>> = self
                        .output_vecs
                        .iter_mut()
                        .map(|v| {
                            let len = v.len();
                            JxlOutputBuffer::new(v, info.height, len / info.height)
                        })
                        .collect();

                    match decoder.process(data, &mut buffers) {
                        Ok(ProcessingResult::Complete { result }) => {
                            self.has_more_frames = result.has_more_frames();
                            self.state = Some(DecoderState::WithImageInfo(result));
                            self.frame_ready = true;
                            return Ok(true);
                        }
                        Ok(ProcessingResult::NeedsMoreInput {
                            fallback,
                            size_hint: _hint,
                        }) => {
                            self.state = Some(DecoderState::WithFrameInfo(fallback));
                            if data.available_bytes().unwrap() == 0 {
                                return Ok(false);
                            }
                        }
                        Err(e) => {
                            self.state =
                                Some(DecoderState::Error(format!("Frame decode error: {e:?}")));
                            return Err("Failed to decode frame");
                        }
                    }
                }

                Some(DecoderState::Error(_)) => {
                    return Err("Decoder in error state");
                }
            }
        }
    }

    fn cache_image_info(&mut self, decoder: &JxlDecoder<WithImageInfo>) {
        let basic_info = decoder.basic_info();

        self.animation_info = basic_info.animation.clone();

        // TODO(zond): This is what the jxl-rs API actually does today - might have to change
        // it if the API becomes cleverer.
        self.original_color_channels =
            if decoder.current_pixel_format().color_type == JxlColorType::Grayscale {
                1
            } else {
                3
            };
        self.extra_channels = basic_info.extra_channels.clone();
        self.icc_profile = Some(decoder.output_color_profile().as_icc().to_vec());

        let alpha_channel = self
            .extra_channels
            .iter()
            .find(|ec| ec.ec_type == ExtraChannel::Alpha);
        let info = CachedImageInfo {
            width: if basic_info.orientation.is_transposing() {
                basic_info.size.1
            } else {
                basic_info.size.0
            },
            height: if basic_info.orientation.is_transposing() {
                basic_info.size.0
            } else {
                basic_info.size.1
            },
            has_alpha: alpha_channel.is_some(),
            alpha_premultiplied: alpha_channel.map_or(false, |ec| ec.alpha_associated),
        };

        self.cached_info = Some(info);
    }

    /// Extract decoded pixels into the provided output buffer.
    ///
    /// The frame must be ready (check with is_frame_ready()) before calling this function.
    /// After successful extraction, the decoder is reset for the next frame.
    pub fn decode_frame(&mut self, output: &mut [u32]) -> Result<usize, &'static str> {
        if !self.frame_ready {
            return Err("Frame not ready for decoding");
        }

        match self.apply_icc_color_transform(output) {
            Err(e) => {
                self.state = Some(DecoderState::Error(e.to_string()));
                Err(e)
            }
            Ok(pixel_count) => {
                self.output_vecs = vec![];
                self.alpha_channel = None;
                self.black_channel = None;
                self.frame_ready = false;
                Ok(pixel_count)
            }
        }
    }

    /// Apply ICC color transform to convert input color space to RGBA.
    /// If we don't actually use alpha, then the A channel will just be opaque,
    /// and will later be discarded after nsJXLRustDecoder::ProcessFrame
    /// registers the image as RGBX.
    fn apply_icc_color_transform(&self, rgba: &mut [u32]) -> Result<usize, &'static str> {
        let input_profile = match Profile::new_from_slice(self.icc_profile.as_ref().unwrap(), false)
        {
            Some(p) => p,
            None => return Err("Unable to parse ICC profile"),
        };

        let alpha = self
            .alpha_channel
            .map(|idx| self.output_vecs[idx as usize].as_slice());
        let black = self
            .black_channel
            .map(|idx| self.output_vecs[idx as usize].as_slice());

        let info: &CachedImageInfo = self.cached_info.as_ref().unwrap();

        let output_profile = Profile::new_sRGB();

        let pixel_count = info.width * info.height;

        let alpha_for_compositing = match alpha {
            Some(alpha_buffer) => {
                let mut buf = vec![0u8; pixel_count];
                for (idx, alpha) in buf.iter_mut().enumerate() {
                    *alpha =
                        f32::from_ne_bytes(alpha_buffer[idx * 4..idx * 4 + 4].try_into().unwrap())
                            as u8;
                }
                buf
            }
            None => {
                vec![255u8; pixel_count] // Default to opaque
            }
        };
        let mut tmp_color_buf = vec![0u8; 0];
        #[allow(non_upper_case_globals)]
        let (input_data_type, colors_for_qcms) = match qcms_profile_get_color_space(&input_profile)
        {
            icSigGrayData => {
                if self.original_color_channels != 1 {
                    return Err("Gray requires exactly one input channel");
                }
                (DataType::Gray8, &self.output_vecs[0])
            }
            icSigRgbData => {
                if self.original_color_channels != 3 {
                    return Err("RGB requires exactly 3 input channels");
                }
                (DataType::RGB8, &self.output_vecs[0])
            }
            icSigCmykData => {
                // TODO(zond): The jxl-rs API only ever returns 1 or 3 channels as of now, when it's cleverer
                // maybe improve this.
                if self.original_color_channels != 3 {
                    return Err(
                        "CMYK with extra black channel requires exactly 3 regular input channels",
                    );
                }
                if let Some(buf) = black {
                    tmp_color_buf = vec![0u8; pixel_count * 4 * 4];
                    for y in 0..(info.height) {
                        for x in 0..(info.width) {
                            let pixel_idx = y * info.width + x;
                            // Copy the first three channels from colors.
                            tmp_color_buf[pixel_idx * 16..pixel_idx * 16 + 12].copy_from_slice(
                                &self.output_vecs[0][pixel_idx * 12..pixel_idx * 12 + 12],
                            );
                            // Copy the fourth channel from black.
                            tmp_color_buf[pixel_idx * 16 + 12..pixel_idx * 16 + 16]
                                .copy_from_slice(&buf[pixel_idx * 4..pixel_idx * 4 + 4]);
                        }
                    }
                }
                (DataType::CMYK, &tmp_color_buf)
            }
            _ => {
                // Unsupported color space - could be LAB, XYZ, or other formats
                // that qcms doesn't currently support in our DataType enum
                return Err("Unknown color space");
            }
        };

        let qcms_input_channels = input_data_type.bytes_per_pixel();
        let mut input_u8 = vec![0u8; pixel_count * qcms_input_channels];

        // Convert f32 input to u8 input for qcms
        for i in 0..(pixel_count * qcms_input_channels) {
            let f32_val =
                f32::from_ne_bytes(colors_for_qcms[i * 4..(i + 1) * 4].try_into().unwrap());
            input_u8[i] = f32_val.clamp(0.0, 255.0) as u8;
        }
        let transform = Transform::new_to(
            &input_profile,
            &output_profile,
            input_data_type,
            DataType::RGB8,
            Intent::Perceptual,
        )
        .ok_or("Unable to create color transform")?;
        let mut rgb_u8 = vec![0u8; pixel_count * 3];
        transform.convert(&input_u8, &mut rgb_u8);

        for i in 0..pixel_count {
            let r = rgb_u8[i * 3];
            let g = rgb_u8[i * 3 + 1];
            let b = rgb_u8[i * 3 + 2];
            let a = alpha_for_compositing[i];

            rgba[i] = ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
        }

        Ok(pixel_count)
    }
}
