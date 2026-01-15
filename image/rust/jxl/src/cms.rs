// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use jxl::api::{JxlCms, JxlCmsTransformer, JxlColorEncoding, JxlColorProfile};
use jxl::error::{Error, Result};
use qcms::{DataType, Intent, Profile, Transform};

pub struct QcmsCms;

impl QcmsCms {
    pub fn new() -> Option<Self> {
        Some(Self)
    }
}

fn get_data_type(profile: &JxlColorProfile) -> DataType {
    match profile {
        JxlColorProfile::Simple(encoding) => match encoding {
            JxlColorEncoding::RgbColorSpace { .. } | JxlColorEncoding::XYB { .. } => DataType::RGB8,
            JxlColorEncoding::GrayscaleColorSpace { .. } => DataType::Gray8,
        },
        JxlColorProfile::Icc(icc) => {
            if icc.len() >= 20 {
                match &icc[16..20] {
                    b"CMYK" => DataType::CMYK,
                    b"GRAY" => DataType::Gray8,
                    _ => DataType::RGB8,
                }
            } else {
                DataType::RGB8
            }
        }
    }
}

fn channels_for_data_type(dt: DataType) -> usize {
    match dt {
        DataType::Gray8 => 1,
        DataType::RGB8 => 3,
        DataType::CMYK => 4,
        _ => 3,
    }
}

impl JxlCms for QcmsCms {
    fn initialize_transforms(
        &self,
        n: usize,
        _max_pixels_per_transform: usize,
        input: JxlColorProfile,
        output: JxlColorProfile,
        _intensity_target: f32,
    ) -> Result<(usize, Vec<Box<dyn JxlCmsTransformer + Send>>)> {
        let in_type = get_data_type(&input);
        let out_type = get_data_type(&output);

        let input_icc = input.as_icc();
        let input_profile =
            Profile::new_from_slice(&input_icc, false).ok_or(Error::InvalidIccStream)?;

        let output_icc = output.as_icc();
        let output_profile =
            Profile::new_from_slice(&output_icc, false).ok_or(Error::InvalidIccStream)?;

        let in_channels = channels_for_data_type(in_type);
        let out_channels = channels_for_data_type(out_type);

        let mut transformers: Vec<Box<dyn JxlCmsTransformer + Send>> = Vec::with_capacity(n);
        for _ in 0..n {
            let transform = Transform::new_to(
                &input_profile,
                &output_profile,
                in_type,
                out_type,
                Intent::Perceptual,
            )
            .ok_or(Error::InvalidIccStream)?;
            transformers.push(Box::new(QcmsTransformer {
                transform,
                in_type,
                in_channels,
                out_channels,
            }));
        }

        Ok((out_channels, transformers))
    }
}

struct QcmsTransformer {
    transform: Transform,
    in_type: DataType,
    in_channels: usize,
    out_channels: usize,
}

impl JxlCmsTransformer for QcmsTransformer {
    fn do_transform(&mut self, input: &[f32], output: &mut [f32]) -> Result<()> {
        let num_pixels = input.len() / self.in_channels;

        let input_u8: Vec<u8> = if self.in_type == DataType::CMYK {
            input.iter().map(|&v| f32_to_u8_inverted(v)).collect()
        } else {
            input.iter().map(|&v| f32_to_u8(v)).collect()
        };

        let mut output_u8 = vec![0u8; num_pixels * self.out_channels];
        self.transform.convert(&input_u8, &mut output_u8);

        for (i, &v) in output_u8.iter().enumerate() {
            output[i] = v as f32 / 255.0;
        }
        Ok(())
    }

    fn do_transform_inplace(&mut self, inout: &mut [f32]) -> Result<()> {
        if self.in_channels != self.out_channels {
            return Err(Error::CmsChannelCountIncrease {
                in_channels: self.in_channels,
                out_channels: self.out_channels,
            });
        }

        let num_pixels = inout.len() / self.in_channels;
        let mut buf = vec![0u8; num_pixels * self.in_channels];

        for (i, &v) in inout.iter().enumerate() {
            buf[i] = f32_to_u8(v);
        }

        self.transform.apply(&mut buf);

        for (i, &v) in buf.iter().enumerate() {
            inout[i] = v as f32 / 255.0;
        }
        Ok(())
    }
}

fn f32_to_u8(v: f32) -> u8 {
    (v * 255.0).clamp(0.0, 255.0) as u8
}

fn f32_to_u8_inverted(v: f32) -> u8 {
    ((1.0 - v) * 255.0).clamp(0.0, 255.0) as u8
}
