// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use jxl::api::{
    JxlBitstreamInput, JxlColorType, JxlDecoderInner, JxlDecoderOptions, JxlOutputBuffer,
    ProcessingResult,
};
use jxl::headers::extra_channels::ExtraChannel;

pub struct JxlApiDecoder {
    pub inner: JxlDecoderInner,

    // True if Firefox only wanted metadata.
    metadata_only: bool,

    // Destination for API rendering of pixels
    output_vecs: Vec<Vec<u8>>,
    alpha_index: Option<usize>,
    black_index: Option<usize>,

    // True if we are processing frame content
    processing_frame: bool,

    // Signal for frame ready to render
    pub frame_ready: bool,

    // Cached during frame processing to make available after the frame is cleaned up
    pub frame_duration: f64,
}

fn u8_from_f32_u8s(v: &[u8]) -> u8 {
    f32::from_ne_bytes(v.try_into().unwrap()).clamp(0.0, 255.0) as u8
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
            inner: JxlDecoderInner::new(JxlDecoderOptions::default(), None),
            metadata_only,
            output_vecs: vec![],
            alpha_index: None,
            black_index: None,
            processing_frame: false,
            frame_ready: false,
            frame_duration: 0.0,
        }
    }

    pub fn process_data(&mut self, data: &mut impl JxlBitstreamInput) -> Result<bool, Error> {
        loop {
            let mut output_bufs: Option<Vec<JxlOutputBuffer<'_>>> = if let (
                Some(basic_info),
                true,
            ) =
                (self.inner.basic_info(), self.processing_frame)
            {
                // TODO: Remove this when jxl-rs is updated to a version that does this internally.
                let (_width, height) = if basic_info.orientation.is_transposing() {
                    (basic_info.size.1, basic_info.size.0)
                } else {
                    (basic_info.size.0, basic_info.size.1)
                };
                Some(
                    self.output_vecs
                        .iter_mut()
                        .map(|v| {
                            let len = v.len();
                            JxlOutputBuffer::new(v, height, len / height)
                        })
                        .collect(),
                )
            } else {
                None
            };
            match self
                .inner
                .process(data, output_bufs.as_mut().map(|v| v.as_mut_slice()))?
            {
                ProcessingResult::Complete { result: _ } => {
                    if let (Some(basic_info), Some(pixel_format)) =
                        (self.inner.basic_info(), self.inner.current_pixel_format())
                    {
                        // TODO: Remove this when jxl-rs is updated to a version that does this internally.
                        let (width, height) = if basic_info.orientation.is_transposing() {
                            (basic_info.size.1, basic_info.size.0)
                        } else {
                            (basic_info.size.0, basic_info.size.1)
                        };
                        if self.metadata_only {
                            // Return after image metadata when that's the only thing we wanted.
                            return Ok(true);
                        }

                        if self.output_vecs.is_empty() {
                            // TODO(zond): This is what the jxl-rs API actually does today - might have to change
                            // it if the API becomes cleverer.
                            let channels = if pixel_format.color_type == JxlColorType::Grayscale {
                                1
                            } else {
                                3
                            };
                            let bytes_per_pixel = channels * 4;
                            let num_pixels = height * width;
                            self.output_vecs = vec![vec![0; num_pixels * bytes_per_pixel]];
                            for ec in basic_info.extra_channels.iter() {
                                match ec.ec_type {
                                    ExtraChannel::Alpha => {
                                        self.alpha_index = Some(self.output_vecs.len());
                                        self.output_vecs.push(vec![0; num_pixels * 4]);
                                    }
                                    ExtraChannel::Black => {
                                        self.black_index = Some(self.output_vecs.len());
                                        self.output_vecs.push(vec![0; num_pixels * 4]);
                                    }
                                    _ => {
                                        return Err(Error::String(format!(
                                            "{:?} is not black or alpha",
                                            ec.ec_type
                                        )));
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
                    if data.available_bytes()? == 0 {
                        return Ok(false);
                    }
                }
            }
        }
    }

    /// Extract decoded pixels into the provided output buffer.
    ///
    /// The frame must be ready (check with is_frame_ready()) before calling this function.
    /// After successful extraction, the decoder is reset for the next frame.
    pub fn decode_frame(&mut self, output: &mut [u32]) -> Result<usize, Error> {
        let basic_info = self.inner.basic_info().unwrap();
        let pixel_format = self.inner.current_pixel_format().unwrap();

        // TODO: Remove this when jxl-rs is updated to a version that does this internally.
        let (width, height) = if basic_info.orientation.is_transposing() {
            (basic_info.size.1, basic_info.size.0)
        } else {
            (basic_info.size.0, basic_info.size.1)
        };

        if pixel_format.color_type == JxlColorType::Grayscale {
            if self.black_index.is_some() {
                return Err(Error::String(
                    "Can't combine grayscale with extra black channel".to_string(),
                ));
            }
            if let Some(alpha_idx) = self.alpha_index {
                let alpha = &self.output_vecs[alpha_idx as usize];
                for y in 0..(height) {
                    for x in 0..(width) {
                        let pixel_idx = y * width + x;
                        let gray =
                            u8_from_f32_u8s(&self.output_vecs[0][pixel_idx * 4..pixel_idx * 4 + 4])
                                as u32;
                        let a = u8_from_f32_u8s(&alpha[pixel_idx * 4..pixel_idx * 4 + 4]) as u32;
                        output[pixel_idx] = (a << 24) | (gray << 16) | (gray << 8) | gray;
                    }
                }
            } else {
                for y in 0..(height) {
                    for x in 0..(width) {
                        let pixel_idx = y * width + x;
                        let gray =
                            u8_from_f32_u8s(&self.output_vecs[0][pixel_idx * 4..pixel_idx * 4 + 4])
                                as u32;
                        output[pixel_idx] = (255 << 24) | (gray << 16) | (gray << 8) | gray;
                    }
                }
            }
        } else {
            if let Some(black_idx) = self.black_index {
                let black = &self.output_vecs[black_idx as usize];
                for y in 0..(height) {
                    for x in 0..(width) {
                        let pixel_idx = y * width + x;
                        let (c, m, y) = (
                            u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 12..pixel_idx * 12 + 4],
                            ) as u32,
                            u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 12 + 4..pixel_idx * 12 + 8],
                            ) as u32,
                            u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 12 + 8..pixel_idx * 12 + 12],
                            ) as u32,
                        );
                        let k = u8_from_f32_u8s(&black[pixel_idx * 4..pixel_idx * 4 + 4]) as u32;
                        // TODO: Check if this is actually the order that qcms expects CMYK in.
                        output[pixel_idx] = (c << 24) | (m << 16) | (y << 8) | k;
                    }
                }
            } else if let Some(alpha_idx) = self.alpha_index {
                let alpha = &self.output_vecs[alpha_idx as usize];
                for y in 0..(height) {
                    for x in 0..(width) {
                        let pixel_idx = y * width + x;
                        let (r, g, b) = (
                            u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 12..pixel_idx * 12 + 4],
                            ) as u32,
                            u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 12 + 4..pixel_idx * 12 + 8],
                            ) as u32,
                            u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 12 + 8..pixel_idx * 12 + 12],
                            ) as u32,
                        );
                        let a = u8_from_f32_u8s(&alpha[pixel_idx * 4..pixel_idx * 4 + 4]) as u32;
                        output[pixel_idx] = (a << 24) | (r << 16) | (g << 8) | b;
                    }
                }
            } else {
                for y in 0..(height) {
                    for x in 0..(width) {
                        let pixel_idx = y * width + x;
                        let (r, g, b) = (
                            u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 12..pixel_idx * 12 + 4],
                            ) as u32,
                            u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 12 + 4..pixel_idx * 12 + 8],
                            ) as u32,
                            u8_from_f32_u8s(
                                &self.output_vecs[0][pixel_idx * 12 + 8..pixel_idx * 12 + 12],
                            ) as u32,
                        );
                        output[pixel_idx] = (255 << 24) | (r << 16) | (g << 8) | b;
                    }
                }
            }
        }
        Ok(width * height)
    }
}
