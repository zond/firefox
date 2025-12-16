// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use jxl::api::{
    JxlBitstreamInput, JxlColorType, JxlDecoderInner, JxlDecoderOptions, JxlOutputBuffer,
    ProcessingResult,
};
use jxl::headers::extra_channels::ExtraChannel;
use jxl::image::{Image, Rect};

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

        // For CMYK images, output CMYK bytes for SurfacePipeline ICC transform
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
