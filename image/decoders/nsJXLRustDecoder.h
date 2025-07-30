/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*-
 *
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#ifndef mozilla_image_decoders_nsJXLRustDecoder_h
#define mozilla_image_decoders_nsJXLRustDecoder_h

#include "Decoder.h"
#include "SurfacePipe.h"
#include "StreamingLexer.h"

// Include the generated header to get the types
extern "C" {
#include "jxl_rust.h"
}

namespace mozilla::image {
class RasterImage;

class nsJXLRustDecoder final : public Decoder {
 public:
  virtual ~nsJXLRustDecoder();

  DecoderType GetType() const override { return DecoderType::JXL; }
  IntSize Size() const { return mSize; }

 protected:
  LexerResult DoDecode(SourceBufferIterator& aIterator,
                       IResumable* aOnResume) override;

 private:
  friend class DecoderFactory;

  // Decoders should only be instantiated via DecoderFactory.
  explicit nsJXLRustDecoder(RasterImage* aImage);

  enum class State { JXL_DATA, FINISHED_JXL_DATA };

  LexerTransition<State> ReadJXLData(const char* aData, size_t aLength);
  LexerTransition<State> FinishedJXLData();

  StreamingLexer<State> mLexer;
  IntSize mSize;

  // Buffer to accumulate data when non-contiguous
  Vector<uint8_t> mBuffer;

  // Opaque pointer to Rust decoder
  struct JxlRustDecoderDeleter {
    void operator()(::mozilla::JxlRustDecoder* aDecoder);
  };
  UniquePtr<::mozilla::JxlRustDecoder, JxlRustDecoderDeleter> mRustDecoder;
};

}  // namespace mozilla::image

#endif  // mozilla_image_decoders_nsJXLRustDecoder_h