/*!
 * Sitemap
 * Copyright(c) 2011 Eugene Kalinin
 * MIT Licensed
 */
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
export declare const LIMITS: {
    readonly MAX_URL_LENGTH: 2048;
    readonly URL_PROTOCOL_REGEX: RegExp;
    readonly MIN_SITEMAP_ITEM_LIMIT: 1;
    readonly MAX_SITEMAP_ITEM_LIMIT: 50000;
    readonly MAX_VIDEO_TITLE_LENGTH: 100;
    readonly MAX_VIDEO_DESCRIPTION_LENGTH: 2048;
    readonly MAX_VIDEO_CATEGORY_LENGTH: 256;
    readonly MAX_TAGS_PER_VIDEO: 32;
    readonly MAX_NEWS_TITLE_LENGTH: 200;
    readonly MAX_NEWS_NAME_LENGTH: 256;
    readonly MAX_IMAGE_CAPTION_LENGTH: 512;
    readonly MAX_IMAGE_TITLE_LENGTH: 512;
    readonly MAX_IMAGES_PER_URL: 1000;
    readonly MAX_VIDEOS_PER_URL: 100;
    readonly MAX_LINKS_PER_URL: 100;
    readonly MAX_URL_ENTRIES: 50000;
    readonly ISO_DATE_REGEX: RegExp;
    readonly MAX_CUSTOM_NAMESPACES: 20;
    readonly MAX_NAMESPACE_LENGTH: 512;
    readonly MAX_PARSER_ERRORS: 100;
};
/**
 * Default maximum number of items in each sitemap XML file
 * Set below the max to leave room for URLs added during processing
 *
 * @see https://www.sitemaps.org/protocol.html#index
 */
export declare const DEFAULT_SITEMAP_ITEM_LIMIT = 45000;
