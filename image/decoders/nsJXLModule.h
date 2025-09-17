/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*-
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#ifndef nsJXLModule_h
#define nsJXLModule_h

#include "nsID.h"

// Implemented in Rust (media/jxl/jxl_xpcom)
extern "C" {
nsresult nsJXLDecoderConstructor(REFNSIID aIID, void** aResult);
}

#endif  // nsJXLModule_h
