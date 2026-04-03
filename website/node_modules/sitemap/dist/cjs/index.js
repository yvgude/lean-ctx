"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __exportStar = (this && this.__exportStar) || function(m, exports) {
    for (var p in m) if (p !== "default" && !Object.prototype.hasOwnProperty.call(exports, p)) __createBinding(exports, m, p);
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.DEFAULT_SITEMAP_ITEM_LIMIT = exports.LIMITS = exports.isAllowDeny = exports.isValidYesNo = exports.isValidChangeFreq = exports.isResolution = exports.isPriceType = exports.validators = exports.validateXSLUrl = exports.validatePublicBasePath = exports.validateLimit = exports.validatePath = exports.validateURL = exports.simpleSitemapAndIndex = exports.IndexObjectStreamToJSON = exports.XMLToSitemapIndexStream = exports.parseSitemapIndex = exports.ObjectStreamToJSON = exports.XMLToSitemapItemStream = exports.parseSitemap = exports.xmlLint = exports.ReadlineStream = exports.normalizeURL = exports.validateSMIOptions = exports.mergeStreams = exports.lineSeparatedURLsToSitemapOptions = exports.SitemapStream = exports.streamToPromise = exports.SitemapAndIndexStream = exports.SitemapIndexStream = exports.IndexTagNames = exports.SitemapItemStream = void 0;
/*!
 * Sitemap
 * Copyright(c) 2011 Eugene Kalinin
 * MIT Licensed
 */
var sitemap_item_stream_js_1 = require("./lib/sitemap-item-stream.js");
Object.defineProperty(exports, "SitemapItemStream", { enumerable: true, get: function () { return sitemap_item_stream_js_1.SitemapItemStream; } });
var sitemap_index_stream_js_1 = require("./lib/sitemap-index-stream.js");
Object.defineProperty(exports, "IndexTagNames", { enumerable: true, get: function () { return sitemap_index_stream_js_1.IndexTagNames; } });
Object.defineProperty(exports, "SitemapIndexStream", { enumerable: true, get: function () { return sitemap_index_stream_js_1.SitemapIndexStream; } });
Object.defineProperty(exports, "SitemapAndIndexStream", { enumerable: true, get: function () { return sitemap_index_stream_js_1.SitemapAndIndexStream; } });
var sitemap_stream_js_1 = require("./lib/sitemap-stream.js");
Object.defineProperty(exports, "streamToPromise", { enumerable: true, get: function () { return sitemap_stream_js_1.streamToPromise; } });
Object.defineProperty(exports, "SitemapStream", { enumerable: true, get: function () { return sitemap_stream_js_1.SitemapStream; } });
__exportStar(require("./lib/errors.js"), exports);
__exportStar(require("./lib/types.js"), exports);
var utils_js_1 = require("./lib/utils.js");
Object.defineProperty(exports, "lineSeparatedURLsToSitemapOptions", { enumerable: true, get: function () { return utils_js_1.lineSeparatedURLsToSitemapOptions; } });
Object.defineProperty(exports, "mergeStreams", { enumerable: true, get: function () { return utils_js_1.mergeStreams; } });
Object.defineProperty(exports, "validateSMIOptions", { enumerable: true, get: function () { return utils_js_1.validateSMIOptions; } });
Object.defineProperty(exports, "normalizeURL", { enumerable: true, get: function () { return utils_js_1.normalizeURL; } });
Object.defineProperty(exports, "ReadlineStream", { enumerable: true, get: function () { return utils_js_1.ReadlineStream; } });
var xmllint_js_1 = require("./lib/xmllint.js");
Object.defineProperty(exports, "xmlLint", { enumerable: true, get: function () { return xmllint_js_1.xmlLint; } });
var sitemap_parser_js_1 = require("./lib/sitemap-parser.js");
Object.defineProperty(exports, "parseSitemap", { enumerable: true, get: function () { return sitemap_parser_js_1.parseSitemap; } });
Object.defineProperty(exports, "XMLToSitemapItemStream", { enumerable: true, get: function () { return sitemap_parser_js_1.XMLToSitemapItemStream; } });
Object.defineProperty(exports, "ObjectStreamToJSON", { enumerable: true, get: function () { return sitemap_parser_js_1.ObjectStreamToJSON; } });
var sitemap_index_parser_js_1 = require("./lib/sitemap-index-parser.js");
Object.defineProperty(exports, "parseSitemapIndex", { enumerable: true, get: function () { return sitemap_index_parser_js_1.parseSitemapIndex; } });
Object.defineProperty(exports, "XMLToSitemapIndexStream", { enumerable: true, get: function () { return sitemap_index_parser_js_1.XMLToSitemapIndexStream; } });
Object.defineProperty(exports, "IndexObjectStreamToJSON", { enumerable: true, get: function () { return sitemap_index_parser_js_1.IndexObjectStreamToJSON; } });
var sitemap_simple_js_1 = require("./lib/sitemap-simple.js");
Object.defineProperty(exports, "simpleSitemapAndIndex", { enumerable: true, get: function () { return sitemap_simple_js_1.simpleSitemapAndIndex; } });
var validation_js_1 = require("./lib/validation.js");
Object.defineProperty(exports, "validateURL", { enumerable: true, get: function () { return validation_js_1.validateURL; } });
Object.defineProperty(exports, "validatePath", { enumerable: true, get: function () { return validation_js_1.validatePath; } });
Object.defineProperty(exports, "validateLimit", { enumerable: true, get: function () { return validation_js_1.validateLimit; } });
Object.defineProperty(exports, "validatePublicBasePath", { enumerable: true, get: function () { return validation_js_1.validatePublicBasePath; } });
Object.defineProperty(exports, "validateXSLUrl", { enumerable: true, get: function () { return validation_js_1.validateXSLUrl; } });
Object.defineProperty(exports, "validators", { enumerable: true, get: function () { return validation_js_1.validators; } });
Object.defineProperty(exports, "isPriceType", { enumerable: true, get: function () { return validation_js_1.isPriceType; } });
Object.defineProperty(exports, "isResolution", { enumerable: true, get: function () { return validation_js_1.isResolution; } });
Object.defineProperty(exports, "isValidChangeFreq", { enumerable: true, get: function () { return validation_js_1.isValidChangeFreq; } });
Object.defineProperty(exports, "isValidYesNo", { enumerable: true, get: function () { return validation_js_1.isValidYesNo; } });
Object.defineProperty(exports, "isAllowDeny", { enumerable: true, get: function () { return validation_js_1.isAllowDeny; } });
var constants_js_1 = require("./lib/constants.js");
Object.defineProperty(exports, "LIMITS", { enumerable: true, get: function () { return constants_js_1.LIMITS; } });
Object.defineProperty(exports, "DEFAULT_SITEMAP_ITEM_LIMIT", { enumerable: true, get: function () { return constants_js_1.DEFAULT_SITEMAP_ITEM_LIMIT; } });
