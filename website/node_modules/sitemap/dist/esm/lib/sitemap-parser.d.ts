import type { SAXStream } from 'sax';
import { Readable, Transform, TransformOptions, TransformCallback } from 'node:stream';
import { SitemapItem, ErrorLevel } from './types.js';
type Logger = (level: 'warn' | 'error' | 'info' | 'log', ...message: Parameters<Console['log']>[0]) => void;
export interface XMLToSitemapItemStreamOptions extends TransformOptions {
    level?: ErrorLevel;
    logger?: Logger | false;
}
/**
 * Takes a stream of xml and transforms it into a stream of SitemapItems
 * Use this to parse existing sitemaps into config options compatible with this library
 */
export declare class XMLToSitemapItemStream extends Transform {
    level: ErrorLevel;
    logger: Logger;
    /**
     * Errors encountered during parsing, capped at LIMITS.MAX_PARSER_ERRORS entries
     * to prevent memory DoS from malformed XML (BB-03).
     * Use errorCount for the total number of errors regardless of the cap.
     */
    errors: Error[];
    /** Total number of errors seen, including those beyond the stored cap. */
    errorCount: number;
    saxStream: SAXStream;
    urlCount: number;
    constructor(opts?: XMLToSitemapItemStreamOptions);
    _transform(data: string, encoding: string, callback: TransformCallback): void;
    private err;
}
/**
  Read xml and resolve with the configuration that would produce it or reject with
  an error
  ```
  const { createReadStream } = require('fs')
  const { parseSitemap, createSitemap } = require('sitemap')
  parseSitemap(createReadStream('./example.xml')).then(
    // produces the same xml
    // you can, of course, more practically modify it or store it
    (xmlConfig) => console.log(createSitemap(xmlConfig).toString()),
    (err) => console.log(err)
  )
  ```
  @param {Readable} xml what to parse
  @return {Promise<SitemapItem[]>} resolves with list of sitemap items that can be fed into a SitemapStream. Rejects with an Error object.
 */
export declare function parseSitemap(xml: Readable): Promise<SitemapItem[]>;
export interface ObjectStreamToJSONOptions extends TransformOptions {
    lineSeparated: boolean;
}
/**
 * A Transform that converts a stream of objects into a JSON Array or a line
 * separated stringified JSON
 * @param [lineSeparated=false] whether to separate entries by a new line or comma
 */
export declare class ObjectStreamToJSON extends Transform {
    lineSeparated: boolean;
    firstWritten: boolean;
    constructor(opts?: ObjectStreamToJSONOptions);
    _transform(chunk: SitemapItem, encoding: string, cb: TransformCallback): void;
    _flush(cb: TransformCallback): void;
}
export {};
