"use strict";
/*!
 * Sitemap
 * Copyright(c) 2011 Eugene Kalinin
 * MIT Licensed
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.validators = void 0;
exports.isPriceType = isPriceType;
exports.isResolution = isResolution;
exports.isValidChangeFreq = isValidChangeFreq;
exports.isValidYesNo = isValidYesNo;
exports.isAllowDeny = isAllowDeny;
exports.validateURL = validateURL;
exports.validatePath = validatePath;
exports.validatePublicBasePath = validatePublicBasePath;
exports.validateLimit = validateLimit;
exports.validateXSLUrl = validateXSLUrl;
exports.validateSMIOptions = validateSMIOptions;
const errors_js_1 = require("./errors.js");
const types_js_1 = require("./types.js");
const constants_js_1 = require("./constants.js");
const node_path_1 = require("node:path");
/**
 * Validator regular expressions for various sitemap fields
 */
const allowDeny = /^(?:allow|deny)$/;
exports.validators = {
    'price:currency': /^[A-Z]{3}$/,
    'price:type': /^(?:rent|purchase|RENT|PURCHASE)$/,
    'price:resolution': /^(?:HD|hd|sd|SD)$/,
    'platform:relationship': allowDeny,
    'restriction:relationship': allowDeny,
    restriction: /^([A-Z]{2}( +[A-Z]{2})*)?$/,
    platform: /^((web|mobile|tv)( (web|mobile|tv))*)?$/,
    // Language codes: zh-cn, zh-tw, or ISO 639 2-3 letter codes
    language: /^(zh-cn|zh-tw|[a-z]{2,3})$/,
    genres: /^(PressRelease|Satire|Blog|OpEd|Opinion|UserGenerated)(, *(PressRelease|Satire|Blog|OpEd|Opinion|UserGenerated))*$/,
    stock_tickers: /^(\w+:\w+(, *\w+:\w+){0,4})?$/,
};
/**
 * Type guard to check if a string is a valid price type
 */
function isPriceType(pt) {
    return exports.validators['price:type'].test(pt);
}
/**
 * Type guard to check if a string is a valid resolution
 */
function isResolution(res) {
    return exports.validators['price:resolution'].test(res);
}
/**
 * Type guard to check if a string is a valid changefreq value
 */
const CHANGEFREQ = Object.values(types_js_1.EnumChangefreq);
function isValidChangeFreq(freq) {
    return CHANGEFREQ.includes(freq);
}
/**
 * Type guard to check if a string is a valid yes/no value
 */
function isValidYesNo(yn) {
    return /^YES|NO|[Yy]es|[Nn]o$/.test(yn);
}
/**
 * Type guard to check if a string is a valid allow/deny value
 */
function isAllowDeny(ad) {
    return allowDeny.test(ad);
}
/**
 * Validates that a URL is well-formed and meets security requirements
 *
 * Security: This function enforces that URLs use safe protocols (http/https),
 * are within reasonable length limits (2048 chars per sitemaps.org spec),
 * and can be properly parsed. This prevents protocol injection attacks and
 * ensures compliance with sitemap specifications.
 *
 * @param url - The URL to validate
 * @param paramName - The parameter name for error messages
 * @throws {InvalidHostnameError} If the URL is invalid
 */
function validateURL(url, paramName) {
    if (!url || typeof url !== 'string') {
        throw new errors_js_1.InvalidHostnameError(url, `${paramName} must be a non-empty string`);
    }
    if (url.length > constants_js_1.LIMITS.MAX_URL_LENGTH) {
        throw new errors_js_1.InvalidHostnameError(url, `${paramName} exceeds maximum length of ${constants_js_1.LIMITS.MAX_URL_LENGTH} characters`);
    }
    if (!constants_js_1.LIMITS.URL_PROTOCOL_REGEX.test(url)) {
        throw new errors_js_1.InvalidHostnameError(url, `${paramName} must use http:// or https:// protocol`);
    }
    // Validate URL can be parsed
    try {
        new URL(url);
    }
    catch (err) {
        throw new errors_js_1.InvalidHostnameError(url, `${paramName} is not a valid URL: ${err instanceof Error ? err.message : String(err)}`);
    }
}
/**
 * Validates that a path doesn't contain path traversal sequences
 *
 * Security: This function prevents path traversal attacks by detecting
 * any occurrence of '..' in the path, whether it appears as '../', '/..',
 * or standalone. This prevents attackers from accessing files outside
 * the intended directory structure.
 *
 * @param path - The path to validate
 * @param paramName - The parameter name for error messages
 * @throws {InvalidPathError} If the path contains traversal sequences
 */
