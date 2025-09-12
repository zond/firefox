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
    pub channels: usize,
    pub has_alpha: bool,
    pub has_black: bool,
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
    pub icc_profile: Option<Vec<u8>>,
    // Info for JXL extra channels (alpha, black, ...)
    extra_channels: Vec<JxlExtraChannel>,

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

fn u8_from_f32_u8s(v: &[u8]) -> u8 {
    f32::from_ne_bytes(v.try_into().unwrap()).clamp(0.0, 255.0) as u8
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
                            let bytes_per_pixel = info.channels * 4;
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
            // TODO(zond): This is what the jxl-rs API actually does today - might have to change
            // it if the API becomes cleverer.
            channels: if decoder.current_pixel_format().color_type == JxlColorType::Grayscale {
                1
            } else {
                3
            },
            has_alpha: alpha_channel.is_some(),
            has_black: self.black_channel.is_some(),
            alpha_premultiplied: alpha_channel.map_or(false, |ec| ec.alpha_associated),
        };

        self.cached_info = Some(info);
    }

    /// Extract decoded pixels into the provided output buffer.
    ///
    /// The frame must be ready (check with is_frame_ready()) before calling this function.
    /// After successful extraction, the decoder is reset for the next frame.
    pub fn decode_frame(&mut self, output: &mut [u8]) -> Result<usize, &'static str> {
        if !self.frame_ready {
            return Err("Frame not ready for decoding");
        }

        let result = self.prepare_color_channels(output);

        match result {
            Err(e) => {
                self.state = Some(DecoderState::Error(e.to_string()));
            }
            Ok(pixel_count) => {
                self.output_vecs = vec![];
                self.alpha_channel = None;
                self.black_channel = None;
                self.frame_ready = false;
            }
        }

        result
    }

    /// TODO: Should this be inlined inside decode_frame?
    /// Convert the colors + (optional) alpha or (optional) black to u32.
    /// Note that this disallows the combination of alpha _and_ black, but CMYK with alpha doesn't seem to be a thing.
    fn prepare_color_channels(&self, output: &mut [u8]) -> Result<usize, &'static str> {
        let info: &CachedImageInfo = self.cached_info.as_ref().unwrap();

        match info.channels {
            1 => {
                if self.black_channel.is_some() {
                    return Err("Can't combine grayscale with extra black channel");
                }
                if let Some(alpha_idx) = self.alpha_channel {
                    let alpha = &self.output_vecs[alpha_idx as usize];
                    for y in 0..(info.height) {
                        for x in 0..(info.width) {
                            let pixel_idx = y * info.width + x;
                            output[pixel_idx * 2] = u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 4..pixel_idx * 4 + 4],
                            );
                            output[pixel_idx * 2 + 1] =
                                u8_from_f32_u8s(&alpha[pixel_idx * 4..pixel_idx * 4 + 4]);
                        }
                    }
                } else {
                    for y in 0..(info.height) {
                        for x in 0..(info.width) {
                            let pixel_idx = y * info.width + x;
                            output[pixel_idx] = u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 4..pixel_idx * 4 + 4],
                            );
                        }
                    }
                }
            }
            3 => {
                if self.black_channel.is_some() && self.alpha_channel.is_some() {
                    return Err("Can't have both alpha and black channel");
                }
                if let Some(alpha_idx) = self.alpha_channel {
                    let alpha = &self.output_vecs[alpha_idx as usize];
                    for y in 0..(info.height) {
                        for x in 0..(info.width) {
                            let pixel_idx = y * info.width + x;
                            output[pixel_idx * 4] = u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 4..pixel_idx * 4 + 4],
                            );
                            output[pixel_idx * 4 + 1] = u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 4 + 4..pixel_idx * 4 + 8],
                            );
                            output[pixel_idx * 4 + 2] = u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 4 + 8..pixel_idx * 4 + 12],
                            );
                            output[pixel_idx * 4 + 3] =
                                u8_from_f32_u8s(&alpha[pixel_idx * 4..pixel_idx * 4 + 4]);
                        }
                    }
                } else if let Some(black_idx) = self.black_channel {
                    let black = &self.output_vecs[black_idx as usize];
                    for y in 0..(info.height) {
                        for x in 0..(info.width) {
                            let pixel_idx = y * info.width + x;
                            output[pixel_idx * 4] = u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 4..pixel_idx * 4 + 4],
                            );
                            output[pixel_idx * 4 + 1] = u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 4 + 4..pixel_idx * 4 + 8],
                            );
                            output[pixel_idx * 4 + 2] = u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 4 + 8..pixel_idx * 4 + 12],
                            );
                            output[pixel_idx * 4 + 3] =
                                u8_from_f32_u8s(&black[pixel_idx * 4..pixel_idx * 4 + 4]);
                        }
                    }
                } else {
                    for y in 0..(info.height) {
                        for x in 0..(info.width) {
                            let pixel_idx = y * info.width + x;
                            output[pixel_idx * 3] = u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 4..pixel_idx * 4 + 4],
                            );
                            output[pixel_idx * 3 + 1] = u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 4 + 4..pixel_idx * 4 + 8],
                            );
                            output[pixel_idx * 3 + 2] = u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 4 + 8..pixel_idx * 4 + 12],
                            );
                        }
                    }
                }
            }
            _ => {
                // Unsupported color space - could be LAB, XYZ, or other formats
                // that qcms doesn't currently support in our DataType enum
                return Err("Unknown number of color channels");
            }
        };
        Ok(info.width * info.height)
    }
}
