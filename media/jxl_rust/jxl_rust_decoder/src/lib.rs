// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use jxl::api::{
    states::*, JxlColorType, JxlDecoder, JxlDecoderOptions, JxlOutputBuffer, ProcessingResult,
};
use qcms::{DataType, Intent, Profile, Transform};

/// JXL magic signatures
pub const JXL_CODESTREAM_SIGNATURE: &[u8] = &[0xFF, 0x0A];
pub const JXL_CONTAINER_SIGNATURE: &[u8] = &[
    0x00, 0x00, 0x00, 0x0C, 0x4A, 0x58, 0x4C, 0x20, 0x0D, 0x0A, 0x87, 0x0A,
];

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
    pub has_alpha: bool,
    pub orientation_transpose: bool,
    pub is_grayscale: bool,
}

pub struct JxlRustDecoder {
    state: DecoderState,
    pub cached_info: Option<CachedImageInfo>,
    pub frame_ready: bool,
    decoded_pixels: Option<Vec<u32>>,
    // Persistent buffers for frame decoding
    rgb_buffer: Option<Vec<u8>>,
    alpha_buffer: Option<Vec<u8>>,
    metadata_only: bool,
    // ICC profile for color management
    icc_profile: Option<Vec<u8>>,
    // Original number of color channels (before any conversion)
    original_color_channels: usize,
}

