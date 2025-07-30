// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

/// JXL magic signatures
pub const JXL_CODESTREAM_SIGNATURE: &[u8] = &[0xFF, 0x0A];
pub const JXL_CONTAINER_SIGNATURE: &[u8] = &[0x00, 0x00, 0x00, 0x0C, 0x4A, 0x58, 0x4C, 0x20, 0x0D, 0x0A, 0x87, 0x0A];

pub struct JxlRustDecoder {
    pub header_validated: bool,
    pub width: u32,
    pub height: u32,
    accumulated_data: Vec<u8>,
    frame_ready: bool,
}

impl JxlRustDecoder {
    pub fn new() -> Self {
        Self {
            header_validated: false,
            width: 0,
            height: 0,
            accumulated_data: Vec::new(),
            frame_ready: false,
        }
    }

    pub fn process_data(&mut self, data: &[u8]) -> Result<bool, &'static str> {
        // Accumulate data
        self.accumulated_data.extend_from_slice(data);

        // If header is not validated yet, try to validate it
        if !self.header_validated {
            // Check if we have enough data for the smallest signature
            if self.accumulated_data.len() < JXL_CODESTREAM_SIGNATURE.len() {
                return Ok(false); // Need more data
            }

            // Check for codestream signature
            if !self.accumulated_data.starts_with(JXL_CODESTREAM_SIGNATURE) {
                // Also check for container signature if we have enough data
                if self.accumulated_data.len() >= JXL_CONTAINER_SIGNATURE.len() {
                    if !self.accumulated_data.starts_with(JXL_CONTAINER_SIGNATURE) {
                        return Err("Invalid JXL signature");
                    }
                } else {
                    // Not a codestream, but not enough data to check for container
                    return Ok(false); // Need more data
                }
            }

            // In a real decoder, we would parse the header here
            // For now, we just set some dummy values
            self.header_validated = true;
            self.width = 32;
            self.height = 32;
        }

        // In a real decoder, we would check if we have enough data to decode a frame
        // For this placeholder, we say frame is ready once we have validated the header
        // and have at least one more byte beyond the signature (to handle tiny test files)
        let min_size = if self.accumulated_data.starts_with(JXL_CONTAINER_SIGNATURE) {
            JXL_CONTAINER_SIGNATURE.len() + 1
        } else {
            JXL_CODESTREAM_SIGNATURE.len() + 1
        };

        if self.header_validated && self.accumulated_data.len() >= min_size {
            self.frame_ready = true;
        }

        Ok(true)
    }

    pub fn is_frame_ready(&self) -> bool {
        self.frame_ready
    }

    pub fn decode_frame(&mut self, output: &mut [u32]) -> Result<usize, &'static str> {
        if !self.header_validated {
            return Err("Header not validated");
        }

        let pixel_count = (self.width * self.height) as usize;
        if output.len() < pixel_count {
            return Err("Output buffer too small");
        }

        // In a real decoder, we would:
        // 1. Process the accumulated JXL data
        // 2. Decode it into pixels
        // 3. Write to output buffer
        //
        // For now, we just fill with black pixels
        for pixel in output.iter_mut().take(pixel_count) {
            *pixel = 0xFF000000; // Black in RGBX format
        }

        // Mark frame as consumed
        self.frame_ready = false;

        Ok(pixel_count)
    }
}