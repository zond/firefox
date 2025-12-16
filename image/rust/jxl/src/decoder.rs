// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use jxl::api::{
    JxlBitstreamInput, JxlColorType, JxlDecoderInner, JxlDecoderOptions, JxlOutputBuffer,
    ProcessingResult,
};
use jxl::headers::extra_channels::ExtraChannel;
use jxl::image::{Image, Rect};
use qcms::{DataType, Intent, Profile, Transform};

pub struct JxlApiDecoder {
    pub inner: JxlDecoderInner,
    metadata_only: bool,
    output_images: Vec<Image<f32>>,
    processing_frame: bool,
    pub frame_ready: bool,
    pub frame_duration: f64,
    is_grayscale: bool,
    alpha_channel_idx: Option<usize>,
    black_channel_idx: Option<usize>,
    // QCMS transform for CMYKA (SurfacePipeline can't handle alpha with CMYK)
    cmyk_transform: Option<Transform>,
}

#[derive(Debug)]
pub enum Error {
    JXL(jxl::error::Error),
    String(String),
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Error {
        Error::String(format!("{:?}", err))
    }
}

impl From<jxl::error::Error> for Error {
    fn from(err: jxl::error::Error) -> Error {
        Error::JXL(err)
    }
}

impl JxlApiDecoder {
    pub fn new(metadata_only: bool) -> Self {
        Self {
            inner: JxlDecoderInner::new(JxlDecoderOptions::default()),
            metadata_only,
            output_images: vec![],
            processing_frame: false,
            frame_ready: false,
            frame_duration: 0.0,
            is_grayscale: false,
            alpha_channel_idx: None,
            black_channel_idx: None,
            cmyk_transform: None,
        }
    }