impl JxlRustDecoder {
    pub fn new(metadata_only: bool) -> Self {
        Self {
            state: DecoderState::Uninitialized,
            cached_info: None,
            frame_ready: false,
            decoded_pixels: None,
            rgb_buffer: None,
            alpha_buffer: None,
            metadata_only,
            icc_profile: None,
            original_color_channels: 3,
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
                            let info = self.cached_info.as_ref().ok_or("No cached info")?;
                            let (width, height) = if info.orientation_transpose {
                                (info.height as usize, info.width as usize)
                            } else {
                                (info.width as usize, info.height as usize)
                            };

                            // Allocate buffers based on original channel count (before any conversion)
                            // Each channel is 4 bytes (f32)
                            let bytes_per_pixel = self.original_color_channels * 4;

                            self.rgb_buffer = Some(vec![0; width * height * bytes_per_pixel]);
                            self.alpha_buffer = if info.has_alpha {
                                Some(vec![0; width * height * 4])
                            } else {
                                None
                            };

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
                    // Use existing persistent buffers
                    let info = self.cached_info.as_ref().ok_or("No cached info")?;
                    let (width, height) = if info.orientation_transpose {
                        (info.height as usize, info.width as usize)
                    } else {
                        (info.width as usize, info.height as usize)
                    };

                    // Create output buffers from the persistent buffers
                    // Calculate bytes per row based on original number of color channels
                    let bytes_per_row = width * self.original_color_channels * 4; // Each channel is 4 bytes (f32)
                    let mut buffers = vec![JxlOutputBuffer::new(
                        self.rgb_buffer.as_mut().ok_or("No RGB buffer allocated")?,
                        height,
                        bytes_per_row,
                    )];
                    if let Some(alpha_buf) = self.alpha_buffer.as_mut() {
                        buffers.push(JxlOutputBuffer::new(alpha_buf, height, width * 4));
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
                            let rgb_bytes = self.rgb_buffer.as_mut().unwrap();

                            let alpha_bytes = self.alpha_buffer.as_deref();

                            // Apply ICC color transformation for CMYK images
                            let actual_color_channels = if self.original_color_channels > 3 {
                                // CMYK requires ICC profile for conversion to RGB
                                if let Some(icc_data) = &self.icc_profile {
                                    if apply_cmyk_to_rgb_transform(
                                        rgb_bytes, width, height, icc_data,
                                    ) {
                                        3 // After transformation, we have RGB data
                                    } else {
                                        self.state = DecoderState::Error(format!(
                                            "Failed to apply color transform for {} channel image",
                                            self.original_color_channels
                                        ));
                                        return Err("Failed to apply color transform");
                                    }
                                } else {
                                    self.state = DecoderState::Error(format!(
                                        "Image has {} color channels but no ICC profile for conversion to RGB",
                                        self.original_color_channels
                                    ));
                                    return Err("No ICC profile for multi-channel image");
                                }
                            } else if info.is_grayscale {
                                1 // Grayscale has 1 color channel
                            } else {
                                3 // RGB has 3 color channels
                            };

                            // Convert pixels
                            convert_f32_rgb_to_u32_bgra(
                                rgb_bytes,
                                &mut decoded_pixels,
                                width,
                                height,
                                info.has_alpha,
                                alpha_bytes,
                                actual_color_channels, // Use actual channels after transformation
                            );

                            // Store decoded pixels and clean up buffers
                            self.decoded_pixels = Some(decoded_pixels);
                            self.rgb_buffer = None;
                            self.alpha_buffer = None;
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
        let pixel_format = decoder.current_pixel_format();

        // Determine number of color channels based on color type
        // jxl-rs outputs actual channels, so we need to check what we're really getting
        let num_color_channels = pixel_format.color_type.samples_per_pixel();
        let has_alpha = pixel_format.color_type.has_alpha();

        // Store the original number of color channels (excluding alpha)
        // For CMYK, we expect 4 color channels (C, M, Y, K) plus optional alpha
        // For RGB, we expect 3 color channels (R, G, B) plus optional alpha
        // For Grayscale, we expect 1 color channel plus optional alpha
        self.original_color_channels = if has_alpha {
            num_color_channels - 1
        } else {
            num_color_channels
        };

        // Extract ICC profile from the image
        let color_profile = decoder.output_color_profile();
        let icc_bytes = color_profile.as_icc();
        self.icc_profile = Some(icc_bytes.to_vec());

        let is_grayscale = matches!(
            pixel_format.color_type,
            JxlColorType::Grayscale | JxlColorType::GrayscaleAlpha
        );

        let info = CachedImageInfo {
            width: basic_info.size.0 as u32,
            height: basic_info.size.1 as u32,
            has_alpha,
            orientation_transpose: basic_info.orientation.is_transposing(),
            is_grayscale,
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

/// Apply color transform from CMYK to RGB
fn apply_cmyk_to_rgb_transform(
    data: &mut [u8],
    width: usize,
    height: usize,
    icc_data: &[u8],
) -> bool {
    // Parse the ICC profile (false = not curves_only)
    let input_profile = match Profile::new_from_slice(icc_data, false) {
        Some(p) => p,
        None => return false,
    };

    // Create sRGB output profile
    let output_profile = Profile::new_sRGB();

    // Create transform from CMYK to RGB
    let transform = match Transform::new_to(
        &input_profile,
        &output_profile,
        DataType::CMYK,
        DataType::RGB8,
        Intent::Perceptual,
    ) {
        Some(t) => t,
        None => return false,
    };

    // Convert f32 data to u8 for qcms
    let pixel_count = width * height;
    let mut cmyk_u8 = vec![0u8; pixel_count * 4];
    let mut rgb_u8 = vec![0u8; pixel_count * 3];

    // Convert f32 CMYK to u8 CMYK
    for i in 0..pixel_count * 4 {
        let f32_val = f32::from_ne_bytes(data[i * 4..(i + 1) * 4].try_into().unwrap());
        cmyk_u8[i] = (f32_val.clamp(0.0, 1.0) * 255.0) as u8;
    }

    // Apply color transform
    transform.convert(&cmyk_u8, &mut rgb_u8);

    // Convert u8 RGB back to f32 RGB
    for i in 0..pixel_count * 3 {
        let f32_val = rgb_u8[i] as f32 / 255.0;
        data[i * 4..(i + 1) * 4].copy_from_slice(&f32_val.to_ne_bytes());
    }
    true
}

/// Convert f32 RGB/Grayscale/CMYK to u32 BGRA packed format
fn convert_f32_rgb_to_u32_bgra(
    rgb_buffer: &[u8],
    output: &mut [u32],
    width: usize,
    height: usize,
    has_alpha: bool,
    alpha_buffer: Option<&[u8]>,
    num_color_channels: usize,
) {
    for y in 0..height {
        for x in 0..width {
            let pixel_idx = y * width + x;

            // Extract f32 values based on number of color channels
            let (r, g, b) = if num_color_channels == 1 {
                // Grayscale: single channel, replicate to RGB
                let gray_offset = pixel_idx * 4;
                let gray = f32::from_ne_bytes(
                    rgb_buffer[gray_offset..gray_offset + 4].try_into().unwrap(),
                );
                (gray, gray, gray)
            } else if num_color_channels == 3 {
                // RGB: 3 channels (includes converted CMYK)
                let rgb_offset = pixel_idx * 12;
                let r =
                    f32::from_ne_bytes(rgb_buffer[rgb_offset..rgb_offset + 4].try_into().unwrap());
                let g = f32::from_ne_bytes(
                    rgb_buffer[rgb_offset + 4..rgb_offset + 8]
                        .try_into()
                        .unwrap(),
                );
                let b = f32::from_ne_bytes(
                    rgb_buffer[rgb_offset + 8..rgb_offset + 12]
                        .try_into()
                        .unwrap(),
                );
                (r, g, b)
            } else {
                // Shouldn't reach here after conversion
                (0.0, 0.0, 0.0)
            };

            // Get alpha if available
            let a = if has_alpha {
                if let Some(alpha) = alpha_buffer {
                    let alpha_offset = pixel_idx * 4;
                    f32::from_ne_bytes(alpha[alpha_offset..alpha_offset + 4].try_into().unwrap())
                } else {
                    255.0
                }
            } else {
                255.0
            };

            // Convert to u8 and pack as BGRA (actually ARGB in memory on little-endian)
            let r_u8 = (r.clamp(0.0, 255.0)) as u8;
            let g_u8 = (g.clamp(0.0, 255.0)) as u8;
            let b_u8 = (b.clamp(0.0, 255.0)) as u8;
            let a_u8 = (a.clamp(0.0, 255.0)) as u8;

            // Pack as 0xAARRGGBB for OS_RGBX format
            output[pixel_idx] = ((a_u8 as u32) << 24)
                | ((r_u8 as u32) << 16)
                | ((g_u8 as u32) << 8)
                | (b_u8 as u32);
        }
    }
}
