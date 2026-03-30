import sax from 'sax';
import { Transform, } from 'node:stream';
import { ErrorLevel, IndexTagNames } from './types.js';
import { validateURL } from './validation.js';
import { LIMITS } from './constants.js';
function isValidTagName(tagName) {
    // This only works because the enum name and value are the same
    return tagName in IndexTagNames;
}
function tagTemplate() {
    return {
        url: '',
    };
}
const defaultLogger = (level, ...message) => console[level](...message);
const defaultStreamOpts = {
    logger: defaultLogger,
};
/**
 * Takes a stream of xml and transforms it into a stream of IndexItems
 * Use this to parse existing sitemap indices into config options compatible with this library
 */
export class XMLToSitemapIndexStream extends Transform {
    level;
    logger;
    error;
    saxStream;
    constructor(opts = defaultStreamOpts) {
        opts.objectMode = true;
        super(opts);
        this.error = null;
        this.saxStream = sax.createStream(true, {
            xmlns: true,
            // @ts-expect-error - SAX types don't include strictEntities option
            strictEntities: true,
            trim: true,
        });
        this.level = opts.level || ErrorLevel.WARN;
        if (this.level !== ErrorLevel.SILENT && opts.logger !== false) {
            this.logger = opts.logger ?? defaultLogger;
        }
        else {
            this.logger = () => undefined;
        }
        let currentItem = tagTemplate();
        let currentTag;
        this.saxStream.on('opentagstart', (tag) => {
            currentTag = tag.name;
        });
        this.saxStream.on('opentag', (tag) => {
            if (!isValidTagName(tag.name)) {
                this.logger('warn', 'unhandled tag', tag.name);
                this.err(`unhandled tag: ${tag.name}`);
            }
        });
        this.saxStream.on('text', (text) => {
            switch (currentTag) {
                case IndexTagNames.loc:
                    // Validate URL for security: prevents protocol injection, checks length limits
                    try {
                        validateURL(text, 'Sitemap index URL');
                        currentItem.url = text;
                    }
                    catch (error) {
                        const errMsg = error instanceof Error ? error.message : String(error);
                        this.logger('warn', 'Invalid URL in sitemap index:', errMsg);
                        this.err(`Invalid URL in sitemap index: ${errMsg}`);
                    }
                    break;
                case IndexTagNames.lastmod:
                    // Validate date format for security and spec compliance
                    if (text && !LIMITS.ISO_DATE_REGEX.test(text)) {
                        this.logger('warn', 'Invalid lastmod date format in sitemap index:', text);
                        this.err(`Invalid lastmod date format: ${text}`);
                    }
                    else {
                        currentItem.lastmod = text;
                    }
                    break;
                default:
                    this.logger('log', 'unhandled text for tag:', currentTag, `'${text}'`);
                    this.err(`unhandled text for tag: ${currentTag} '${text}'`);
                    break;
            }
        });
        this.saxStream.on('cdata', (text) => {
            switch (currentTag) {
                case IndexTagNames.loc:
                    // Validate URL for security: prevents protocol injection, checks length limits
                    try {
                        validateURL(text, 'Sitemap index URL');
                        currentItem.url = text;
                    }
                    catch (error) {
                        const errMsg = error instanceof Error ? error.message : String(error);
                        this.logger('warn', 'Invalid URL in sitemap index:', errMsg);
                        this.err(`Invalid URL in sitemap index: ${errMsg}`);
                    }
                    break;
                case IndexTagNames.lastmod:
                    // Validate date format for security and spec compliance
                    if (text && !LIMITS.ISO_DATE_REGEX.test(text)) {
                        this.logger('warn', 'Invalid lastmod date format in sitemap index:', text);
                        this.err(`Invalid lastmod date format: ${text}`);
                    }
                    else {
                        currentItem.lastmod = text;
                    }
                    break;
                default:
                    this.logger('log', 'unhandled cdata for tag:', currentTag);
                    this.err(`unhandled cdata for tag: ${currentTag}`);
                    break;
            }
        });
        this.saxStream.on('attribute', (attr) => {
            switch (currentTag) {
                case IndexTagNames.sitemapindex:
                    break;
                default:
                    this.logger('log', 'unhandled attr', currentTag, attr.name);
                    this.err(`unhandled attr: ${currentTag} ${attr.name}`);
            }
        });
        this.saxStream.on('closetag', (tag) => {
            switch (tag) {
                case IndexTagNames.sitemap:
                    // Only push items with valid URLs (non-empty after validation)
                    if (currentItem.url) {
                        this.push(currentItem);
                    }
                    currentItem = tagTemplate();
                    break;
                default:
                    break;
            }
        });
    }
    _transform(data, encoding, callback) {
        try {
            const cb = () => callback(this.level === ErrorLevel.THROW ? this.error : null);
            // correcting the type here can be done without making it a breaking change
            // TODO fix this
            // eslint-disable-next-line @typescript-eslint/ban-ts-comment
            // @ts-ignore
            if (!this.saxStream.write(data, encoding)) {
                this.saxStream.once('drain', cb);
            }
            else {
                process.nextTick(cb);
            }
        }
        catch (error) {
            callback(error);
        }
    }
    err(msg) {
        if (!this.error)
            this.error = new Error(msg);
    }
}
/**
  Read xml and resolve with the configuration that would produce it or reject with
  an error
  ```
  const { createReadStream } = require('fs')
  const { parseSitemapIndex, createSitemap } = require('sitemap')
  parseSitemapIndex(createReadStream('./example-index.xml')).then(
    // produces the same xml
    // you can, of course, more practically modify it or store it
    (xmlConfig) => console.log(createSitemap(xmlConfig).toString()),
    (err) => console.log(err)
  )
  ```
  @param {Readable} xml what to parse
  @param {number} maxEntries Maximum number of sitemap entries to parse (default: 50,000 per sitemaps.org spec)
  @return {Promise<IndexItem[]>} resolves with list of index items that can be fed into a SitemapIndexStream. Rejects with an Error object.
 */