function validatePath(path, paramName) {
    if (!path || typeof path !== 'string') {
        throw new errors_js_1.InvalidPathError(path, `${paramName} must be a non-empty string`);
    }
    // Reject absolute paths to prevent arbitrary write location when caller input
    // reaches destinationDir (BB-04)
    if ((0, node_path_1.isAbsolute)(path)) {
        throw new errors_js_1.InvalidPathError(path, `${paramName} must be a relative path (absolute paths are not allowed)`);
    }
    // Check for path traversal sequences - must check before and after normalization
    // to catch both Windows-style (\) and Unix-style (/) separators
    if (path.includes('..')) {
        throw new errors_js_1.InvalidPathError(path, `${paramName} contains path traversal sequence (..)`);
    }
    // Additional check after normalization to catch encoded or obfuscated attempts
    const normalizedPath = path.replace(/\\/g, '/');
    const pathComponents = normalizedPath.split('/').filter((p) => p.length > 0);
    if (pathComponents.includes('..')) {
        throw new errors_js_1.InvalidPathError(path, `${paramName} contains path traversal sequence (..)`);
    }
    // Check for null bytes (security issue in some contexts)
    if (path.includes('\0')) {
        throw new errors_js_1.InvalidPathError(path, `${paramName} contains null byte character`);
    }
}
/**
 * Validates that a public base path is safe for URL construction
 *
 * Security: This function prevents path traversal attacks and validates
 * that the path is safe for use in URL construction within sitemap indexes.
 * It checks for '..' sequences, null bytes, and invalid whitespace that
 * could be used to manipulate URL structure or inject malicious content.
 *
 * @param publicBasePath - The public base path to validate
 * @throws {InvalidPublicBasePathError} If the path is invalid
 */
function validatePublicBasePath(publicBasePath) {
    if (!publicBasePath || typeof publicBasePath !== 'string') {
        throw new errors_js_1.InvalidPublicBasePathError(publicBasePath, 'must be a non-empty string');
    }
    // Check for path traversal - check the raw string first
    if (publicBasePath.includes('..')) {
        throw new errors_js_1.InvalidPublicBasePathError(publicBasePath, 'contains path traversal sequence (..)');
    }
    // Additional check for path components after normalization
    const normalizedPath = publicBasePath.replace(/\\/g, '/');
    const pathComponents = normalizedPath.split('/').filter((p) => p.length > 0);
    if (pathComponents.includes('..')) {
        throw new errors_js_1.InvalidPublicBasePathError(publicBasePath, 'contains path traversal sequence (..)');
    }
    // Check for null bytes
    if (publicBasePath.includes('\0')) {
        throw new errors_js_1.InvalidPublicBasePathError(publicBasePath, 'contains null byte character');
    }
    // Check for potentially dangerous characters that could break URL construction
    if (/[\r\n\t]/.test(publicBasePath)) {
        throw new errors_js_1.InvalidPublicBasePathError(publicBasePath, 'contains invalid whitespace characters');
    }
}
/**
 * Validates that a limit is within acceptable range per sitemaps.org spec
 *
 * Security: This function enforces sitemap size limits (1-50,000 URLs per
 * sitemap) as specified by sitemaps.org. This prevents resource exhaustion
 * attacks and ensures compliance with search engine requirements.
 *
 * @param limit - The limit to validate
 * @throws {InvalidLimitError} If the limit is out of range
 */
function validateLimit(limit) {
    if (typeof limit !== 'number' ||
        !Number.isFinite(limit) ||
        Number.isNaN(limit)) {
        throw new errors_js_1.InvalidLimitError(limit);
    }
    if (limit < constants_js_1.LIMITS.MIN_SITEMAP_ITEM_LIMIT ||
        limit > constants_js_1.LIMITS.MAX_SITEMAP_ITEM_LIMIT) {
        throw new errors_js_1.InvalidLimitError(limit);
    }
    // Ensure it's an integer
    if (!Number.isInteger(limit)) {
        throw new errors_js_1.InvalidLimitError(limit);
    }
}
/**
 * Validates that an XSL URL is safe and well-formed
 *
 * Security: This function validates XSL stylesheet URLs to prevent
 * injection attacks. It blocks dangerous protocols and content patterns
 * that could be used for XSS or other attacks. The validation uses
 * case-insensitive matching to catch obfuscated attacks.
 *
 * @param xslUrl - The XSL URL to validate
 * @throws {InvalidXSLUrlError} If the URL is invalid
 */
