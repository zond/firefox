/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*-
 *
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#include "nsJXLDecoder.h"

#include "RasterImage.h"
#include "SurfaceFilters.h"
#include "SurfacePipeFactory.h"
#include "mozilla/Vector.h"
#include "imgIContainer.h"
#include "nsIJXLDecoder.h"
#include "nsComponentManagerUtils.h"
#include <iostream>

using namespace mozilla::gfx;

namespace mozilla {
namespace image {

nsJXLDecoder::nsJXLDecoder(RasterImage* aImage)
    : Decoder(aImage),
      mLexer(Transition::ToUnbuffered(State::FINISHED_JXL_DATA, State::JXL_DATA,
                                      SIZE_MAX),
             Transition::TerminateSuccess()) {}

nsJXLDecoder::~nsJXLDecoder() = default;

LexerResult nsJXLDecoder::DoDecode(SourceBufferIterator& aIterator,
                                   IResumable* aOnResume) {
  MOZ_ASSERT(!HasError(), "Shouldn't call DoDecode after error!");

  if (!mDecoder) {
    nsresult rv;
    nsCOMPtr<nsIJXLDecoder> decoder =
        do_CreateInstance("@mozilla.org/image/jxl-decoder;1", &rv);
    if (NS_FAILED(rv)) {
      return LexerResult(TerminalState::FAILURE);
    }

    rv = decoder->Init(IsMetadataDecode());
    if (NS_FAILED(rv)) {
      return LexerResult(TerminalState::FAILURE);
    }

    mDecoder = decoder;
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

LexerTransition<nsJXLDecoder::State> nsJXLDecoder::ReadJXLData(
    const char* aData, size_t aLength) {
  MOZ_ASSERT(mDecoder);
  const size_t originalLength = aLength;

  while (true) {
    uint16_t decoder_status = 0;
    nsresult rv = mDecoder->ProcessData(
        reinterpret_cast<const uint8_t**>(&aData),
        reinterpret_cast<uint32_t*>(&aLength), &decoder_status);
    if (NS_FAILED(rv)) {
      return Transition::TerminateFailure();
    }

    switch (decoder_status) {
      case nsIJXLDecoderStatus::STATUS_OK: {
        if (!HasSize()) {
          nsCOMPtr<nsIJXLImageInfo> imageInfo;
          rv = mDecoder->GetImageInfo(getter_AddRefs(imageInfo));
          if (NS_FAILED(rv)) {
            if (aLength == 0) {
              return Transition::ContinueUnbuffered(State::JXL_DATA);
            } else {
              break;
            }
          }

          imageInfo->GetWidth(&mCachedImageInfo.width);
          imageInfo->GetHeight(&mCachedImageInfo.height);
          imageInfo->GetHasAlpha(&mCachedImageInfo.hasAlpha);
          imageInfo->GetAlphaPremultiplied(
              &mCachedImageInfo.alphaPremultiplied);

          PostSize(mCachedImageInfo.width, mCachedImageInfo.height);
          if (mCachedImageInfo.hasAlpha) {
            PostHasTransparency();
          }

          // Get animation info
          nsCOMPtr<nsIJXLAnimationInfo> animInfo;
          rv = mDecoder->GetAnimationInfo(getter_AddRefs(animInfo));
          if (NS_FAILED(rv)) {
            return Transition::TerminateFailure();
          }

          animInfo->GetIsAnimated(&mCachedAnimInfo.isAnimated);
          animInfo->GetNumLoops(&mCachedAnimInfo.numLoops);

          if (IsMetadataDecode()) {
            return Transition::TerminateSuccess();
          }
        }

        bool frameReady = false;
        mDecoder->IsFrameReady(&frameReady);

        if (HasSize() && frameReady) {
          if (NS_FAILED(ProcessFrame())) {
            return Transition::TerminateFailure();
          }

          bool hasMoreFrames = false;
          mDecoder->HasMoreFrames(&hasMoreFrames);

          // If static, we're done. If animated, yield and signal how much of
          // the buffer we used.
          if (IsFirstFrameDecode() || !mCachedAnimInfo.isAnimated ||
              !hasMoreFrames) {
            PostDecodeDone();
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
      }

      case nsIJXLDecoderStatus::STATUS_NEED_MORE_DATA: {
        if (aLength == 0) {
          return Transition::ContinueUnbuffered(State::JXL_DATA);
        }
        break;
      }

      case nsIJXLDecoderStatus::STATUS_INVALID_DATA:
        return Transition::TerminateFailure();

      case nsIJXLDecoderStatus::STATUS_ERROR:
        return Transition::TerminateFailure();

      default:
        return Transition::TerminateFailure();
    }
  }
}

LexerTransition<nsJXLDecoder::State> nsJXLDecoder::FinishedJXLData() {
  MOZ_ASSERT_UNREACHABLE("Should complete decode before reaching end");
  return Transition::TerminateFailure();
}

nsresult nsJXLDecoder::ProcessFrame() {
  OrientedIntSize fullSize(mCachedImageInfo.width, mCachedImageInfo.height);
  OrientedIntSize outputSize = OutputSize();
  OrientedIntRect frameRect(OrientedIntPoint(0, 0), fullSize);

  Maybe<AnimationParams> animParams;
  if (mCachedAnimInfo.isAnimated) {
    nsCOMPtr<nsIJXLFrameInfo> frameInfo;
    nsresult rv = mDecoder->GetFrameInfo(getter_AddRefs(frameInfo));
    if (NS_FAILED(rv)) {
      return NS_ERROR_FAILURE;
    }

    double durationMs;
    frameInfo->GetDurationMs(&durationMs);

    const FrameTimeout timeout = FrameTimeout::FromRawMilliseconds(durationMs);
    if (mFrameIndex == 0) {
      PostIsAnimated(timeout);
      PostLoopCount(mCachedAnimInfo.numLoops == 0 ? -1
                                                  : mCachedAnimInfo.numLoops);
    }
    animParams.emplace(frameRect.ToUnknownRect(), timeout, mFrameIndex,
                       BlendMethod::SOURCE, DisposalMethod::KEEP);
  }

  SurfaceFormat format;
  SurfacePipeFlags pipeFlags = SurfacePipeFlags();
  if (mCachedImageInfo.hasAlpha) {
    format = SurfaceFormat::OS_RGBA;
    // Tell Firefox to premultiply if necessary.
    if (!mCachedImageInfo.alphaPremultiplied) {
      pipeFlags |= SurfacePipeFlags::PREMULTIPLY_ALPHA;
    }
  } else {
    format = SurfaceFormat::OS_RGBX;
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
  uint16_t retval = 0;
  nsresult rv = mDecoder->DecodeFrame(
      pixelBuffer.begin(), pixelBuffer.length(),
      reinterpret_cast<uint32_t*>(&pixelsWritten), &retval);
  if (NS_FAILED(rv)) {
    return NS_ERROR_FAILURE;
  }
  if (retval != nsIJXLDecoderStatus::STATUS_OK) {
    return NS_ERROR_FAILURE;
  }
  if (pixelsWritten != fullPixelCount) {
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
