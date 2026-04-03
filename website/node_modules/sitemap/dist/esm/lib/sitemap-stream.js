import { Transform, Writable, } from 'node:stream';
import { ErrorLevel } from './types.js';
import { normalizeURL } from './utils.js';
import { validateSMIOptions, validateURL, validateXSLUrl, } from './validation.js';
import { SitemapItemStream } from './sitemap-item-stream.js';
import { EmptyStream, EmptySitemap } from './errors.js';
import { LIMITS } from './constants.js';
const xmlDec = '<?xml version="1.0" encoding="UTF-8"?>';
export const stylesheetInclude = (url) => {
    const safe = url
        .replace(/&/g, '&amp;')
        .replace(/"/g, '&quot;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;');
    return `<?xml-stylesheet type="text/xsl" href="${safe}"?>`;
};
const urlsetTagStart = '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"';
/**
 * Validates custom namespace declarations for security
 * @param custom - Array of custom namespace declarations
 * @throws {Error} If namespace format is invalid or contains malicious content
 */
function validateCustomNamespaces(custom) {
    if (!Array.isArray(custom)) {
        throw new Error('Custom namespaces must be an array');
    }
    // Limit number of custom namespaces to prevent DoS
    if (custom.length > LIMITS.MAX_CUSTOM_NAMESPACES) {
        throw new Error(`Too many custom namespaces: ${custom.length} exceeds limit of ${LIMITS.MAX_CUSTOM_NAMESPACES}`);
    }
    // Basic format validation for xmlns declarations and namespace-qualified attributes
    // Supports both xmlns:prefix="uri" and prefix:attribute="value" (e.g., xsi:schemaLocation)
    const xmlAttributePattern = /^[a-zA-Z_][\w.-]*:[a-zA-Z_][\w.-]*="[^"<>]*"$/;
    for (const ns of custom) {
        if (typeof ns !== 'string' || ns.length === 0) {
            throw new Error('Custom namespace must be a non-empty string');
        }
        if (ns.length > LIMITS.MAX_NAMESPACE_LENGTH) {
            throw new Error(`Custom namespace exceeds maximum length of ${LIMITS.MAX_NAMESPACE_LENGTH} characters: ${ns.substring(0, 50)}...`);
        }
        // Check for potentially malicious content BEFORE format check
        // (format check will reject < and > but we want specific error message)
        const lowerNs = ns.toLowerCase();
        if (lowerNs.includes('<script') ||
            lowerNs.includes('javascript:') ||
            lowerNs.includes('data:text/html')) {
            throw new Error(`Custom namespace contains potentially malicious content: ${ns.substring(0, 50)}`);
        }
        // Check format matches xmlns declaration or namespace-qualified attribute
        if (!xmlAttributePattern.test(ns)) {
            throw new Error(`Invalid namespace format (must be prefix:name="value", e.g., xmlns:prefix="uri" or xsi:schemaLocation="..."): ${ns.substring(0, 50)}`);
        }
    }
}
const getURLSetNs = ({ news, video, image, xhtml, custom }, xslURL) => {
    let ns = xmlDec;
    if (xslURL) {
        ns += stylesheetInclude(xslURL);
    }
    ns += urlsetTagStart;
    if (news) {
        ns += ' xmlns:news="http://www.google.com/schemas/sitemap-news/0.9"';
    }
    if (xhtml) {
        ns += ' xmlns:xhtml="http://www.w3.org/1999/xhtml"';
    }
    if (image) {
        ns += ' xmlns:image="http://www.google.com/schemas/sitemap-image/1.1"';
    }
    if (video) {
        ns += ' xmlns:video="http://www.google.com/schemas/sitemap-video/1.1"';
    }
    if (custom) {
        validateCustomNamespaces(custom);
        ns += ' ' + custom.join(' ');
    }
    return ns + '>';
};
export const closetag = '</urlset>';
const defaultXMLNS = {
    news: true,
    xhtml: true,
    image: true,
    video: true,
};
const defaultStreamOpts = {
    xmlns: defaultXMLNS,
};
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
export class SitemapStream extends Transform {
    hostname;
    level;
    hasHeadOutput;
    xmlNS;
    xslUrl;
    errorHandler;
    smiStream;
    lastmodDateOnly;
    constructor(opts = defaultStreamOpts) {
        opts.objectMode = true;
        super(opts);
        // Validate hostname if provided
        if (opts.hostname !== undefined) {
            validateURL(opts.hostname, 'hostname');
        }
        // Validate xslUrl if provided
        if (opts.xslUrl !== undefined) {
            validateXSLUrl(opts.xslUrl);
        }
        this.hasHeadOutput = false;
        this.hostname = opts.hostname;
        this.level = opts.level || ErrorLevel.WARN;
        this.errorHandler = opts.errorHandler;
        this.smiStream = new SitemapItemStream({ level: opts.level });
        this.smiStream.on('data', (data) => this.push(data));
        this.lastmodDateOnly = opts.lastmodDateOnly || false;
        this.xmlNS = opts.xmlns || defaultXMLNS;
        this.xslUrl = opts.xslUrl;
    }
    _transform(item, encoding, callback) {
        if (!this.hasHeadOutput) {
            this.hasHeadOutput = true;
            this.push(getURLSetNs(this.xmlNS, this.xslUrl));
        }
        if (!this.smiStream.write(validateSMIOptions(normalizeURL(item, this.hostname, this.lastmodDateOnly), this.level, this.errorHandler))) {
            this.smiStream.once('drain', callback);
        }
        else {
            process.nextTick(callback);
        }
    }
    _flush(cb) {
        if (!this.hasHeadOutput) {
            cb(new EmptySitemap());
        }
        else {
            this.push(closetag);
            cb();
        }
    }
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
export function streamToPromise(stream) {
    return new Promise((resolve, reject) => {
        const drain = [];
        stream
            // Error propagation is not automatic
            // Bubble up errors on the read stream
            .on('error', reject)
            .pipe(new Writable({
            write(chunk, enc, next) {
                drain.push(chunk);
                next();
            },
        }))
            // This bubbles up errors when writing to the internal buffer
            // This is unlikely to happen, but we have this for completeness
            .on('error', reject)
            .on('finish', () => {
            if (!drain.length) {
                reject(new EmptyStream());
            }
            else {
                resolve(Buffer.concat(drain));
            }
        });
    });
}
