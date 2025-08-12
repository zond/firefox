// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use jxl::api::{states::*, JxlDecoder, JxlDecoderOptions, JxlOutputBuffer, ProcessingResult};
use jxl::headers::extra_channels::{ExtraChannel, ExtraChannelInfo};
use qcms::c_bindings::{icSigCmykData, icSigGrayData, icSigRgbData, qcms_profile_get_color_space};
use qcms::{DataType, Intent, Profile, Transform};

/// Enum to hold the decoder in any of its typestates
enum DecoderState {
    Uninitialized,
    Initialized(JxlDecoder<Initialized>),
    WithImageInfo(JxlDecoder<WithImageInfo>),
    WithFrameInfo(JxlDecoder<WithFrameInfo>),
    Error(String),
}

impl std::fmt::Debug for DecoderState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecoderState::Uninitialized => write!(f, "Uninitialized"),
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
    pub width: u32,
    pub height: u32,
    pub alpha_premultiplied: bool,
}

pub struct JxlRustDecoder {
    state: DecoderState,
    // Cached image info from file header
    pub cached_info: Option<CachedImageInfo>,
    // Signal for frame ready to render
    pub frame_ready: bool,
    // True if Firefox only wanted CachedImageInfo
    metadata_only: bool,
    // Destination for rendering pixels
    decoded_pixels: Option<Vec<u32>>,
    // Persistent buffers for frame decoding
    color_buffer: Option<Vec<u8>>,
    alpha_buffer: Option<Vec<u8>>,
    alpha_channel: Option<u8>,
    black_buffer: Option<Vec<u8>>,
    black_channel: Option<u8>,
    // ICC profile of JXL image
    icc_profile: Option<Vec<u8>>,
    // Original number of color channels (before any conversion)
    original_color_channels: usize,
    // Info for JXL extra channels (alpha, black, ...)
    extra_channels: Option<Vec<ExtraChannelInfo>>,
    orientation_transpose: bool,
}

impl JxlRustDecoder {
    pub fn new(metadata_only: bool) -> Self {
        Self {
            state: DecoderState::Uninitialized,
            cached_info: None,
            frame_ready: false,
            decoded_pixels: None,
            color_buffer: None,
            alpha_buffer: None,
            alpha_channel: None,
            black_buffer: None,
            black_channel: None,
            metadata_only,
            icc_profile: None,
            original_color_channels: 3,
            extra_channels: None,
            orientation_transpose: false,
        }
    }

