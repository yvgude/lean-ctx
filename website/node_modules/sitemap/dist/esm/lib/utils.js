/*!
 * Sitemap
 * Copyright(c) 2011 Eugene Kalinin
 * MIT Licensed
 */
import { statSync } from 'node:fs';
import { Readable, Transform, PassThrough, } from 'node:stream';
import { createInterface } from 'node:readline';
import { URL } from 'node:url';
import { EnumYesNo, } from './types.js';
// Re-export validateSMIOptions from validation.ts for backward compatibility
export { validateSMIOptions } from './validation.js';
/**
 * Combines multiple streams into one
 * @param streams the streams to combine
 */
export function mergeStreams(streams, options) {
    let pass = new PassThrough(options);
    let waiting = streams.length;
    for (const stream of streams) {
        pass = stream.pipe(pass, { end: false });
        stream.once('end', () => --waiting === 0 && pass.emit('end'));
    }
    return pass;
}
/**
 * Wraps node's ReadLine in a stream
 */
export class ReadlineStream extends Readable {
    _source;
    constructor(options) {
        if (options.autoDestroy === undefined) {
            options.autoDestroy = true;
        }
        options.objectMode = true;
        super(options);
        this._source = createInterface({
            input: options.input,
            terminal: false,
            crlfDelay: Infinity,
        });
        // Every time there's data, push it into the internal buffer.
        this._source.on('line', (chunk) => {
            // If push() returns false, then stop reading from source.
            if (!this.push(chunk))
                this._source.pause();
        });
        // When the source ends, push the EOF-signaling `null` chunk.
        this._source.on('close', () => {
            this.push(null);
        });
    }
    // _read() will be called when the stream wants to pull more data in.
    // The advisory size argument is ignored in this case.
    _read(size) {
        this._source.resume();
    }
}
/**
 * Takes a stream likely from fs.createReadStream('./path') and returns a stream
 * of sitemap items
 * @param stream a stream of line separated urls.
 * @param opts.isJSON is the stream line separated JSON. leave undefined to guess
 */
export function lineSeparatedURLsToSitemapOptions(stream, { isJSON } = {}) {
    return new ReadlineStream({ input: stream }).pipe(new Transform({
        objectMode: true,
        transform: (line, encoding, cb) => {
            if (isJSON || (isJSON === undefined && line[0] === '{')) {
                cb(null, JSON.parse(line));
            }
            else {
                cb(null, line);
            }
        },
    }));
}
/**
 * Based on lodash's implementation of chunk.
 *
 * Copyright JS Foundation and other contributors <https://js.foundation/>
 *
 * Based on Underscore.js, copyright Jeremy Ashkenas,
 * DocumentCloud and Investigative Reporters & Editors <http://underscorejs.org/>
 *
 * This software consists of voluntary contributions made by many
 * individuals. For exact contribution history, see the revision history
 * available at https://github.com/lodash/lodash
 */
/* eslint-disable @typescript-eslint/no-explicit-any */
export function chunk(array, size = 1) {
    size = Math.max(Math.trunc(size), 0);
    const length = array ? array.length : 0;
    if (!length || size < 1) {
        return [];
    }
    const result = Array(Math.ceil(length / size));
    let index = 0, resIndex = 0;
    while (index < length) {
        result[resIndex++] = array.slice(index, (index += size));
    }
    return result;
}
function boolToYESNO(bool) {
    if (bool === undefined) {
        return undefined;
    }
    if (typeof bool === 'boolean') {
        return bool ? EnumYesNo.yes : EnumYesNo.no;
    }
    return bool;
}
/**
 * Converts the passed in sitemap entry into one capable of being consumed by SitemapItem
 * @param {string | SitemapItemLoose} elem the string or object to be converted
 * @param {string} hostname
 * @returns SitemapItemOptions a strict sitemap item option
 */
export function normalizeURL(elem, hostname, lastmodDateOnly = false) {
    // SitemapItem
    // create object with url property
    const smi = {
        img: [],
        video: [],
        links: [],
        url: '',
    };
    if (typeof elem === 'string') {
        smi.url = new URL(elem, hostname).toString();
        return smi;
    }
    const { url, img, links, video, lastmodfile, lastmodISO, lastmod, ...other } = elem;
    Object.assign(smi, other);
    smi.url = new URL(url, hostname).toString();
    if (img) {
        // prepend hostname to all image urls
        smi.img = (Array.isArray(img) ? img : [img]).map((el) => typeof el === 'string'
            ? { url: new URL(el, hostname).toString() }
            : { ...el, url: new URL(el.url, hostname).toString() });
    }
    if (links) {
        smi.links = links.map((link) => ({
            ...link,
            url: new URL(link.url, hostname).toString(),
        }));
    }
    if (video) {
        smi.video = (Array.isArray(video) ? video : [video]).map((video) => {
            const nv = {
                ...video,
                family_friendly: boolToYESNO(video.family_friendly),
                live: boolToYESNO(video.live),
                requires_subscription: boolToYESNO(video.requires_subscription),
                tag: [],
                rating: undefined,
            };
            if (video.tag !== undefined) {
                nv.tag = !Array.isArray(video.tag) ? [video.tag] : video.tag;
            }
            if (video.rating !== undefined) {
                if (typeof video.rating === 'string') {
                    const parsedRating = parseFloat(video.rating);
                    // Validate parsed rating is a valid number
                    if (Number.isNaN(parsedRating)) {
                        throw new Error(`Invalid video rating "${video.rating}" for URL "${elem.url}": must be a valid number`);
                    }
                    nv.rating = parsedRating;
                }
                else {
                    nv.rating = video.rating;
                }
            }
            if (typeof video.view_count === 'string') {
                const parsedViewCount = parseInt(video.view_count, 10);
                // Validate parsed view count is a valid non-negative integer
                if (Number.isNaN(parsedViewCount)) {
                    throw new Error(`Invalid video view_count "${video.view_count}" for URL "${elem.url}": must be a valid number`);
                }
                if (parsedViewCount < 0) {
                    throw new Error(`Invalid video view_count "${video.view_count}" for URL "${elem.url}": cannot be negative`);
                }
                nv.view_count = parsedViewCount;
            }
            else if (typeof video.view_count === 'number') {
                nv.view_count = video.view_count;
            }
            return nv;
        });
    }
    // If given a file to use for last modified date
    if (lastmodfile) {
        const { mtime } = statSync(lastmodfile);
        const lastmodDate = new Date(mtime);
        // Validate date is valid
        if (Number.isNaN(lastmodDate.getTime())) {
            throw new Error(`Invalid date from file stats for URL "${smi.url}": file modification time is invalid`);
        }
        smi.lastmod = lastmodDate.toISOString();
        // The date of last modification (YYYY-MM-DD)
    }
    else if (lastmodISO) {
        const lastmodDate = new Date(lastmodISO);
        // Validate date is valid
        if (Number.isNaN(lastmodDate.getTime())) {
            throw new Error(`Invalid lastmodISO "${lastmodISO}" for URL "${smi.url}": must be a valid date string`);
        }
        smi.lastmod = lastmodDate.toISOString();
    }
    else if (lastmod) {
        const lastmodDate = new Date(lastmod);
        // Validate date is valid
        if (Number.isNaN(lastmodDate.getTime())) {
            throw new Error(`Invalid lastmod "${lastmod}" for URL "${smi.url}": must be a valid date string`);
        }
        smi.lastmod = lastmodDate.toISOString();
    }
    if (lastmodDateOnly && smi.lastmod) {
        smi.lastmod = smi.lastmod.slice(0, 10);
    }
    return smi;
}