function validateXSLUrl(xslUrl) {
    if (!xslUrl || typeof xslUrl !== 'string') {
        throw new errors_js_1.InvalidXSLUrlError(xslUrl, 'must be a non-empty string');
    }
    if (xslUrl.length > constants_js_1.LIMITS.MAX_URL_LENGTH) {
        throw new errors_js_1.InvalidXSLUrlError(xslUrl, `exceeds maximum length of ${constants_js_1.LIMITS.MAX_URL_LENGTH} characters`);
    }
    if (!constants_js_1.LIMITS.URL_PROTOCOL_REGEX.test(xslUrl)) {
        throw new errors_js_1.InvalidXSLUrlError(xslUrl, 'must use http:// or https:// protocol');
    }
    // Validate URL can be parsed
    try {
        new URL(xslUrl);
    }
    catch (err) {
        throw new errors_js_1.InvalidXSLUrlError(xslUrl, `is not a valid URL: ${err instanceof Error ? err.message : String(err)}`);
    }
    // Check for potentially dangerous content (case-insensitive)
    const lowerUrl = xslUrl.toLowerCase();
    // Block dangerous HTML/script content
    if (lowerUrl.includes('<script')) {
        throw new errors_js_1.InvalidXSLUrlError(xslUrl, 'contains potentially malicious content (<script tag)');
    }
    // Block dangerous protocols (already checked http/https above, but double-check for encoded variants)
    const dangerousProtocols = [
        'javascript:',
        'data:',
        'vbscript:',
        'file:',
        'about:',
    ];
    for (const protocol of dangerousProtocols) {
        if (lowerUrl.includes(protocol)) {
            throw new errors_js_1.InvalidXSLUrlError(xslUrl, `contains dangerous protocol: ${protocol}`);
        }
    }
    // Check for URL-encoded variants of dangerous patterns
    // %3C = '<', %3E = '>', %3A = ':'
    const encodedPatterns = [
        '%3cscript', // <script
        '%3c%73%63%72%69%70%74', // <script (fully encoded)
        'javascript%3a', // javascript:
        'data%3a', // data:
    ];
    for (const pattern of encodedPatterns) {
        if (lowerUrl.includes(pattern)) {
            throw new errors_js_1.InvalidXSLUrlError(xslUrl, 'contains URL-encoded malicious content');
        }
    }
    // Reject unencoded XML special characters — these must be percent-encoded in
    // valid URLs and could break out of XML attribute context if left raw.
    if (xslUrl.includes('"') || xslUrl.includes('<') || xslUrl.includes('>')) {
        throw new errors_js_1.InvalidXSLUrlError(xslUrl, 'contains unencoded XML special characters (" < >); percent-encode them in the URL');
    }
}
/**
 * Internal helper to validate fields against their validators
 */
function validate(subject, name, url, level) {
    Object.keys(subject).forEach((key) => {
        // eslint-disable-next-line @typescript-eslint/ban-ts-comment
        // @ts-ignore
        const val = subject[key];
        if (exports.validators[key] && !exports.validators[key].test(val)) {
            if (level === types_js_1.ErrorLevel.THROW) {
                throw new errors_js_1.InvalidAttrValue(key, val, exports.validators[key]);
            }
            else {
                console.warn(`${url}: ${name} key ${key} has invalid value: ${val}`);
            }
        }
    });
}
/**
 * Internal helper to handle errors based on error level
 */
function handleError(error, level) {
    if (level === types_js_1.ErrorLevel.THROW) {
        throw error;
    }
    else if (level === types_js_1.ErrorLevel.WARN) {
        console.warn(error.name, error.message);
    }
}
/**
 * Verifies all data passed in will comply with sitemap spec.
 * @param conf Options to validate
 * @param level logging level
 * @param errorHandler error handling func
 */