    pub fn process_data(&mut self, data: &mut impl JxlBitstreamInput) -> Result<bool, Error> {
        loop {
            let mut output_bufs: Option<Vec<JxlOutputBuffer<'_>>> = if self.processing_frame {
                Some(
                    self.output_images
                        .iter_mut()
                        .map(|v| {
                            JxlOutputBuffer::from_image_rect_mut(
                                v.get_rect_mut(Rect {
                                    size: v.size(),
                                    origin: (0, 0),
                                })
                                .into_raw(),
                            )
                        })
                        .collect(),
                )
            } else {
                None
            };
            match self.inner.process(data, output_bufs.as_deref_mut())? {
                ProcessingResult::Complete { result: _ } => {
                    if let (Some(basic_info), Some(pixel_format)) =
                        (self.inner.basic_info(), self.inner.current_pixel_format())
                    {
                        if self.metadata_only {
                            return Ok(true);
                        }

                        if self.output_images.is_empty() {
                            self.is_grayscale = pixel_format.color_type == JxlColorType::Grayscale;
                            let channels = if self.is_grayscale { 1 } else { 3 };
                            self.output_images = vec![Image::new((
                                basic_info.size.0 * channels,
                                basic_info.size.1,
                            ))?];
                            // Allocate buffers for extra channels (alpha, black, etc.)
                            // Find the alpha and black channel indices while we're at it
                            for (i, ec) in basic_info.extra_channels.iter().enumerate() {
                                self.output_images
                                    .push(Image::new((basic_info.size.0, basic_info.size.1))?);
                                let idx = i + 1; // +1 because output_images[0] is color
                                if ec.ec_type == ExtraChannel::Alpha
                                    && self.alpha_channel_idx.is_none()
                                {
                                    self.alpha_channel_idx = Some(idx);
                                }
                                if ec.ec_type == ExtraChannel::Black
                                    && self.black_channel_idx.is_none()
                                {
                                    self.black_channel_idx = Some(idx);
                                }
                            }
                            // Create QCMS transform for CMYKA (CMYK with alpha)
                            // SurfacePipeline can't handle alpha with CMYK, so we do it here
                            if self.black_channel_idx.is_some() && self.alpha_channel_idx.is_some()
                            {
                                self.cmyk_transform = self.create_cmyk_transform();
                            }
                        } else if let (Some(frame_header), false) =
                            (self.inner.frame_header(), self.processing_frame)
                        {
                            self.processing_frame = true;
                            self.frame_duration = frame_header.duration.unwrap_or(0.0);
                            self.frame_ready = false;
                        } else if let (None, true) =
                            (self.inner.frame_header(), self.processing_frame)
                        {
                            self.processing_frame = false;
                            self.frame_ready = true;
                            return Ok(true);
                        }
                    }
                }
                ProcessingResult::NeedsMoreInput {
                    size_hint: _,
                    fallback: _,
                } => {
                    return Ok(false);
                }
            }
        }
    }

    fn create_cmyk_transform(&self) -> Option<Transform> {
        let profile = self.inner.output_color_profile()?;
        let icc_bytes = profile.as_icc();
        let input_profile = Profile::new_from_slice(&icc_bytes, false)?;
        let output_profile = Profile::new_sRGB();
        Transform::new_to(
            &input_profile,
            &output_profile,
            DataType::CMYK,
            DataType::RGB8,
            Intent::Perceptual,
        )
    }

    pub fn decode_frame(&mut self, output: &mut [u32]) -> Result<usize, Error> {
        if !self.frame_ready {
            return Err(Error::String("Frame is not ready".to_string()));
        }

        let basic_info = self.inner.basic_info().unwrap();

        if self.is_grayscale && self.black_channel_idx.is_some() {
            return Err(Error::String(
                "Can't combine grayscale with extra black channel".to_string(),
            ));
        }

        let color_image = &self.output_images[0];

        // For CMYK images with alpha, convert CMYK→RGB and combine with alpha
        // SurfacePipeline can't handle alpha with CMYK, so we do it here
        if let (Some(black_idx), Some(alpha_idx)) = (self.black_channel_idx, self.alpha_channel_idx)
        {
            let black_image = &self.output_images[black_idx];
            let alpha_image = &self.output_images[alpha_idx];
            let num_pixels = basic_info.size.0 * basic_info.size.1;

            if let Some(transform) = &self.cmyk_transform {
                // Use QCMS for ICC-based color management
                // Build CMYK input buffer with inverted values for QCMS
                // JXL uses inverted convention: 0 = max ink, 1 = no ink
                // QCMS expects standard ICC convention: 0 = no ink, 255 = max ink
                let mut cmyk_input = vec![0u8; num_pixels * 4];
                for y in 0..(basic_info.size.1) {
                    let color_row = color_image.row(y);
                    let black_row = black_image.row(y);
                    for x in 0..(basic_info.size.0) {
                        let pixel_idx = y * basic_info.size.0 + x;
                        cmyk_input[pixel_idx * 4] = u8_from_f32_inverted(color_row[x * 3]);
                        cmyk_input[pixel_idx * 4 + 1] = u8_from_f32_inverted(color_row[x * 3 + 1]);
                        cmyk_input[pixel_idx * 4 + 2] = u8_from_f32_inverted(color_row[x * 3 + 2]);
                        cmyk_input[pixel_idx * 4 + 3] = u8_from_f32_inverted(black_row[x]);
                    }
                }

                // Convert CMYK to RGB using QCMS
                let mut rgb_output = vec![0u8; num_pixels * 3];
                transform.convert(&cmyk_input, &mut rgb_output);

                // Combine RGB with alpha and output ARGB
                for y in 0..(basic_info.size.1) {
                    let alpha_row = alpha_image.row(y);
                    for x in 0..(basic_info.size.0) {
                        let pixel_idx = y * basic_info.size.0 + x;
                        let r = rgb_output[pixel_idx * 3] as u32;
                        let g = rgb_output[pixel_idx * 3 + 1] as u32;
                        let b = rgb_output[pixel_idx * 3 + 2] as u32;
                        let a = u8_from_f32(alpha_row[x]) as u32;
                        output[pixel_idx] = (a << 24) | (r << 16) | (g << 8) | b;
                    }
                }
            } else {
                // Fallback: naive CMYK→RGB conversion when no ICC profile is available
                // JXL CMYK values are "reflectance": 1 = no ink (white), 0 = max ink
                // RGB = CMY * K (all values in JXL's reflectance convention)
                for y in 0..(basic_info.size.1) {
                    let color_row = color_image.row(y);
                    let black_row = black_image.row(y);
                    let alpha_row = alpha_image.row(y);
                    for x in 0..(basic_info.size.0) {
                        let pixel_idx = y * basic_info.size.0 + x;
                        let c = color_row[x * 3];
                        let m = color_row[x * 3 + 1];
                        let y_val = color_row[x * 3 + 2];
                        let k = black_row[x];
                        let r = u8_from_f32(c * k) as u32;
                        let g = u8_from_f32(m * k) as u32;
                        let b = u8_from_f32(y_val * k) as u32;
                        let a = u8_from_f32(alpha_row[x]) as u32;
                        output[pixel_idx] = (a << 24) | (r << 16) | (g << 8) | b;
                    }
                }
            }

            return Ok(num_pixels);
        }

        // For CMYK images without alpha, output CMYK bytes for SurfacePipeline ICC transform
        if let Some(black_idx) = self.black_channel_idx {
            let black_image = &self.output_images[black_idx];
            let num_pixels = basic_info.size.0 * basic_info.size.1;

            // Output inverted CMYK values for SurfacePipeline ICC transform
            // QCMS expects standard ICC convention: 0 = no ink, 255 = max ink
            // JXL uses inverted convention: 0 = max ink, 1 = no ink
            // SurfaceFormat::CMYK expects bytes in memory as [C, M, Y, K]
            for y in 0..(basic_info.size.1) {
                let color_row = color_image.row(y);
                let black_row = black_image.row(y);
                for x in 0..(basic_info.size.0) {
                    let pixel_idx = y * basic_info.size.0 + x;
                    let c = u8_from_f32_inverted(color_row[x * 3]);
                    let m = u8_from_f32_inverted(color_row[x * 3 + 1]);
                    let y_val = u8_from_f32_inverted(color_row[x * 3 + 2]);
                    let k = u8_from_f32_inverted(black_row[x]);
                    output[pixel_idx] = u32::from_ne_bytes([c, m, y_val, k]);
                }
            }

            return Ok(num_pixels);
        }

        // For non-CMYK images, output ARGB pixels
        for y in 0..(basic_info.size.1) {
            let color_row = color_image.row(y);
            for x in 0..(basic_info.size.0) {
                let pixel_idx = y * basic_info.size.0 + x;
                let (r, g, b) = if self.is_grayscale {
                    let gray = color_row[x];
                    (gray, gray, gray)
                } else {
                    (color_row[x * 3], color_row[x * 3 + 1], color_row[x * 3 + 2])
                };

                let a = if let Some(alpha_idx) = self.alpha_channel_idx {
                    u8_from_f32(self.output_images[alpha_idx].row(y)[x]) as u32
                } else {
                    255
                };
                output[pixel_idx] = (a << 24)
                    | ((u8_from_f32(r) as u32) << 16)
                    | ((u8_from_f32(g) as u32) << 8)
                    | (u8_from_f32(b) as u32);
            }
        }
        Ok(basic_info.size.0 * basic_info.size.1)
    }
}

fn u8_from_f32(v: f32) -> u8 {
    (v * 255.0).clamp(0.0, 255.0) as u8
}

fn u8_from_f32_inverted(v: f32) -> u8 {
    ((1.0 - v) * 255.0).clamp(0.0, 255.0) as u8
}
