import { Transform, TransformOptions, TransformCallback, Readable } from 'node:stream';
import { SitemapItemLoose, ErrorLevel, ErrorHandler } from './types.js';
export declare const stylesheetInclude: (url: string) => string;
export interface NSArgs {
    news: boolean;
    video: boolean;
    xhtml: boolean;
    image: boolean;
    custom?: string[];
}
export declare const closetag = "</urlset>";
export interface SitemapStreamOptions extends TransformOptions {
    hostname?: string;
    level?: ErrorLevel;
    lastmodDateOnly?: boolean;
    xmlns?: NSArgs;
    xslUrl?: string;
    errorHandler?: ErrorHandler;
}
/**
 * A [Transform](https://nodejs.org/api/stream.html#stream_implementing_a_transform_stream)
 * for turning a
 * [Readable stream](https://nodejs.org/api/stream.html#stream_readable_streams)
 * of either [SitemapItemOptions](#sitemap-item-options) or url strings into a
 * Sitemap. The readable stream it transforms **must** be in object mode.
 *
 * @param {SitemapStreamOptions} opts - Configuration options
 * @param {string} [opts.hostname] - Base URL for relative paths. Must use http:// or https:// protocol
 * @param {ErrorLevel} [opts.level=ErrorLevel.WARN] - Error handling level (SILENT, WARN, or THROW)
 * @param {boolean} [opts.lastmodDateOnly=false] - Format lastmod as date only (YYYY-MM-DD)
 * @param {NSArgs} [opts.xmlns] - Control which XML namespaces to include in output
 * @param {string} [opts.xslUrl] - URL to XSL stylesheet for sitemap display. Must use http:// or https://
 * @param {ErrorHandler} [opts.errorHandler] - Custom error handler function
 *
 * @throws {InvalidHostnameError} If hostname is provided but invalid (non-http(s), malformed, or >2048 chars)
 * @throws {InvalidXSLUrlError} If xslUrl is provided but invalid (non-http(s), malformed, >2048 chars, or contains malicious content)
 * @throws {Error} If xmlns.custom contains invalid namespace declarations
 *
 * @example
 * ```typescript
 * const stream = new SitemapStream({
 *   hostname: 'https://example.com',
 *   level: ErrorLevel.THROW
 * });
 * stream.write({ url: '/page', changefreq: 'daily' });
 * stream.end();
 * ```
 *
 * @security
 * - Hostname and xslUrl are validated to prevent URL injection attacks
 * - Custom namespaces are validated to prevent XML injection
 * - All URLs are normalized and validated before output
 * - XML content is properly escaped to prevent injection
 */
export declare class SitemapStream extends Transform {
    hostname?: string;
    level: ErrorLevel;
    hasHeadOutput: boolean;
    xmlNS: NSArgs;
    xslUrl?: string;
    errorHandler?: ErrorHandler;
    private smiStream;
    lastmodDateOnly: boolean;
    constructor(opts?: SitemapStreamOptions);
    _transform(item: SitemapItemLoose, encoding: string, callback: TransformCallback): void;
    _flush(cb: TransformCallback): void;
}
/**
 * Converts a readable stream into a promise that resolves with the concatenated data from the stream.
 *
 * The function listens for 'data' events from the stream, and when the stream ends, it resolves the promise with the concatenated data. If an error occurs while reading from the stream, the promise is rejected with the error.
 *
 * ⚠️ CAUTION: This function should not generally be used in production / when writing to files as it holds a copy of the entire file contents in memory until finished.
 *
 * @param {Readable} stream - The readable stream to convert to a promise.
 * @returns {Promise<Buffer>} A promise that resolves with the concatenated data from the stream as a Buffer, or rejects with an error if one occurred while reading from the stream. If the stream is empty, the promise is rejected with an EmptyStream error.
 * @throws {EmptyStream} If the stream is empty.
 */
export declare function streamToPromise(stream: Readable): Promise<Buffer>;
