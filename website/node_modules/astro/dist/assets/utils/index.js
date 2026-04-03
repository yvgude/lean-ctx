import {
  isRemoteAllowed,
  matchHostname,
  matchPathname,
  matchPattern,
  matchPort,
  matchProtocol
} from "@astrojs/internal-helpers/remote";
import { emitClientAsset } from "./assets.js";
import { isESMImportedImage, isRemoteImage, resolveSrc } from "./imageKind.js";
import { imageMetadata } from "./metadata.js";
import { getOrigQueryParams } from "./queryParams.js";
import { inferRemoteSize } from "./remoteProbe.js";
export {
  emitClientAsset,
  getOrigQueryParams,
  imageMetadata,
  inferRemoteSize,
  isESMImportedImage,
  isRemoteAllowed,
  isRemoteImage,
  matchHostname,
  matchPathname,
  matchPattern,
  matchPort,
  matchProtocol,
  resolveSrc
};
