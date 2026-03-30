/*!
 * Sitemap
 * Copyright(c) 2011 Eugene Kalinin
 * MIT Licensed
 */
import { InvalidPathError, InvalidHostnameError, InvalidLimitError, InvalidPublicBasePathError, InvalidXSLUrlError, ChangeFreqInvalidError, InvalidAttrValue, InvalidNewsAccessValue, InvalidNewsFormat, InvalidVideoDescription, InvalidVideoDuration, InvalidVideoFormat, InvalidVideoRating, NoURLError, NoConfigError, PriorityInvalidError, InvalidVideoTitle, InvalidVideoViewCount, InvalidVideoTagCount, InvalidVideoCategory, InvalidVideoFamilyFriendly, InvalidVideoRestriction, InvalidVideoRestrictionRelationship, InvalidVideoPriceType, InvalidVideoResolution, InvalidVideoPriceCurrency, } from './errors.js';
import { ErrorLevel, EnumChangefreq, } from './types.js';
import { LIMITS } from './constants.js';
import { isAbsolute } from 'node:path';
/**
 * Validator regular expressions for various sitemap fields
 */
const allowDeny = /^(?:allow|deny)$/;
export const validators = {
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
export function isPriceType(pt) {
    return validators['price:type'].test(pt);
}
/**
 * Type guard to check if a string is a valid resolution
 */
export function isResolution(res) {
    return validators['price:resolution'].test(res);
}
/**
 * Type guard to check if a string is a valid changefreq value
 */
const CHANGEFREQ = Object.values(EnumChangefreq);
export function isValidChangeFreq(freq) {
    return CHANGEFREQ.includes(freq);
}
/**
 * Type guard to check if a string is a valid yes/no value
 */
export function isValidYesNo(yn) {
    return /^YES|NO|[Yy]es|[Nn]o$/.test(yn);
}
/**
 * Type guard to check if a string is a valid allow/deny value
 */
export function isAllowDeny(ad) {
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
export function validateURL(url, paramName) {
    if (!url || typeof url !== 'string') {
        throw new InvalidHostnameError(url, `${paramName} must be a non-empty string`);
    }
    if (url.length > LIMITS.MAX_URL_LENGTH) {
        throw new InvalidHostnameError(url, `${paramName} exceeds maximum length of ${LIMITS.MAX_URL_LENGTH} characters`);
    }
    if (!LIMITS.URL_PROTOCOL_REGEX.test(url)) {
        throw new InvalidHostnameError(url, `${paramName} must use http:// or https:// protocol`);
    }
    // Validate URL can be parsed
    try {
        new URL(url);
    }
    catch (err) {
        throw new InvalidHostnameError(url, `${paramName} is not a valid URL: ${err instanceof Error ? err.message : String(err)}`);
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
export function validatePath(path, paramName) {
    if (!path || typeof path !== 'string') {
        throw new InvalidPathError(path, `${paramName} must be a non-empty string`);
    }
    // Reject absolute paths to prevent arbitrary write location when caller input
    // reaches destinationDir (BB-04)
    if (isAbsolute(path)) {
        throw new InvalidPathError(path, `${paramName} must be a relative path (absolute paths are not allowed)`);
    }
    // Check for path traversal sequences - must check before and after normalization
    // to catch both Windows-style (\) and Unix-style (/) separators
    if (path.includes('..')) {
        throw new InvalidPathError(path, `${paramName} contains path traversal sequence (..)`);
    }
    // Additional check after normalization to catch encoded or obfuscated attempts
    const normalizedPath = path.replace(/\\/g, '/');
    const pathComponents = normalizedPath.split('/').filter((p) => p.length > 0);
    if (pathComponents.includes('..')) {
        throw new InvalidPathError(path, `${paramName} contains path traversal sequence (..)`);
    }
    // Check for null bytes (security issue in some contexts)
    if (path.includes('\0')) {
        throw new InvalidPathError(path, `${paramName} contains null byte character`);
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
export function validatePublicBasePath(publicBasePath) {
    if (!publicBasePath || typeof publicBasePath !== 'string') {
        throw new InvalidPublicBasePathError(publicBasePath, 'must be a non-empty string');
    }
    // Check for path traversal - check the raw string first
    if (publicBasePath.includes('..')) {
        throw new InvalidPublicBasePathError(publicBasePath, 'contains path traversal sequence (..)');
    }
    // Additional check for path components after normalization
    const normalizedPath = publicBasePath.replace(/\\/g, '/');
    const pathComponents = normalizedPath.split('/').filter((p) => p.length > 0);
    if (pathComponents.includes('..')) {
        throw new InvalidPublicBasePathError(publicBasePath, 'contains path traversal sequence (..)');
    }
    // Check for null bytes
    if (publicBasePath.includes('\0')) {
        throw new InvalidPublicBasePathError(publicBasePath, 'contains null byte character');
    }
    // Check for potentially dangerous characters that could break URL construction
    if (/[\r\n\t]/.test(publicBasePath)) {
        throw new InvalidPublicBasePathError(publicBasePath, 'contains invalid whitespace characters');
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
export function validateLimit(limit) {
    if (typeof limit !== 'number' ||
        !Number.isFinite(limit) ||
        Number.isNaN(limit)) {
        throw new InvalidLimitError(limit);
    }
    if (limit < LIMITS.MIN_SITEMAP_ITEM_LIMIT ||
        limit > LIMITS.MAX_SITEMAP_ITEM_LIMIT) {
        throw new InvalidLimitError(limit);
    }
    // Ensure it's an integer
    if (!Number.isInteger(limit)) {
        throw new InvalidLimitError(limit);
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
export function validateXSLUrl(xslUrl) {
    if (!xslUrl || typeof xslUrl !== 'string') {
        throw new InvalidXSLUrlError(xslUrl, 'must be a non-empty string');
    }
    if (xslUrl.length > LIMITS.MAX_URL_LENGTH) {
        throw new InvalidXSLUrlError(xslUrl, `exceeds maximum length of ${LIMITS.MAX_URL_LENGTH} characters`);
    }
    if (!LIMITS.URL_PROTOCOL_REGEX.test(xslUrl)) {
        throw new InvalidXSLUrlError(xslUrl, 'must use http:// or https:// protocol');
    }
    // Validate URL can be parsed
    try {
        new URL(xslUrl);
    }
    catch (err) {
        throw new InvalidXSLUrlError(xslUrl, `is not a valid URL: ${err instanceof Error ? err.message : String(err)}`);
    }
    // Check for potentially dangerous content (case-insensitive)
    const lowerUrl = xslUrl.toLowerCase();
    // Block dangerous HTML/script content
    if (lowerUrl.includes('<script')) {
        throw new InvalidXSLUrlError(xslUrl, 'contains potentially malicious content (<script tag)');
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
            throw new InvalidXSLUrlError(xslUrl, `contains dangerous protocol: ${protocol}`);
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
            throw new InvalidXSLUrlError(xslUrl, 'contains URL-encoded malicious content');
        }
    }
    // Reject unencoded XML special characters — these must be percent-encoded in
    // valid URLs and could break out of XML attribute context if left raw.
    if (xslUrl.includes('"') || xslUrl.includes('<') || xslUrl.includes('>')) {
        throw new InvalidXSLUrlError(xslUrl, 'contains unencoded XML special characters (" < >); percent-encode them in the URL');
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
        if (validators[key] && !validators[key].test(val)) {
            if (level === ErrorLevel.THROW) {
                throw new InvalidAttrValue(key, val, validators[key]);
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
    if (level === ErrorLevel.THROW) {
        throw error;
    }
    else if (level === ErrorLevel.WARN) {
        console.warn(error.name, error.message);
    }
}
/**
 * Verifies all data passed in will comply with sitemap spec.
 * @param conf Options to validate
 * @param level logging level
 * @param errorHandler error handling func
 */
export function validateSMIOptions(conf, level = ErrorLevel.WARN, errorHandler = handleError) {
    if (!conf) {
        throw new NoConfigError();
    }
    if (level === ErrorLevel.SILENT) {
        return conf;
    }
    const { url, changefreq, priority, news, video } = conf;
    if (!url) {
        errorHandler(new NoURLError(), level);
    }
    if (changefreq) {
        if (!isValidChangeFreq(changefreq)) {
            errorHandler(new ChangeFreqInvalidError(url, changefreq), level);
        }
    }
    if (priority) {
        if (!(priority >= 0.0 && priority <= 1.0)) {
            errorHandler(new PriorityInvalidError(url, priority), level);
        }
    }
    if (news) {
        if (news.access &&
            news.access !== 'Registration' &&
            news.access !== 'Subscription') {
            errorHandler(new InvalidNewsAccessValue(url, news.access), level);
        }
        if (!news.publication ||
            !news.publication.name ||
            !news.publication.language ||
            !news.publication_date ||
            !news.title) {
            errorHandler(new InvalidNewsFormat(url), level);
        }
        validate(news, 'news', url, level);
        validate(news.publication, 'publication', url, level);
    }
    if (video) {
        video.forEach((vid) => {
            if (vid.duration !== undefined) {
                if (vid.duration < 0 || vid.duration > 28800) {
                    errorHandler(new InvalidVideoDuration(url, vid.duration), level);
                }
            }
            if (vid.rating !== undefined && (vid.rating < 0 || vid.rating > 5)) {
                errorHandler(new InvalidVideoRating(url, vid.title, vid.rating), level);
            }
            if (typeof vid !== 'object' ||
                !vid.thumbnail_loc ||
                !vid.title ||
                !vid.description) {
                // has to be an object and include required categories https://support.google.com/webmasters/answer/80471?hl=en&ref_topic=4581190
                errorHandler(new InvalidVideoFormat(url), level);
            }
            if (vid.title.length > 100) {
                errorHandler(new InvalidVideoTitle(url, vid.title.length), level);
            }
            if (vid.description.length > 2048) {
                errorHandler(new InvalidVideoDescription(url, vid.description.length), level);
            }
            if (vid.view_count !== undefined && vid.view_count < 0) {
                errorHandler(new InvalidVideoViewCount(url, vid.view_count), level);
            }
            if (vid.tag.length > 32) {
                errorHandler(new InvalidVideoTagCount(url, vid.tag.length), level);
            }
            if (vid.category !== undefined && vid.category?.length > 256) {
                errorHandler(new InvalidVideoCategory(url, vid.category.length), level);
            }
            if (vid.family_friendly !== undefined &&
                !isValidYesNo(vid.family_friendly)) {
                errorHandler(new InvalidVideoFamilyFriendly(url, vid.family_friendly), level);
            }
            if (vid.restriction) {
                if (!validators.restriction.test(vid.restriction)) {
                    errorHandler(new InvalidVideoRestriction(url, vid.restriction), level);
                }
                if (!vid['restriction:relationship'] ||
                    !isAllowDeny(vid['restriction:relationship'])) {
                    errorHandler(new InvalidVideoRestrictionRelationship(url, vid['restriction:relationship']), level);
                }
            }
            // TODO price element should be unbounded
            if ((vid.price === '' && vid['price:type'] === undefined) ||
                (vid['price:type'] !== undefined && !isPriceType(vid['price:type']))) {
                errorHandler(new InvalidVideoPriceType(url, vid['price:type'], vid.price), level);
            }
            if (vid['price:resolution'] !== undefined &&
                !isResolution(vid['price:resolution'])) {
                errorHandler(new InvalidVideoResolution(url, vid['price:resolution']), level);
            }
            if (vid['price:currency'] !== undefined &&
                !validators['price:currency'].test(vid['price:currency'])) {
                errorHandler(new InvalidVideoPriceCurrency(url, vid['price:currency']), level);
            }
            validate(vid, 'video', url, level);
        });
    }
    return conf;
}
