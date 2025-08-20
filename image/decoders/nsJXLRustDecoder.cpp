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
#include "imgIContainer.h"
#include <iostream>

using namespace mozilla::gfx;

namespace mozilla {
namespace image {

void nsJXLRustDecoder::JxlRustDecoderDeleter::operator()(
    ::mozilla::JxlRustDecoder* aDecoder) {
  if (aDecoder) {
    jxl_rust_decoder_free(aDecoder);
  }
}

nsJXLRustDecoder::nsJXLRustDecoder(RasterImage* aImage)
    : Decoder(aImage),
      mLexer(Transition::ToUnbuffered(State::FINISHED_JXL_DATA, State::JXL_DATA,
                                      SIZE_MAX),
             Transition::TerminateSuccess()) {}

nsJXLRustDecoder::~nsJXLRustDecoder() = default;

LexerResult nsJXLRustDecoder::DoDecode(SourceBufferIterator& aIterator,
                                       IResumable* aOnResume) {
  MOZ_ASSERT(!HasError(), "Shouldn't call DoDecode after error!");

  if (!mRustDecoder) {
    bool isMetadataDecode = IsMetadataDecode();
    ::mozilla::JxlRustDecoder* decoder = jxl_rust_decoder_new(isMetadataDecode);
    if (!decoder) {
      return LexerResult(TerminalState::FAILURE);
    }
    mRustDecoder.reset(decoder);
  }

  return mLexer.Lex(aIterator, aOnResume,
                    [this](State aState, const char* aData, size_t aLength) {
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
  const size_t originalLength = aLength;
  while (true) {
    ::mozilla::JxlRustStatus status = jxl_rust_decoder_process_data(
        mRustDecoder.get(), reinterpret_cast<const uint8_t**>(&aData),
        &aLength);
    switch (status) {
      case ::mozilla::JXL_RUST_STATUS_OK:
        if (!HasSize()) {
          mImageInfo.reset(new ::mozilla::JxlRustImageInfo());
          ::mozilla::JxlRustStatus infoStatus =
              jxl_rust_decoder_get_info(mRustDecoder.get(), mImageInfo.get());
          if (infoStatus != ::mozilla::JXL_RUST_STATUS_OK) {
            return Transition::TerminateFailure();
          }
          PostSize(mImageInfo->width, mImageInfo->height);

          mAnimInfo.reset(new ::mozilla::JxlRustAnimationInfo());
          if (jxl_rust_decoder_get_animation_info(mRustDecoder.get(),
                                                  mAnimInfo.get()) !=
              ::mozilla::JXL_RUST_STATUS_OK) {
            return Transition::TerminateFailure();
          }

          if (IsMetadataDecode()) {
            return Transition::TerminateSuccess();
          }
        }

        if (HasSize() && jxl_rust_decoder_is_frame_ready(mRustDecoder.get())) {
          if (NS_FAILED(ProcessFrame())) {
            return Transition::TerminateFailure();
          }
          // If static, we're done. If animated, yield and signal how much of
          // the buffer we used.
          if (IsFirstFrameDecode() || !mAnimInfo->is_animated ||
              !jxl_rust_decoder_has_more_frames(mRustDecoder.get())) {
            PostDecodeDone();
            PostFrameCount(mFrameIndex);
            return Transition::TerminateSuccess();
          } else {
            return Transition::ContinueUnbufferedAfterYield(
                State::JXL_DATA, originalLength - aLength);
          }
        } else {
          if (aLength == 0) {
            return Transition::ContinueUnbuffered(State::JXL_DATA);
          }
        }
        break;

      case ::mozilla::JXL_RUST_STATUS_NEED_MORE_DATA:
        if (aLength == 0) {
          return Transition::ContinueUnbuffered(State::JXL_DATA);
        }
        break;

      case ::mozilla::JXL_RUST_STATUS_INVALID_DATA:
        return Transition::TerminateFailure();

      case ::mozilla::JXL_RUST_STATUS_ERROR:
        return Transition::TerminateFailure();

      default:
        return Transition::TerminateFailure();
    }
  }
}

LexerTransition<nsJXLRustDecoder::State> nsJXLRustDecoder::FinishedJXLData() {
  MOZ_ASSERT_UNREACHABLE("Should complete decode before reaching end");
  return Transition::TerminateFailure();
}

nsresult nsJXLRustDecoder::ProcessFrame() {
  OrientedIntSize fullSize(mImageInfo->width, mImageInfo->height);
  OrientedIntSize outputSize = OutputSize();
  OrientedIntRect frameRect(OrientedIntPoint(0, 0), fullSize);

  Maybe<AnimationParams> animParams;
  if (mAnimInfo->is_animated) {
    ::mozilla::JxlRustFrameInfo frameInfo;
    ::mozilla::JxlRustStatus status =
        jxl_rust_decoder_get_frame_info(mRustDecoder.get(), &frameInfo);
    if (status != ::mozilla::JXL_RUST_STATUS_OK) {
      return NS_ERROR_FAILURE;
    }
    const FrameTimeout timeout = FrameTimeout::FromRawMilliseconds(50);
    if (mFrameIndex == 0) {
      PostIsAnimated(timeout);
      PostLoopCount(mAnimInfo->num_loops == 0 ? -1 : mAnimInfo->num_loops);
    }
    animParams.emplace(frameRect.ToUnknownRect(), timeout, mFrameIndex,
                       BlendMethod::SOURCE, DisposalMethod::KEEP);
  }

  SurfaceFormat format = SurfaceFormat::OS_RGBA;
  PostHasTransparency();
  SurfacePipeFlags pipeFlags = SurfacePipeFlags();
  // Tell Firefox to premultiply if necessary.
  if (!mImageInfo->alpha_premultiplied) {
    pipeFlags |= SurfacePipeFlags::PREMULTIPLY_ALPHA;
  }

  Maybe<SurfacePipe> pipe = SurfacePipeFactory::CreateSurfacePipe(
      this, fullSize, outputSize, frameRect, format, format, animParams,
      /* aTransform */ nullptr, pipeFlags);
  if (!pipe) {
    return NS_ERROR_FAILURE;
  }

  Vector<uint32_t> pixelBuffer;
  size_t fullPixelCount = fullSize.width * fullSize.height;
  if (!pixelBuffer.resize(fullPixelCount)) {
    return NS_ERROR_OUT_OF_MEMORY;
  }

  size_t pixelsWritten = 0;
  ::mozilla::JxlRustStatus status =
      jxl_rust_decoder_decode_frame(mRustDecoder.get(), pixelBuffer.begin(),
                                    pixelBuffer.length(), &pixelsWritten);

  if (status != ::mozilla::JXL_RUST_STATUS_OK ||
      pixelsWritten != fullPixelCount) {
    return NS_ERROR_FAILURE;
  }

  uint32_t* currentRow = pixelBuffer.begin();
  for (int32_t y = 0; y < fullSize.height; ++y) {
    WriteState result = pipe->WriteBuffer(currentRow);
    if (result == WriteState::FAILURE) {
      return NS_ERROR_FAILURE;
    }
    currentRow += fullSize.width;
  }

  if (Maybe<SurfaceInvalidRect> invalidRect = pipe->TakeInvalidRect()) {
    PostInvalidation(invalidRect->mInputSpaceRect,
                     Some(invalidRect->mOutputSpaceRect));
  }

  PostFrameStop();

  mFrameIndex++;
  return NS_OK;
}

}  // namespace image
}  // namespace mozilla