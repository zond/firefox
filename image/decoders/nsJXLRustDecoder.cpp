/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*-
 *
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#include "nsJXLRustDecoder.h"

#include "RasterImage.h"
#include "SurfaceFilters.h"
#include "SurfacePipeFactory.h"
#include "mozilla/Vector.h"

using namespace mozilla::gfx;

namespace mozilla {
namespace image {

// Implement the deleter
void nsJXLRustDecoder::JxlRustDecoderDeleter::operator()(
    ::mozilla::JxlRustDecoder* aDecoder) {
  if (aDecoder) {
    jxl_rust_decoder_free(aDecoder);
  }
}

nsJXLRustDecoder::nsJXLRustDecoder(RasterImage* aImage)
    : Decoder(aImage),
      mLexer(Transition::ToUnbuffered(State::FINISHED_JXL_DATA,
                                       State::JXL_DATA, SIZE_MAX),
             Transition::TerminateSuccess()),
      mSize(0, 0) {
}

nsJXLRustDecoder::~nsJXLRustDecoder() {
}

LexerResult nsJXLRustDecoder::DoDecode(SourceBufferIterator& aIterator,
                                       IResumable* aOnResume) {
  MOZ_ASSERT(!HasError(), "Shouldn't call DoDecode after error!");

  // Create Rust decoder on first use
  if (!mRustDecoder) {
    ::mozilla::JxlRustDecoder* decoder = jxl_rust_decoder_new();
    if (!decoder) {
      return LexerResult(TerminalState::FAILURE);
    }
    mRustDecoder.reset(decoder);
  }

  return mLexer.Lex(aIterator, aOnResume,
                    [=](State aState, const char* aData, size_t aLength) {
                      switch (aState) {
                        case State::JXL_DATA:
                          return ReadJXLData(aData, aLength);
                        case State::FINISHED_JXL_DATA:
                          return FinishedJXLData();
                      }
                      MOZ_CRASH("Unknown State");
                    });
}

LexerTransition<nsJXLRustDecoder::State> nsJXLRustDecoder::ReadJXLData(
    const char* aData, size_t aLength) {
  MOZ_ASSERT(mRustDecoder);

  const uint8_t* input = reinterpret_cast<const uint8_t*>(aData);
  size_t length = aLength;

  // If we have buffered data, append new data and use buffer
  if (mBuffer.length() != 0) {
    if (!mBuffer.append(aData, aLength)) {
      return Transition::TerminateFailure();
    }
    input = mBuffer.begin();
    length = mBuffer.length();
  }

  // Process data with Rust decoder
  ::mozilla::JxlRustStatus status = jxl_rust_decoder_process_data(
      mRustDecoder.get(), input, length);

  switch (status) {
    case ::mozilla::JXL_RUST_STATUS_OK: {
      // Check if we have image info yet
      ::mozilla::JxlRustImageInfo info;
      ::mozilla::JxlRustStatus infoStatus = jxl_rust_decoder_get_info(
          mRustDecoder.get(), &info);

      if (infoStatus == ::mozilla::JXL_RUST_STATUS_OK && !HasSize()) {
        mSize = IntSize(info.width, info.height);
        PostSize(info.width, info.height);

        if (IsMetadataDecode()) {
          return Transition::TerminateSuccess();
        }
      }

      // Check if Rust decoder has a frame ready
      if (jxl_rust_decoder_is_frame_ready(mRustDecoder.get())) {
        // Create the output surface
        OrientedIntSize outputSize = OutputSize();

        SurfaceFormat format = SurfaceFormat::OS_RGBX;
        SurfacePipeFlags pipeFlags = SurfacePipeFlags();

        // Create surface pipe
        OrientedIntRect frameRect(OrientedIntPoint(0, 0), outputSize);
        Maybe<SurfacePipe> pipe = SurfacePipeFactory::CreateSurfacePipe(
            this, outputSize, outputSize, frameRect,
            format, format, /* aAnimParams */ Nothing(),
            /* aTransform */ nullptr, pipeFlags);

        if (!pipe) {
          return Transition::TerminateFailure();
        }

        // Allocate buffer for decoded pixels
        Vector<uint32_t> pixelBuffer;
        size_t pixelCount = outputSize.width * outputSize.height;
        if (!pixelBuffer.resize(pixelCount)) {
          return Transition::TerminateFailure();
        }

        // Decode the frame
        size_t pixelsWritten = 0;
        status = jxl_rust_decoder_decode_frame(
            mRustDecoder.get(),
            pixelBuffer.begin(),
            pixelBuffer.length(),
            &pixelsWritten);

        if (status != ::mozilla::JXL_RUST_STATUS_OK || pixelsWritten != pixelCount) {
          return Transition::TerminateFailure();
        }

        // Write decoded pixels to the surface row by row
        uint32_t* currentRow = pixelBuffer.begin();
        for (int32_t y = 0; y < outputSize.height; ++y) {
          WriteState result = pipe->WriteBuffer(currentRow);
          if (result == WriteState::FAILURE) {
            return Transition::TerminateFailure();
          }
          currentRow += outputSize.width;
        }

        if (Maybe<SurfaceInvalidRect> invalidRect = pipe->TakeInvalidRect()) {
          PostInvalidation(invalidRect->mInputSpaceRect,
                           Some(invalidRect->mOutputSpaceRect));
        }

        PostFrameStop();
        PostDecodeDone();
        return Transition::TerminateSuccess();
      }

      // Clear buffer if we used it
      mBuffer.clear();

      // Continue reading more data
      return Transition::ContinueUnbuffered(State::JXL_DATA);
    }

    case ::mozilla::JXL_RUST_STATUS_NEED_MORE_DATA: {
      // Need to buffer data if it wasn't contiguous
      if (mBuffer.length() == 0 && aLength > 0) {
        if (!mBuffer.append(aData, aLength)) {
              return Transition::TerminateFailure();
        }
      }
      return Transition::ContinueUnbuffered(State::JXL_DATA);
    }

    case ::mozilla::JXL_RUST_STATUS_INVALID_DATA:
      return Transition::TerminateFailure();

    case ::mozilla::JXL_RUST_STATUS_ERROR:
      return Transition::TerminateFailure();

    default:
      return Transition::TerminateFailure();
  }
}

LexerTransition<nsJXLRustDecoder::State> nsJXLRustDecoder::FinishedJXLData() {
  MOZ_ASSERT_UNREACHABLE("Should complete decode before reaching end");
  return Transition::TerminateFailure();
}

}  // namespace image
}  // namespace mozilla