/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*-
 *
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#ifndef mozilla_image_decoders_nsJXLDecoder_h
#define mozilla_image_decoders_nsJXLDecoder_h

#include "Decoder.h"
#include "SurfacePipe.h"
#include "StreamingLexer.h"
#include "nsCOMPtr.h"
#include "mozilla/AlreadyAddRefed.h"

class nsIJXLDecoder;

struct qcms_profile_deleter {
  void operator()(void* ptr) {
    qcms_profile_release(static_cast<qcms_profile*>(ptr));
  }
};

struct qcms_transform_deleter {
  void operator()(void* ptr) {
    qcms_transform_release(static_cast<qcms_transform*>(ptr));
  }
};

namespace mozilla::image {

class nsJXLDecoder final : public Decoder {
 public:
  ~nsJXLDecoder() override;

  DecoderType GetType() const override { return DecoderType::JXL; }

 protected:
  LexerResult DoDecode(SourceBufferIterator& aIterator,
                       IResumable* aOnResume) override;

 private:
  friend class DecoderFactory;

  // Decoders should only be instantiated via DecoderFactory.
  explicit nsJXLDecoder(RasterImage* aImage);

  std::unique_ptr<qcms_profile, qcms_profile_deleter> mInProfile;
  std::unique_ptr<qcms_transform, qcms_transform_deleter> mTransform;

  enum class State { JXL_DATA, FINISHED_JXL_DATA };

  nsresult ProcessFrame();

  LexerTransition<State> ReadJXLData(const char* aData, size_t aLength);
  LexerTransition<State> FinishedJXLData();

  StreamingLexer<State> mLexer;

  // XPCOM decoder interface
  nsCOMPtr<nsIJXLDecoder> mDecoder;

  // Cached data structures to avoid repeated XPCOM calls
  struct CachedImageInfo {
    uint32_t width;
    uint32_t height;
    bool hasAlpha;
    bool cmyk;
    bool alphaPremultiplied;
  } mCachedImageInfo;

  struct CachedAnimationInfo {
    bool isAnimated;
    uint32_t numLoops;
  } mCachedAnimInfo;

  // Animation state
  uint32_t mFrameIndex = 0;
};

}  // namespace mozilla::image

#endif  // mozilla_image_decoders_nsJXLDecoder_h
