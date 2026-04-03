import { Transform } from 'node:stream';
import { ErrorLevel, IndexTagNames, } from './types.js';
import { stylesheetInclude } from './sitemap-stream.js';
import { element, otag, ctag } from './sitemap-xml.js';
import { LIMITS, DEFAULT_SITEMAP_ITEM_LIMIT } from './constants.js';
import { validateURL, validateXSLUrl } from './validation.js';
// Re-export IndexTagNames for backward compatibility
export { IndexTagNames };
const xmlDec = '<?xml version="1.0" encoding="UTF-8"?>';
const sitemapIndexTagStart = '<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">';
const closetag = '</sitemapindex>';
const defaultStreamOpts = {};
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
export class SitemapIndexStream extends Transform {
    lastmodDateOnly;
    level;
    xslUrl;
    hasHeadOutput;
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
    constructor(opts = defaultStreamOpts) {
        opts.objectMode = true;
        super(opts);
        this.hasHeadOutput = false;
        this.lastmodDateOnly = opts.lastmodDateOnly || false;
        this.level = opts.level ?? ErrorLevel.WARN;
        if (opts.xslUrl !== undefined) {
            validateXSLUrl(opts.xslUrl);
        }
        this.xslUrl = opts.xslUrl;
    }
    writeHeadOutput() {
        this.hasHeadOutput = true;
        let stylesheet = '';
        if (this.xslUrl) {
            stylesheet = stylesheetInclude(this.xslUrl);
        }
        this.push(xmlDec + stylesheet + sitemapIndexTagStart);
    }
    _transform(item, encoding, callback) {
        if (!this.hasHeadOutput) {
            this.writeHeadOutput();
        }
        try {
            // Validate URL using centralized validation (checks protocol, length, format)
            const url = typeof item === 'string' ? item : item.url;
            if (!url || typeof url !== 'string') {
                const error = new Error('Invalid sitemap index item: URL must be a non-empty string');
                if (this.level === ErrorLevel.THROW) {
                    callback(error);
                    return;
                }
                else if (this.level === ErrorLevel.WARN) {
                    console.warn(error.message, item);
                }
                // For SILENT or after WARN, skip this item
                callback();
                return;
            }
            // Security: Use centralized validation to enforce protocol restrictions,
            // length limits, and prevent injection attacks
            try {
                validateURL(url, 'Sitemap index URL');
            }
            catch (error) {
                // Wrap the validation error with consistent message format
                const validationMsg = error instanceof Error ? error.message : String(error);
                const err = new Error(`Invalid URL in sitemap index: ${validationMsg}`);
                if (this.level === ErrorLevel.THROW) {
                    callback(err);
                    return;
                }
                else if (this.level === ErrorLevel.WARN) {
                    console.warn(err.message);
                }
                // For SILENT or after WARN, skip this item
                callback();
                return;
            }
            this.push(otag(IndexTagNames.sitemap));
            if (typeof item === 'string') {
                this.push(element(IndexTagNames.loc, item));
            }
            else {
                this.push(element(IndexTagNames.loc, item.url));
                if (item.lastmod) {
                    try {
                        const lastmod = new Date(item.lastmod).toISOString();
                        this.push(element(IndexTagNames.lastmod, this.lastmodDateOnly ? lastmod.slice(0, 10) : lastmod));
                    }
                    catch {
                        const error = new Error(`Invalid lastmod date in sitemap index: ${item.lastmod}`);
                        if (this.level === ErrorLevel.THROW) {
                            callback(error);
                            return;
                        }
                        else if (this.level === ErrorLevel.WARN) {
                            console.warn(error.message);
                        }
                        // Continue without lastmod for SILENT or after WARN
                    }
                }
            }
            this.push(ctag(IndexTagNames.sitemap));
            callback();
        }
        catch (error) {
            callback(error instanceof Error ? error : new Error(String(error)));
        }
    }
    _flush(cb) {
        if (!this.hasHeadOutput) {
            this.writeHeadOutput();
        }
        this.push(closetag);
        cb();
    }
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
export class SitemapAndIndexStream extends SitemapIndexStream {
    itemsWritten;
    getSitemapStream;
    currentSitemap;
    limit;
    currentSitemapPipeline;
    /**
     * Flag to prevent race conditions when creating new sitemap files.
     * Set to true while waiting for the current sitemap to finish and
     * a new one to be created.
     */
    isCreatingSitemap;
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
    constructor(opts) {
        opts.objectMode = true;
        super(opts);
        this.itemsWritten = 0;
        this.getSitemapStream = opts.getSitemapStream;
        this.limit = opts.limit ?? DEFAULT_SITEMAP_ITEM_LIMIT;
        this.isCreatingSitemap = false;
        // Validate limit is within acceptable range per sitemaps.org spec
        // See: https://www.sitemaps.org/protocol.html#index
        if (this.limit < LIMITS.MIN_SITEMAP_ITEM_LIMIT ||
            this.limit > LIMITS.MAX_SITEMAP_ITEM_LIMIT) {
            throw new Error(`limit must be between ${LIMITS.MIN_SITEMAP_ITEM_LIMIT} and ${LIMITS.MAX_SITEMAP_ITEM_LIMIT} per sitemaps.org spec, got ${this.limit}`);
        }
    }
    _transform(item, encoding, callback) {
        if (this.itemsWritten % this.limit === 0) {
            // Prevent race condition if multiple items arrive during sitemap creation
            if (this.isCreatingSitemap) {
                // Wait and retry on next tick
                process.nextTick(() => this._transform(item, encoding, callback));
                return;
            }
            if (this.currentSitemap) {
                this.isCreatingSitemap = true;
                const currentSitemap = this.currentSitemap;
                const currentPipeline = this.currentSitemapPipeline;
                // Set up promises with proper cleanup to prevent memory leaks
                const onFinish = new Promise((resolve, reject) => {
                    const finishHandler = () => {
                        currentSitemap.off('error', errorHandler);
                        resolve();
                    };
                    const errorHandler = (err) => {
                        currentSitemap.off('finish', finishHandler);
                        reject(err);
                    };
                    currentSitemap.on('finish', finishHandler);
                    currentSitemap.on('error', errorHandler);
                    currentSitemap.end();
                });
                const onPipelineFinish = currentPipeline
                    ? new Promise((resolve, reject) => {
                        const finishHandler = () => {
                            currentPipeline.off('error', errorHandler);
                            resolve();
                        };
                        const errorHandler = (err) => {
                            currentPipeline.off('finish', finishHandler);
                            reject(err);
                        };
                        currentPipeline.on('finish', finishHandler);
                        currentPipeline.on('error', errorHandler);
                    })
                    : Promise.resolve();
                Promise.all([onFinish, onPipelineFinish])
                    .then(() => {
                    this.isCreatingSitemap = false;
                    this.createSitemap(encoding);
                    this.writeItem(item, callback);
                })
                    .catch((err) => {
                    this.isCreatingSitemap = false;
                    callback(err);
                });
                return;
            }
            else {
                this.createSitemap(encoding);
            }
        }
        this.writeItem(item, callback);
    }
    writeItem(item, callback) {
        if (!this.currentSitemap) {
            callback(new Error('No sitemap stream available'));
            return;
        }
        if (!this.currentSitemap.write(item)) {
            this.currentSitemap.once('drain', callback);
        }
        else {
            process.nextTick(callback);
        }
        // Increment the count of items written
        this.itemsWritten++;
    }
    /**
     * Called when the stream is finished.
     * If there is a current sitemap, we wait for it to finish before calling the callback.
     * Includes proper event listener cleanup to prevent memory leaks.
     *
     * @param cb - The callback to invoke when flushing is complete
     */
    _flush(cb) {
        const currentSitemap = this.currentSitemap;
        const currentPipeline = this.currentSitemapPipeline;
        const onFinish = new Promise((resolve, reject) => {
            if (currentSitemap) {
                const finishHandler = () => {
                    currentSitemap.off('error', errorHandler);
                    resolve();
                };
                const errorHandler = (err) => {
                    currentSitemap.off('finish', finishHandler);
                    reject(err);
                };
                currentSitemap.on('finish', finishHandler);
                currentSitemap.on('error', errorHandler);
                currentSitemap.end();
            }
            else {
                resolve();
            }
        });
        const onPipelineFinish = new Promise((resolve, reject) => {
            if (currentPipeline) {
                const finishHandler = () => {
                    currentPipeline.off('error', errorHandler);
                    resolve();
                };
                const errorHandler = (err) => {
                    currentPipeline.off('finish', finishHandler);
                    reject(err);
                };
                currentPipeline.on('finish', finishHandler);
                currentPipeline.on('error', errorHandler);
                // The pipeline (pipe target) will get its end() call
                // from the sitemap stream ending.
            }
            else {
                resolve();
            }
        });
        Promise.all([onFinish, onPipelineFinish])
            .then(() => {
            super._flush(cb);
        })
            .catch((err) => {
            cb(err);
        });
    }
    createSitemap(encoding) {
        const sitemapIndex = this.itemsWritten / this.limit;
        let result;
        try {
            result = this.getSitemapStream(sitemapIndex);
        }
        catch (err) {
            this.emit('error', new Error(`getSitemapStream callback threw an error for index ${sitemapIndex}: ${err instanceof Error ? err.message : String(err)}`));
            return;
        }
        // Validate the return value
        if (!Array.isArray(result) || result.length !== 3) {
            this.emit('error', new Error(`getSitemapStream must return a 3-element array [IndexItem | string, SitemapStream, WriteStream], got: ${typeof result}`));
            return;
        }
        const [idxItem, currentSitemap, currentSitemapPipeline] = result;
        // Validate each element
        if (!idxItem ||
            (typeof idxItem !== 'string' && typeof idxItem !== 'object')) {
            this.emit('error', new Error('getSitemapStream must return an IndexItem or string as the first element'));
            return;
        }
        if (!currentSitemap || typeof currentSitemap.write !== 'function') {
            this.emit('error', new Error('getSitemapStream must return a SitemapStream as the second element'));
            return;
        }
        if (currentSitemapPipeline &&
            typeof currentSitemapPipeline.write !== 'function') {
            this.emit('error', new Error('getSitemapStream must return a WriteStream or undefined as the third element'));
            return;
        }
        // Propagate errors from the sitemap stream
        currentSitemap.on('error', (err) => this.emit('error', err));
        this.currentSitemap = currentSitemap;
        this.currentSitemapPipeline = currentSitemapPipeline;
        super._transform(idxItem, encoding, () => {
            // We are not too concerned about waiting for the index item to be written
            // as we'll wait for the file to finish at the end, and index file write
            // volume tends to be small in comparison to sitemap writes.
            // noop
        });
    }
}
