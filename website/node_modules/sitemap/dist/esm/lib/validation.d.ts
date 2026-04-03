/*!
 * Sitemap
 * Copyright(c) 2011 Eugene Kalinin
 * MIT Licensed
 */
import { SitemapItem, ErrorLevel, EnumChangefreq, EnumYesNo, EnumAllowDeny, PriceType, Resolution, ErrorHandler } from './types.js';
export declare const validators: {
    [index: string]: RegExp;
};
/**
 * Type guard to check if a string is a valid price type
 */
export declare function isPriceType(pt: string | PriceType): pt is PriceType;
/**
 * Type guard to check if a string is a valid resolution
 */
export declare function isResolution(res: string): res is Resolution;
export declare function isValidChangeFreq(freq: string): freq is EnumChangefreq;
/**
 * Type guard to check if a string is a valid yes/no value
 */
export declare function isValidYesNo(yn: string): yn is EnumYesNo;
/**
 * Type guard to check if a string is a valid allow/deny value
 */
export declare function isAllowDeny(ad: string): ad is EnumAllowDeny;
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
export declare function validateURL(url: string, paramName: string): void;
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
export declare function validatePath(path: string, paramName: string): void;
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
export declare function validatePublicBasePath(publicBasePath: string): void;
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
export declare function validateLimit(limit: number): void;
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
export declare function validateXSLUrl(xslUrl: string): void;
/**
 * Verifies all data passed in will comply with sitemap spec.
 * @param conf Options to validate
 * @param level logging level
 * @param errorHandler error handling func
 */
export declare function validateSMIOptions(conf: SitemapItem, level?: ErrorLevel, errorHandler?: ErrorHandler): SitemapItem;