export async function parseSitemapIndex(xml, maxEntries = LIMITS.MAX_SITEMAP_ITEM_LIMIT) {
    const urls = [];
    return new Promise((resolve, reject) => {
        let settled = false;
        const parser = new XMLToSitemapIndexStream();
        // Handle source stream errors (prevents unhandled error events on xml)
        xml.on('error', (error) => {
            if (!settled) {
                settled = true;
                reject(error);
            }
        });
        xml
            .pipe(parser)
            .on('data', (smi) => {
            if (settled)
                return;
            // Security: Prevent memory exhaustion by limiting number of entries
            if (urls.length >= maxEntries) {
                settled = true;
                reject(new Error(`Sitemap index exceeds maximum allowed entries (${maxEntries})`));
                // Immediately destroy both streams to stop further processing (BB-05)
                parser.destroy();
                xml.destroy();
                return;
            }
            urls.push(smi);
        })
            .on('end', () => {
            if (!settled) {
                settled = true;
                resolve(urls);
            }
        })
            .on('error', (error) => {
            if (!settled) {
                settled = true;
                reject(error);
            }
        });
    });
}
const defaultObjectStreamOpts = {
    lineSeparated: false,
};
/**
 * A Transform that converts a stream of objects into a JSON Array or a line
 * separated stringified JSON
 * @param [lineSeparated=false] whether to separate entries by a new line or comma
 */
export class IndexObjectStreamToJSON extends Transform {
    lineSeparated;
    firstWritten;
    constructor(opts = defaultObjectStreamOpts) {
        opts.writableObjectMode = true;
        super(opts);
        this.lineSeparated = opts.lineSeparated;
        this.firstWritten = false;
    }
    _transform(chunk, encoding, cb) {
        if (!this.firstWritten) {
            this.firstWritten = true;
            if (!this.lineSeparated) {
                this.push('[');
            }
        }
        else if (this.lineSeparated) {
            this.push('\n');
        }
        else {
            this.push(',');
        }
        if (chunk) {
            this.push(JSON.stringify(chunk));
        }
        cb();
    }
    _flush(cb) {
        if (!this.lineSeparated) {
            this.push(']');
        }
        cb();
    }
}
