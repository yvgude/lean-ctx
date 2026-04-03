import { Readable } from 'node:stream';
import { SitemapItemLoose } from './types.js';
/**
 * Options for the simpleSitemapAndIndex function
 */
export interface SimpleSitemapAndIndexOptions {
    /**
     * The hostname for all URLs
     * Must be a valid http:// or https:// URL
     */
    hostname: string;
    /**
     * The hostname for the sitemaps if different than hostname
     * Must be a valid http:// or https:// URL
     */
    sitemapHostname?: string;
    /**
     * The urls you want to make a sitemap out of.
     * Can be an array of items, a file path string, a Readable stream, or an array of strings
     */
    sourceData: SitemapItemLoose[] | string | Readable | string[];
    /**
     * Where to write the sitemaps and index
     * Must be a relative path without path traversal sequences
     */
    destinationDir: string;
    /**
     * Where the sitemaps are relative to the hostname. Defaults to root.
     * Must not contain path traversal sequences
     */
    publicBasePath?: string;
    /**
     * How many URLs to write before switching to a new file
     * Must be between 1 and 50,000 per sitemaps.org spec
     * @default 50000
     */
    limit?: number;
    /**
     * Whether to compress the written files
     * @default true
     */
    gzip?: boolean;
    /**
     * Optional URL to an XSL stylesheet
     * Must be a valid http:// or https:// URL
     */
    xslUrl?: string;
}
/**
 * A simpler interface for creating sitemaps and indexes.
 * Automatically handles splitting large datasets into multiple sitemap files.
 *
 * @param options - Configuration options
 * @returns A promise that resolves when all sitemaps and the index are written
 * @throws {InvalidHostnameError} If hostname or sitemapHostname is invalid
 * @throws {InvalidPathError} If destinationDir contains path traversal
 * @throws {InvalidPublicBasePathError} If publicBasePath is invalid
 * @throws {InvalidLimitError} If limit is out of range
 * @throws {InvalidXSLUrlError} If xslUrl is invalid
 * @throws {Error} If sourceData type is not supported
 */
export declare const simpleSitemapAndIndex: ({ hostname, sitemapHostname, sourceData, destinationDir, limit, gzip, publicBasePath, xslUrl, }: SimpleSitemapAndIndexOptions) => Promise<void>;
export default simpleSitemapAndIndex;
