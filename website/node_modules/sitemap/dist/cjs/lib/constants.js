"use strict";
/*!
 * Sitemap
 * Copyright(c) 2011 Eugene Kalinin
 * MIT Licensed
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.DEFAULT_SITEMAP_ITEM_LIMIT = exports.LIMITS = void 0;
/**
 * Shared constants used across the sitemap library
 * This file serves as a single source of truth for limits and validation patterns
 */
/**
 * Security limits for sitemap generation and parsing
 *
 * These limits are based on:
 * - sitemaps.org protocol specification
 * - Security best practices to prevent DoS and injection attacks
 * - Google's sitemap extension specifications
 *
 * @see https://www.sitemaps.org/protocol.html
 * @see https://developers.google.com/search/docs/advanced/sitemaps/build-sitemap
 */
exports.LIMITS = {
    // URL constraints per sitemaps.org spec
    MAX_URL_LENGTH: 2048,
    URL_PROTOCOL_REGEX: /^https?:\/\//i,
    // Sitemap size limits per sitemaps.org spec
    MIN_SITEMAP_ITEM_LIMIT: 1,
    MAX_SITEMAP_ITEM_LIMIT: 50000,
    // Video field length constraints per Google spec
    MAX_VIDEO_TITLE_LENGTH: 100,
    MAX_VIDEO_DESCRIPTION_LENGTH: 2048,
    MAX_VIDEO_CATEGORY_LENGTH: 256,
    MAX_TAGS_PER_VIDEO: 32,
    // News field length constraints per Google spec
    MAX_NEWS_TITLE_LENGTH: 200,
    MAX_NEWS_NAME_LENGTH: 256,
    // Image field length constraints per Google spec
    MAX_IMAGE_CAPTION_LENGTH: 512,
    MAX_IMAGE_TITLE_LENGTH: 512,
    // Limits on number of items per URL entry
    MAX_IMAGES_PER_URL: 1000,
    MAX_VIDEOS_PER_URL: 100,
    MAX_LINKS_PER_URL: 100,
    // Total entries in a sitemap
    MAX_URL_ENTRIES: 50000,
    // Date validation - ISO 8601 / W3C format
    ISO_DATE_REGEX: /^\d{4}-\d{2}-\d{2}(T\d{2}:\d{2}:\d{2}(\.\d{3})?([+-]\d{2}:\d{2}|Z)?)?$/,
    // Custom namespace limits to prevent DoS
    MAX_CUSTOM_NAMESPACES: 20,
    MAX_NAMESPACE_LENGTH: 512,
    // Cap on stored parser errors to prevent memory DoS (BB-03)
    // Errors beyond this limit are counted in errorCount but not retained as objects
    MAX_PARSER_ERRORS: 100,
};
/**
 * Default maximum number of items in each sitemap XML file
 * Set below the max to leave room for URLs added during processing
 *
 * @see https://www.sitemaps.org/protocol.html#index
 */
exports.DEFAULT_SITEMAP_ITEM_LIMIT = 45000;
