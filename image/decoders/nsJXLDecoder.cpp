/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*-
 *
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#include "nsJXLDecoder.h"

#include "AnimationParams.h"
#include "RasterImage.h"
#include "SurfacePipeFactory.h"
#include "mozilla/CheckedInt.h"
#include "mozilla/Vector.h"

using namespace mozilla::gfx;

namespace mozilla::image {

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
    mDecoder.reset(jxl_decoder_new(IsMetadataDecode()));
    if (!mDecoder) {
      return LexerResult(TerminalState::FAILURE);
    }
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
    JxlDecoderStatus decoder_status = jxl_decoder_process_data(
        mDecoder.get(), reinterpret_cast<const uint8_t**>(&aData), &aLength);

    switch (decoder_status) {
      case JxlDecoderStatus::Ok: {
        if (!HasSize()) {
          mCachedBasicInfo = jxl_decoder_get_basic_info(mDecoder.get());
          if (!mCachedBasicInfo.valid) {
            if (aLength == 0) {
              return Transition::ContinueUnbuffered(State::JXL_DATA);
            } else {
              break;
            }
          }

          PostSize(mCachedBasicInfo.width, mCachedBasicInfo.height);

          if (IsMetadataDecode()) {
            return Transition::TerminateSuccess();
          }
        }

        bool frameReady = jxl_decoder_is_frame_ready(mDecoder.get());

        if (HasSize() && frameReady) {
          if (NS_FAILED(ProcessFrame())) {
            return Transition::TerminateFailure();
          }

          bool hasMoreFrames = jxl_decoder_has_more_frames(mDecoder.get());

          if (IsFirstFrameDecode() || !mCachedBasicInfo.is_animated ||
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

      case JxlDecoderStatus::NeedMoreData: {
        if (aLength == 0) {
          return Transition::ContinueUnbuffered(State::JXL_DATA);
        }
        break;
      }

      default:
        return Transition::TerminateFailure();
    }
  }
}

LexerTransition<nsJXLDecoder::State> nsJXLDecoder::FinishedJXLData() {
  MOZ_ASSERT_UNREACHABLE("Read the entire address space?");
  return Transition::TerminateFailure();
}

nsresult nsJXLDecoder::ProcessFrame() {
  OrientedIntSize fullSize(mCachedBasicInfo.width, mCachedBasicInfo.height);
  OrientedIntSize outputSize = OutputSize();
  OrientedIntRect frameRect(OrientedIntPoint(0, 0), fullSize);

  Maybe<AnimationParams> animParams;
  if (mCachedBasicInfo.is_animated) {
    JxlFrameInfo frameInfo = jxl_decoder_get_frame_info(mDecoder.get());
    if (!frameInfo.valid) {
      return NS_ERROR_FAILURE;
    }

    const FrameTimeout timeout =
        FrameTimeout::FromRawMilliseconds(frameInfo.duration_ms);
    if (mFrameIndex == 0) {
      PostIsAnimated(timeout);
      PostLoopCount(mCachedBasicInfo.num_loops == 0
                        ? -1
                        : static_cast<int32_t>(mCachedBasicInfo.num_loops));
    }
    animParams.emplace(frameRect.ToUnknownRect(), timeout, mFrameIndex,
                       BlendMethod::SOURCE, DisposalMethod::KEEP);
  }

  SurfaceFormat inFormat = SurfaceFormat::A8R8G8B8_UINT32;
  SurfaceFormat outFormat = SurfaceFormat::OS_RGBX;
  SurfacePipeFlags pipeFlags = SurfacePipeFlags();

  Maybe<SurfacePipe> pipe = SurfacePipeFactory::CreateSurfacePipe(
      this, fullSize, outputSize, frameRect, inFormat, outFormat, animParams,
      nullptr, pipeFlags);
  if (!pipe) {
    return NS_ERROR_FAILURE;
  }

  Vector<uint32_t> pixelBuffer;
  CheckedInt<size_t> fullPixelCount =
      CheckedInt<size_t>(fullSize.width) * fullSize.height;
  if (!fullPixelCount.isValid()) {
    return NS_ERROR_OUT_OF_MEMORY;
  }
  if (!pixelBuffer.resize(fullPixelCount.value())) {
    return NS_ERROR_OUT_OF_MEMORY;
  }

  size_t pixelsWritten = 0;
  JxlDecoderStatus status =
      jxl_decoder_decode_frame(mDecoder.get(), pixelBuffer.begin(),
                               pixelBuffer.length(), &pixelsWritten);
  if (status != JxlDecoderStatus::Ok) {
    return NS_ERROR_FAILURE;
  }
  if (pixelsWritten != fullPixelCount.value()) {
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

}  // namespace mozilla::image
