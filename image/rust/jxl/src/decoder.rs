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

    // True if Firefox only wanted metadata.
    metadata_only: bool,

    // Destination for API rendering of pixels
    output_images: Vec<Image<f32>>,
    alpha_index: Option<usize>,
    black_index: Option<usize>,

    // True if we are processing frame content
    processing_frame: bool,

    // Signal for frame ready to render
    pub frame_ready: bool,

    // Cached during frame processing to make available after the frame is cleaned up
    pub frame_duration: f64,
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
            alpha_index: None,
            black_index: None,
            processing_frame: false,
            frame_ready: false,
            frame_duration: 0.0,
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
                            // Return after image metadata when that's the only thing we wanted.
                            return Ok(true);
                        }

                        if self.output_images.is_empty() {
                            // TODO(zond): This is what the jxl-rs API actually does today - might have to change
                            // it if the API becomes cleverer.
                            let channels = if pixel_format.color_type == JxlColorType::Grayscale {
                                1
                            } else {
                                3
                            };
                            self.output_images = vec![Image::new((
                                basic_info.size.0 * channels,
                                basic_info.size.1,
                            ))?];
                            for ec in basic_info.extra_channels.iter() {
                                match ec.ec_type {
                                    ExtraChannel::Alpha => {
                                        self.alpha_index = Some(self.output_images.len());
                                        self.output_images.push(Image::new((
                                            basic_info.size.0,
                                            basic_info.size.1,
                                        ))?);
                                    }
                                    ExtraChannel::Black => {
                                        self.black_index = Some(self.output_images.len());
                                        self.output_images.push(Image::new((
                                            basic_info.size.0,
                                            basic_info.size.1,
                                        ))?);
                                    }
                                    _ => {
                                        // We don't yet know how to handle other channels, but let's
                                        // not crash the tab because of that. Just add it so the decoder
                                        // can populate it and then ignore it.
                                        self.output_images.push(Image::new((
                                            basic_info.size.0,
                                            basic_info.size.1,
                                        ))?);
                                    }
                                }
                            }
                            // Don't return just because we have image info, decode a frame as well.
                        } else if let (Some(frame_header), false) =
                            (self.inner.frame_header(), self.processing_frame)
                        {
                            self.processing_frame = true;
                            self.frame_duration = frame_header.duration.unwrap_or(0.0);
                            self.frame_ready = false;
                            // Don't return just because we have frame info, decode it as well.
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

    /// Extract decoded pixels into the provided output buffer.
    ///
    /// The frame must be ready (check with is_frame_ready()) before calling this function.
    /// After successful extraction, the decoder is reset for the next frame.
    pub fn decode_frame(&mut self, output: &mut [u32]) -> Result<usize, Error> {
        if !self.frame_ready {
            return Err(Error::String("Frame is not ready".to_string()));
        }

        let basic_info = self.inner.basic_info().unwrap();
        let pixel_format = self.inner.current_pixel_format().unwrap();

        if pixel_format.color_type == JxlColorType::Grayscale {
            if self.black_index.is_some() {
                return Err(Error::String(
                    "Can't combine grayscale with extra black channel".to_string(),
                ));
            }
            if let Some(alpha_idx) = self.alpha_index {
                let alpha_image = &self.output_images[alpha_idx];
                let gray_image = &self.output_images[0];
                for y in 0..(basic_info.size.1) {
                    let alpha_row = alpha_image.row(y);
                    let gray_row = gray_image.row(y);
                    for x in 0..(basic_info.size.0) {
                        let pixel_idx = y * basic_info.size.0 + x;
                        let gray = u8_from_f32(gray_row[x]) as u32;
                        let a = u8_from_f32(alpha_row[x]) as u32;
                        output[pixel_idx] = (a << 24) | (gray << 16) | (gray << 8) | gray;
                    }
                }
            } else {
                let gray_image = &self.output_images[0];
                for y in 0..(basic_info.size.1) {
                    let gray_row = gray_image.row(y);
                    for x in 0..(basic_info.size.0) {
                        let pixel_idx = y * basic_info.size.0 + x;
                        let gray = u8_from_f32(gray_row[x]) as u32;
                        output[pixel_idx] = (255 << 24) | (gray << 16) | (gray << 8) | gray;
                    }
                }
            }
        } else if let Some(black_idx) = self.black_index {
            let black_image = &self.output_images[black_idx];
            let cmy_image = &self.output_images[0];
            for y in 0..(basic_info.size.1) {
                let black_row = black_image.row(y);
                let cmy_row = cmy_image.row(y);
                for x in 0..(basic_info.size.0) {
                    let pixel_idx = y * basic_info.size.0 + x;
                    let (c, m, y) = (
                        u8_from_f32(cmy_row[x * 3]) as u32,
                        u8_from_f32(cmy_row[x * 3 + 1]) as u32,
                        u8_from_f32(cmy_row[x * 3 + 2]) as u32,
                    );
                    let k = u8_from_f32(black_row[x]) as u32;
                    // TODO: Check if this is actually the order that qcms expects CMYK in.
                    output[pixel_idx] = (c << 24) | (m << 16) | (y << 8) | k;
                }
            }
        } else if let Some(alpha_idx) = self.alpha_index {
            let alpha_image = &self.output_images[alpha_idx];
            let rgb_image = &self.output_images[0];
            for y in 0..(basic_info.size.1) {
                let alpha_row = alpha_image.row(y);
                let rgb_row = rgb_image.row(y);
                for x in 0..(basic_info.size.0) {
                    let pixel_idx = y * basic_info.size.0 + x;
                    let (r, g, b) = (
                        u8_from_f32(rgb_row[x * 3]) as u32,
                        u8_from_f32(rgb_row[x * 3 + 1]) as u32,
                        u8_from_f32(rgb_row[x * 3 + 2]) as u32,
                    );
                    let a = u8_from_f32(alpha_row[x]) as u32;
                    output[pixel_idx] = (a << 24) | (r << 16) | (g << 8) | b;
                }
            }
        } else {
            let rgb_image = &self.output_images[0];
            for y in 0..(basic_info.size.1) {
                let rgb_row = rgb_image.row(y);
                for x in 0..(basic_info.size.0) {
                    let pixel_idx = y * basic_info.size.0 + x;
                    let (r, g, b) = (
                        u8_from_f32(rgb_row[x * 3]) as u32,
                        u8_from_f32(rgb_row[x * 3 + 1]) as u32,
                        u8_from_f32(rgb_row[x * 3 + 2]) as u32,
                    );
                    output[pixel_idx] = (255 << 24) | (r << 16) | (g << 8) | b;
                }
            }
        }
        Ok(basic_info.size.0 * basic_info.size.1)
    }
}

fn u8_from_f32(v: f32) -> u8 {
    (v * 255.0).clamp(0.0, 255.0) as u8
}