    /// Process JXL data and advance the decoder state.
    /// Returns (done, size_hint) where done indicates completion and
    /// size_hint suggests optimal buffer size for more data.
    pub fn process_data(&mut self, mut data: &[u8]) -> Result<(bool, usize), &'static str> {
        loop {
            match &mut self.state {
                DecoderState::Uninitialized => {
                    // Create decoder with default options
                    let mut options = JxlDecoderOptions::default();
                    options.xyb_output_linear = false;
                    self.state = DecoderState::Initialized(JxlDecoder::<Initialized>::new(options));
                }

                DecoderState::Initialized(_) => {
                    // Process to get image info
                    let decoder =
                        match std::mem::replace(&mut self.state, DecoderState::Uninitialized) {
                            DecoderState::Initialized(decoder) => decoder,
                            _ => unreachable!(),
                        };
                    match decoder.process(&mut data) {
                        Ok(ProcessingResult::Complete { result }) => {
                            let decoder_with_info = result;

                            // Cache image info
                            self.cache_image_info(&decoder_with_info);
                            self.state = DecoderState::WithImageInfo(decoder_with_info);

                            // If this is metadata-only decode, return early
                            if self.metadata_only {
                                return Ok((true, 0));
                            }

                            // Continue processing for full decode
                        }
                        Ok(ProcessingResult::NeedsMoreInput {
                            fallback,
                            size_hint: hint,
                        }) => {
                            if data.is_empty() {
                                return Ok((false, hint));
                            }
                            self.state = DecoderState::Initialized(fallback);
                        }
                        Err(e) => {
                            self.state = DecoderState::Error(format!("Image info error: {e:?}"));
                            return Err("Failed to process image info");
                        }
                    }
                }

                DecoderState::WithImageInfo(_) => {
                    // Take ownership of the decoder
                    let decoder =
                        match std::mem::replace(&mut self.state, DecoderState::Uninitialized) {
                            DecoderState::WithImageInfo(decoder) => decoder,
                            _ => unreachable!(),
                        };

                    match decoder.process(&mut data) {
                        Ok(ProcessingResult::Complete { result }) => {
                            // Frame info successfully parsed, prepare output buffers
                            let info = self.cached_info.as_mut().ok_or("No cached info")?;
                            let (width, height) = if self.orientation_transpose {
                                (info.height as usize, info.width as usize)
                            } else {
                                (info.width as usize, info.height as usize)
                            };

                            // Allocate buffers based on original channel count (before any conversion)
                            // Each channel is 4 bytes (f32)
                            let bytes_per_pixel = self.original_color_channels * 4;
                            self.color_buffer = Some(vec![0; width * height * bytes_per_pixel]);
                            for (idx, ec) in
                                self.extra_channels.as_ref().unwrap().iter().enumerate()
                            {
                                match ec.ec_type {
                                    ExtraChannel::Alpha => {
                                        self.alpha_channel = Some(idx as u8);
                                        self.alpha_buffer = Some(vec![0; width * height * 4]);
                                        info.alpha_premultiplied = ec.alpha_associated();
                                    }
                                    ExtraChannel::Black => {
                                        self.black_channel = Some(idx as u8);
                                        self.black_buffer = Some(vec![0; width * height * 4]);
                                    }
                                    _ => {}
                                }
                            }
                            self.state = DecoderState::WithFrameInfo(result);
                            // Continue in the loop to process frame decode immediately
                        }
                        Ok(ProcessingResult::NeedsMoreInput {
                            fallback,
                            size_hint: hint,
                        }) => {
                            if data.is_empty() {
                                return Ok((false, hint));
                            }
                            self.state = DecoderState::WithImageInfo(fallback);
                        }
                        Err(e) => {
                            self.state = DecoderState::Error(format!("Frame info error: {e:?}"));
                            return Err("Failed to process frame info");
                        }
                    }
                }

                DecoderState::WithFrameInfo(_) => {
                    let info = self.cached_info.as_ref().ok_or("No cached info")?;
                    let (width, height) = if self.orientation_transpose {
                        (info.height as usize, info.width as usize)
                    } else {
                        (info.width as usize, info.height as usize)
                    };

                    let bytes_per_row = width * self.original_color_channels * 4; // Each channel is 4 bytes (f32)
                    let mut buffers = vec![JxlOutputBuffer::new(
                        self.color_buffer.as_mut().unwrap(),
                        height,
                        bytes_per_row,
                    )];

                    let mut alpha_buffer = match &mut self.alpha_buffer {
                        Some(buf) => Some(buf),
                        _ => None,
                    };
                    let mut black_buffer = match &mut self.black_buffer {
                        Some(buf) => Some(buf),
                        _ => None,
                    };

                    for idx in 0..self.extra_channels.as_ref().unwrap().len() as u8 {
                        if Some(idx) == self.alpha_channel {
                            if let Some(buf) = alpha_buffer.take() {
                                buffers.push(JxlOutputBuffer::new(
                                    buf.as_mut_slice(),
                                    height,
                                    width * 4,
                                ));
                            }
                        } else if Some(idx) == self.black_channel {
                            let Some(buf) = black_buffer.take() else {
                                unreachable!()
                            };
                            buffers.push(JxlOutputBuffer::new(
                                buf.as_mut_slice(),
                                height,
                                width * 4,
                            ))
                        }
                    }

                    // Take ownership of the decoder
                    let decoder =
                        match std::mem::replace(&mut self.state, DecoderState::Uninitialized) {
                            DecoderState::WithFrameInfo(decoder) => decoder,
                            _ => unreachable!(),
                        };

                    match decoder.process(&mut data, &mut buffers) {
                        Ok(ProcessingResult::Complete { result }) => {
                            // Frame decoded successfully - convert the pixel data
                            let pixel_count = width * height;
                            let mut decoded_pixels = vec![0u32; pixel_count];
                            // Get the buffer data for conversion
                            let rgb_bytes = self.color_buffer.as_mut().unwrap();
                            if let Err(e) = apply_icc_color_transform(
                                rgb_bytes,
                                self.alpha_buffer.as_deref(),
                                self.black_buffer.as_deref(),
                                &mut decoded_pixels,
                                width,
                                height,
                                self.icc_profile.as_ref().unwrap(),
                                self.original_color_channels,
                            ) {
                                self.state = DecoderState::Error(e.to_string());
                                return Err(e);
                            }
                            // Store decoded pixels and clean up buffers
                            self.decoded_pixels = Some(decoded_pixels);
                            self.color_buffer = None;
                            self.alpha_buffer = None;
                            self.alpha_channel = None;
                            self.black_buffer = None;
                            self.black_channel = None;
                            self.state = DecoderState::WithImageInfo(result);
                            self.frame_ready = true;

                            // Frame decode complete
                            return Ok((true, 0));
                        }
                        Ok(ProcessingResult::NeedsMoreInput {
                            fallback,
                            size_hint: hint,
                        }) => {
                            if data.is_empty() {
                                return Ok((false, hint));
                            }
                            self.state = DecoderState::WithFrameInfo(fallback);
                        }
                        Err(e) => {
                            self.state = DecoderState::Error(format!("Frame decode error: {e:?}"));
                            return Err("Failed to decode frame");
                        }
                    }
                }

                DecoderState::Error(_) => {
                    return Err("Decoder in error state");
                }
            } // End of match
        } // End of loop
    }

    fn cache_image_info(&mut self, decoder: &JxlDecoder<WithImageInfo>) {
        let basic_info = decoder.basic_info();

        eprintln!("pixel format: {:?}", decoder.current_pixel_format());

        self.original_color_channels = decoder
            .current_pixel_format()
            .color_type
            .samples_per_pixel();
        self.orientation_transpose = basic_info.orientation.is_transposing();
        self.extra_channels = Some(basic_info.extra_channels.clone());
        self.icc_profile = Some(decoder.output_color_profile().as_icc().to_vec());

        let info = CachedImageInfo {
            width: basic_info.size.0 as u32,
            height: basic_info.size.1 as u32,
            alpha_premultiplied: false,
        };

        self.cached_info = Some(info);
    }

    pub fn is_frame_ready(&self) -> bool {
        self.frame_ready
    }

    /// Extract decoded pixels into the provided output buffer.
    ///
    /// The frame must be ready (check with is_frame_ready()) before calling this function.
    /// After successful extraction, the decoder is reset for the next frame.
    pub fn decode_frame(&mut self, output: &mut [u32]) -> Result<usize, &'static str> {
        if !self.frame_ready {
            return Err("Frame not ready for decoding");
        }

        if let Some(pixels) = &self.decoded_pixels {
            let pixel_count = pixels.len();

            if output.len() < pixel_count {
                return Err("Output buffer too small");
            }

            output[..pixel_count].copy_from_slice(pixels);

            // Reset for next frame
            self.frame_ready = false;
            self.decoded_pixels = None;

            Ok(pixel_count)
        } else {
            Err("No decoded pixels available")
        }
    }
}