function validateSMIOptions(conf, level = types_js_1.ErrorLevel.WARN, errorHandler = handleError) {
    if (!conf) {
        throw new errors_js_1.NoConfigError();
    }
    if (level === types_js_1.ErrorLevel.SILENT) {
        return conf;
    }
    const { url, changefreq, priority, news, video } = conf;
    if (!url) {
        errorHandler(new errors_js_1.NoURLError(), level);
    }
    if (changefreq) {
        if (!isValidChangeFreq(changefreq)) {
            errorHandler(new errors_js_1.ChangeFreqInvalidError(url, changefreq), level);
        }
    }
    if (priority) {
        if (!(priority >= 0.0 && priority <= 1.0)) {
            errorHandler(new errors_js_1.PriorityInvalidError(url, priority), level);
        }
    }
    if (news) {
        if (news.access &&
            news.access !== 'Registration' &&
            news.access !== 'Subscription') {
            errorHandler(new errors_js_1.InvalidNewsAccessValue(url, news.access), level);
        }
        if (!news.publication ||
            !news.publication.name ||
            !news.publication.language ||
            !news.publication_date ||
            !news.title) {
            errorHandler(new errors_js_1.InvalidNewsFormat(url), level);
        }
        validate(news, 'news', url, level);
        validate(news.publication, 'publication', url, level);
    }
    if (video) {
        video.forEach((vid) => {
            if (vid.duration !== undefined) {
                if (vid.duration < 0 || vid.duration > 28800) {
                    errorHandler(new errors_js_1.InvalidVideoDuration(url, vid.duration), level);
                }
            }
            if (vid.rating !== undefined && (vid.rating < 0 || vid.rating > 5)) {
                errorHandler(new errors_js_1.InvalidVideoRating(url, vid.title, vid.rating), level);
            }
            if (typeof vid !== 'object' ||
                !vid.thumbnail_loc ||
                !vid.title ||
                !vid.description) {
                // has to be an object and include required categories https://support.google.com/webmasters/answer/80471?hl=en&ref_topic=4581190
                errorHandler(new errors_js_1.InvalidVideoFormat(url), level);
            }
            if (vid.title.length > 100) {
                errorHandler(new errors_js_1.InvalidVideoTitle(url, vid.title.length), level);
            }
            if (vid.description.length > 2048) {
                errorHandler(new errors_js_1.InvalidVideoDescription(url, vid.description.length), level);
            }
            if (vid.view_count !== undefined && vid.view_count < 0) {
                errorHandler(new errors_js_1.InvalidVideoViewCount(url, vid.view_count), level);
            }
            if (vid.tag.length > 32) {
                errorHandler(new errors_js_1.InvalidVideoTagCount(url, vid.tag.length), level);
            }
            if (vid.category !== undefined && vid.category?.length > 256) {
                errorHandler(new errors_js_1.InvalidVideoCategory(url, vid.category.length), level);
            }
            if (vid.family_friendly !== undefined &&
                !isValidYesNo(vid.family_friendly)) {
                errorHandler(new errors_js_1.InvalidVideoFamilyFriendly(url, vid.family_friendly), level);
            }
            if (vid.restriction) {
                if (!exports.validators.restriction.test(vid.restriction)) {
                    errorHandler(new errors_js_1.InvalidVideoRestriction(url, vid.restriction), level);
                }
                if (!vid['restriction:relationship'] ||
                    !isAllowDeny(vid['restriction:relationship'])) {
                    errorHandler(new errors_js_1.InvalidVideoRestrictionRelationship(url, vid['restriction:relationship']), level);
                }
            }
            // TODO price element should be unbounded
            if ((vid.price === '' && vid['price:type'] === undefined) ||
                (vid['price:type'] !== undefined && !isPriceType(vid['price:type']))) {
                errorHandler(new errors_js_1.InvalidVideoPriceType(url, vid['price:type'], vid.price), level);
            }
            if (vid['price:resolution'] !== undefined &&
                !isResolution(vid['price:resolution'])) {
                errorHandler(new errors_js_1.InvalidVideoResolution(url, vid['price:resolution']), level);
            }
            if (vid['price:currency'] !== undefined &&
                !exports.validators['price:currency'].test(vid['price:currency'])) {
                errorHandler(new errors_js_1.InvalidVideoPriceCurrency(url, vid['price:currency']), level);
            }
            validate(vid, 'video', url, level);
        });
    }
    return conf;
}
