"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.simpleSitemapAndIndex = void 0;
const sitemap_index_stream_js_1 = require("./sitemap-index-stream.js");
const sitemap_stream_js_1 = require("./sitemap-stream.js");
const utils_js_1 = require("./utils.js");
const node_zlib_1 = require("node:zlib");
const node_fs_1 = require("node:fs");
const node_path_1 = require("node:path");
const node_stream_1 = require("node:stream");
const promises_1 = require("node:stream/promises");
const node_url_1 = require("node:url");
const validation_js_1 = require("./validation.js");
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
const simpleSitemapAndIndex = async ({ hostname, sitemapHostname = hostname, // if different
sourceData, destinationDir, limit = 50000, gzip = true, publicBasePath = './', xslUrl, }) => {
    // Validate all inputs upfront
    (0, validation_js_1.validateURL)(hostname, 'hostname');
    (0, validation_js_1.validateURL)(sitemapHostname, 'sitemapHostname');
    (0, validation_js_1.validatePath)(destinationDir, 'destinationDir');
    (0, validation_js_1.validateLimit)(limit);
    (0, validation_js_1.validatePublicBasePath)(publicBasePath);
    if (xslUrl) {
        (0, validation_js_1.validateXSLUrl)(xslUrl);
    }
    // Create destination directory with error context
    try {
        await node_fs_1.promises.mkdir(destinationDir, { recursive: true });
    }
    catch (err) {
        throw new Error(`Failed to create destination directory "${destinationDir}": ${err instanceof Error ? err.message : String(err)}`);
    }
    // Normalize publicBasePath (don't mutate the parameter)
    const normalizedPublicBasePath = publicBasePath.endsWith('/')
        ? publicBasePath
        : publicBasePath + '/';
    const sitemapAndIndexStream = new sitemap_index_stream_js_1.SitemapAndIndexStream({
        limit,
        getSitemapStream: (i) => {
            const sitemapStream = new sitemap_stream_js_1.SitemapStream({
                hostname,
                xslUrl,
            });
            const path = `./sitemap-${i}.xml`;
            const writePath = (0, node_path_1.resolve)(destinationDir, path + (gzip ? '.gz' : ''));
            // Construct public path for the sitemap index
            const publicPath = (0, node_path_1.normalize)(normalizedPublicBasePath + path);
            // Construct the URL with proper error handling
            let sitemapUrl;
            try {
                sitemapUrl = new node_url_1.URL(`${publicPath}${gzip ? '.gz' : ''}`, sitemapHostname).toString();
            }
            catch (err) {
                throw new Error(`Failed to construct sitemap URL for index ${i}: ${err instanceof Error ? err.message : String(err)}`);
            }
            let writeStream;
            if (gzip) {
                writeStream = sitemapStream
                    .pipe((0, node_zlib_1.createGzip)()) // compress the output of the sitemap
                    .pipe((0, node_fs_1.createWriteStream)(writePath)); // write it to sitemap-NUMBER.xml
            }
            else {
                writeStream = sitemapStream.pipe((0, node_fs_1.createWriteStream)(writePath)); // write it to sitemap-NUMBER.xml
            }
            return [sitemapUrl, sitemapStream, writeStream];
        },
    });
    // Handle different sourceData types with proper error handling
    let src;
    if (typeof sourceData === 'string') {
        try {
            src = (0, utils_js_1.lineSeparatedURLsToSitemapOptions)((0, node_fs_1.createReadStream)(sourceData));
        }
        catch (err) {
            throw new Error(`Failed to read sourceData file "${sourceData}": ${err instanceof Error ? err.message : String(err)}`);
        }
    }
    else if (sourceData instanceof node_stream_1.Readable) {
        src = sourceData;
    }
    else if (Array.isArray(sourceData)) {
        src = node_stream_1.Readable.from(sourceData);
    }
    else {
        throw new Error(`Invalid sourceData type: expected array, string (file path), or Readable stream, got ${typeof sourceData}`);
    }
    const writePath = (0, node_path_1.resolve)(destinationDir, `./sitemap-index.xml${gzip ? '.gz' : ''}`);
    try {
        if (gzip) {
            return await (0, promises_1.pipeline)(src, sitemapAndIndexStream, (0, node_zlib_1.createGzip)(), (0, node_fs_1.createWriteStream)(writePath));
        }
        else {
            return await (0, promises_1.pipeline)(src, sitemapAndIndexStream, (0, node_fs_1.createWriteStream)(writePath));
        }
    }
    catch (err) {
        throw new Error(`Failed to write sitemap files: ${err instanceof Error ? err.message : String(err)}`);
    }
};
exports.simpleSitemapAndIndex = simpleSitemapAndIndex;
exports.default = exports.simpleSitemapAndIndex;
