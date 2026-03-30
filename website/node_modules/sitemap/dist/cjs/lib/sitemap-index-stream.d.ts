import { WriteStream } from 'node:fs';
import { Transform, TransformOptions, TransformCallback } from 'node:stream';
import { IndexItem, SitemapItemLoose, ErrorLevel, IndexTagNames } from './types.js';
import { SitemapStream } from './sitemap-stream.js';
export { IndexTagNames };
/**
 * Options for the SitemapIndexStream
 */
export interface SitemapIndexStreamOptions extends TransformOptions {
    /**
     * Whether to output the lastmod date only (no time)
     *
     * @default false
     */
    lastmodDateOnly?: boolean;
    /**
     * How to handle errors in passed in urls
     *
     * @default ErrorLevel.WARN
     */
    level?: ErrorLevel;
    /**
     * URL to an XSL stylesheet to include in the XML
     */
    xslUrl?: string;
}
/**
 * `SitemapIndexStream` is a Transform stream that takes `IndexItem`s or sitemap URL strings and outputs a stream of sitemap index XML.
 *
 * It automatically handles the XML declaration and the opening and closing tags for the sitemap index.
 *
 * ⚠️ CAUTION: This object is `readable` and must be read (e.g. piped to a file or to /dev/null)
 * before `finish` will be emitted. Failure to read the stream will result in hangs.
 *
 * @extends {Transform}
 */
export declare class SitemapIndexStream extends Transform {
    lastmodDateOnly: boolean;
    level: ErrorLevel;
    xslUrl?: string;
    private hasHeadOutput;
    /**
     * `SitemapIndexStream` is a Transform stream that takes `IndexItem`s or sitemap URL strings and outputs a stream of sitemap index XML.
     *
     * It automatically handles the XML declaration and the opening and closing tags for the sitemap index.
     *
     * ⚠️ CAUTION: This object is `readable` and must be read (e.g. piped to a file or to /dev/null)
     * before `finish` will be emitted. Failure to read the stream will result in hangs.
     *
     * @param {SitemapIndexStreamOptions} [opts=defaultStreamOpts] - Stream options.
     */
    constructor(opts?: SitemapIndexStreamOptions);
    private writeHeadOutput;
    _transform(item: IndexItem | string, encoding: string, callback: TransformCallback): void;
    _flush(cb: TransformCallback): void;
}
/**
 * Callback function type for creating new sitemap streams when the item limit is reached.
 *
 * This function is called by SitemapAndIndexStream to create a new sitemap file when
 * the current one reaches the item limit.
 *
 * @param i - The zero-based index of the sitemap file being created (0 for first sitemap,
 *            1 for second, etc.)
 * @returns A tuple containing:
 *          - [0]: IndexItem or URL string to add to the sitemap index
 *          - [1]: SitemapStream instance for writing sitemap items
 *          - [2]: WriteStream where the sitemap will be piped (the stream will be
 *                 awaited for 'finish' before creating the next sitemap)
 *
 * @example
 * ```typescript
 * const getSitemapStream = (i: number) => {
 *   const sitemapStream = new SitemapStream();
 *   const path = `./sitemap-${i}.xml`;
 *   const writeStream = createWriteStream(path);
 *   sitemapStream.pipe(writeStream);
 *   return [`https://example.com/${path}`, sitemapStream, writeStream];
 * };
 * ```
 */
type getSitemapStreamFunc = (i: number) => [IndexItem | string, SitemapStream, WriteStream];
/**
 * Options for the SitemapAndIndexStream
 *
 * @extends {SitemapIndexStreamOptions}
 */
export interface SitemapAndIndexStreamOptions extends SitemapIndexStreamOptions {
    /**
     * Max number of items in each sitemap XML file.
     *
     * When the limit is reached the current sitemap file will be closed,
     * a wait for `finish` on the target write stream will happen,
     * and a new sitemap file will be created.
     *
     * Range: 1 - 50,000
     *
     * @default 45000
     */
    limit?: number;
    /**
     * Callback for SitemapIndexAndStream that creates a new sitemap stream for a given sitemap index.
     *
     * Called when a new sitemap file is needed.
     *
     * The write stream is the destination where the sitemap was piped.
     * SitemapAndIndexStream will wait for the `finish` event on each sitemap's
     * write stream before moving on to the next sitemap. This ensures that the
     * contents of the write stream will be fully written before being used
     * by any following operations (e.g. uploading, reading contents for unit tests).
     *
     * @param i - The index of the sitemap file
     * @returns A tuple containing the index item to be written into the sitemap index, the sitemap stream, and the write stream for the sitemap pipe destination
     */
    getSitemapStream: getSitemapStreamFunc;
}
/**
 * `SitemapAndIndexStream` is a Transform stream that takes in sitemap items,
 * writes them to sitemap files, adds the sitemap files to a sitemap index,
 * and creates new sitemap files when the count limit is reached.
 *
 * It waits for the target stream of the current sitemap file to finish before
 * moving on to the next if the target stream is returned by the `getSitemapStream`
 * callback in the 3rd position of the tuple.
 *
 * ⚠️ CAUTION: This object is `readable` and must be read (e.g. piped to a file or to /dev/null)
 * before `finish` will be emitted. Failure to read the stream will result in hangs.
 *
 * @extends {SitemapIndexStream}
 */
export declare class SitemapAndIndexStream extends SitemapIndexStream {
    private itemsWritten;
    private getSitemapStream;
    private currentSitemap?;
    private limit;
    private currentSitemapPipeline?;
    /**
     * Flag to prevent race conditions when creating new sitemap files.
     * Set to true while waiting for the current sitemap to finish and
     * a new one to be created.
     */
    private isCreatingSitemap;
    /**
     * `SitemapAndIndexStream` is a Transform stream that takes in sitemap items,
     * writes them to sitemap files, adds the sitemap files to a sitemap index,
     * and creates new sitemap files when the count limit is reached.
     *
     * It waits for the target stream of the current sitemap file to finish before
     * moving on to the next if the target stream is returned by the `getSitemapStream`
     * callback in the 3rd position of the tuple.
     *
     * ⚠️ CAUTION: This object is `readable` and must be read (e.g. piped to a file or to /dev/null)
     * before `finish` will be emitted. Failure to read the stream will result in hangs.
     *
     * @param {SitemapAndIndexStreamOptions} opts - Stream options.
     */
    constructor(opts: SitemapAndIndexStreamOptions);
    _transform(item: SitemapItemLoose, encoding: string, callback: TransformCallback): void;
    private writeItem;
    /**
     * Called when the stream is finished.
     * If there is a current sitemap, we wait for it to finish before calling the callback.
     * Includes proper event listener cleanup to prevent memory leaks.
     *
     * @param cb - The callback to invoke when flushing is complete
     */
    _flush(cb: TransformCallback): void;
    private createSitemap;
}
