// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

pub mod decoder;
pub mod xpcom;

// Re-export decoder types for backwards compatibility
pub use decoder::{CachedImageInfo, JxlApiDecoder};

// Re-export XPCOM constructor
pub use xpcom::nsJXLDecoderConstructor;