/// Apply ICC color transform to convert input color space to BGRA.
fn apply_icc_color_transform(
    original_colors: &[u8],
    alpha: Option<&[u8]>,
    black: Option<&[u8]>,
    bgra: &mut [u32],
    width: usize,
    height: usize,
    icc_data: &[u8],
    original_input_channels: usize,
) -> Result<(), &'static str> {
    eprintln!("original_colors.len() {}, alpha: {}, black: {}, bgra.len {}, width {}, height {}, icc_data.len {}, input_channels {}", original_colors.len(), alpha.is_some(), black.is_some(), bgra.len(), width, height, icc_data.len(), original_input_channels);

    let input_profile = match Profile::new_from_slice(icc_data, false /* not curves only */) {
        Some(p) => p,
        None => return Err("Unable to parse ICC profile"),
    };

    let output_profile = Profile::new_sRGB();

    let pixel_count = width * height;

    let tmp_alpha_buf;
    let alpha_for_compositing = match alpha {
        Some(buf) => {
            eprintln!(
                "using {} alpha values for alpha compositing, {:?}",
                buf.len(),
                &buf[100..120]
            );
            buf
        }
        None => {
            tmp_alpha_buf = vec![255u8; pixel_count]; // Default to opaque
            tmp_alpha_buf.as_slice()
        }
    };
    let mut tmp_color_buf = vec![0u8; 0];
    #[allow(non_upper_case_globals)]
    let (input_data_type, colors_for_qcms) = match qcms_profile_get_color_space(&input_profile) {
        icSigGrayData => {
            if original_input_channels != 1 {
                return Err("Gray requires exactly one input channel");
            }
            (DataType::Gray8, original_colors)
        }
        icSigRgbData => {
            eprintln!("found rgb data");
            if original_input_channels != 3 {
                return Err("RGB requires exactly 3 input channels");
            }
            (DataType::RGB8, original_colors)
        }
        icSigCmykData => {
            if let Some(buf) = black {
                if original_input_channels == 3 {
                    tmp_color_buf = vec![0u8; pixel_count * 4 * 4];
                    for y in 0..height {
                        for x in 0..width {
                            let pixel_idx = y * width + x;
                            // Copy the first three channels from colors.
                            tmp_color_buf[pixel_idx * 16..pixel_idx * 16 + 12].copy_from_slice(
                                &original_colors[pixel_idx * 12..pixel_idx * 12 + 12],
                            );
                            // Copy the fourth channel from black.
                            tmp_color_buf[pixel_idx * 16 + 12..pixel_idx * 16 + 16]
                                .copy_from_slice(&buf[pixel_idx * 4..pixel_idx * 4 + 4]);
                        }
                    }
                } else {
                    return Err(
                        "CMYK with extra black channel requires exactly 3 regular input channels",
                    );
                }
            } else {
                if original_input_channels != 4 {
                    return Err(
                        "CMYK without extra black channel requires exactly 4 input channels",
                    );
                }
            }
            (DataType::CMYK, tmp_color_buf.as_slice())
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
        let f32_val = f32::from_ne_bytes(colors_for_qcms[i * 4..(i + 1) * 4].try_into().unwrap());
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

        bgra[i] = ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
    }

    Ok(())
}
