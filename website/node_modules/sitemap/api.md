# API

- [API](#api)
  - [SitemapStream](#sitemapstream)
  - [XMLToSitemapItemStream](#xmltositemapitemstream)
  - [SitemapAndIndexStream](#sitemapandindexstream)
  - [simpleSitemapAndIndex](#simplesitemapandindex)
  - [SitemapIndexStream](#sitemapindexstream)
  - [xmlLint](#xmllint)
  - [parseSitemap](#parsesitemap)
  - [lineSeparatedURLsToSitemapOptions](#lineseparatedurlstositemapoptions)
  - [streamToPromise](#streamtopromise)
  - [ObjectStreamToJSON](#objectstreamtojson)
  - [SitemapItemStream](#sitemapitemstream)
  - [Sitemap Item Options](#sitemap-item-options)
  - [SitemapImage](#sitemapimage)
  - [VideoItem](#videoitem)
  - [LinkItem](#linkitem)
  - [NewsItem](#newsitem)

## SitemapStream

A [Transform](https://nodejs.org/api/stream.html#stream_implementing_a_transform_stream) for turning a [Readable stream](https://nodejs.org/api/stream.html#stream_readable_streams) of either [SitemapItemOptions](#sitemap-item-options) or url strings into a Sitemap. The readable stream it transforms **must** be in object mode.

```javascript
// ESM
import { SitemapStream } from 'sitemap'

// CommonJS
const { SitemapStream } = require('sitemap')

const sms = new SitemapStream({
  hostname: 'https://example.com', // optional only necessary if your paths are relative
  lastmodDateOnly: false // defaults to false, flip to true for baidu
  xmlns: { // XML namespaces to turn on - all by default
    news: true,
    xhtml: true,
    image: true,
    video: true,
    // custom: ['xmlns:custom="https://example.com"']
  },
  errorHandler: undefined // defaults to a standard errorLogger that logs to console or throws if the errorLevel is set to throw
})
const readable = // a readable stream of objects
readable.pipe(sms).pipe(process.stdout)
```

## XMLToSitemapItemStream

Takes a stream of xml and transforms it into a stream of SitemapOptions.
Use this to parse existing sitemaps into config options compatible with this library

```javascript
// ESM
import { createReadStream, createWriteStream } from 'fs';
import { XMLToSitemapItemStream, ObjectStreamToJSON, ErrorLevel } from 'sitemap';

// CommonJS
const { createReadStream, createWriteStream } = require('fs');
const { XMLToSitemapItemStream, ObjectStreamToJSON, ErrorLevel } = require('sitemap');

createReadStream('./some/sitemap.xml')
// turn the xml into sitemap option item options
.pipe(new XMLToSitemapItemStream({
  // optional
  level: ErrorLevel.WARN // default is WARN pass SILENT to silence
  logger: false // default is console log, pass false as another way to silence or your own custom logger
}))
// convert the object stream to JSON
.pipe(new ObjectStreamToJSON())
// write the library compatible options to disk
.pipe(createWriteStream('./sitemapOptions.json'))
```

## SitemapAndIndexStream

Use this to take a stream which may go over the max of 50000 items and split it into an index and sitemaps.
SitemapAndIndexStream consumes a stream of urls and streams out index entries while writing individual urls to the streams you give it.
Provide it with a function which when provided with a index returns a url where the sitemap will ultimately be hosted and a stream to write the current sitemap to. This function will be called everytime the next item in the stream would exceed the provided limit.

```js
// ESM
import { createReadStream, createWriteStream } from 'fs';
import { resolve } from 'path';
import { createGzip } from 'zlib';
import {
  SitemapAndIndexStream,
  SitemapStream,
  lineSeparatedURLsToSitemapOptions
} from 'sitemap';

// CommonJS
const { createReadStream, createWriteStream } = require('fs');
const { resolve } = require('path');
const { createGzip } = require('zlib');
const {
  SitemapAndIndexStream,
  SitemapStream,
  lineSeparatedURLsToSitemapOptions
} = require('sitemap');

const sms = new SitemapAndIndexStream({
  limit: 10000, // defaults to 45k
  // SitemapAndIndexStream will call this user provided function every time
  // it needs to create a new sitemap file. You merely need to return a stream
  // for it to write the sitemap urls to and the expected url where that sitemap will be hosted
  getSitemapStream: (i) => {
    const sitemapStream = new SitemapStream();
    const path = `./sitemap-${i}.xml`;

    const ws = sitemapStream
      .pipe(createGzip()) // compress the output of the sitemap
      .pipe(createWriteStream(resolve(path + '.gz'))); // write it to sitemap-NUMBER.xml

    return [new URL(path, 'https://example.com/subdir/').toString(), sitemapStream, ws];
  },
});

lineSeparatedURLsToSitemapOptions(
  createReadStream('./your-data.json.txt')
)
.pipe(sms)
.pipe(createGzip())
.pipe(createWriteStream(resolve('./sitemap-index.xml.gz')));
```

## simpleSitemapAndIndex

A simpler interface for creating sitemaps and indexes. Automatically handles splitting large datasets into multiple sitemap files.

```js
// ESM
import { simpleSitemapAndIndex } from 'sitemap';

// CommonJS
const { simpleSitemapAndIndex } = require('sitemap');

// writes sitemaps and index out to the destination you provide.
await simpleSitemapAndIndex({
  hostname: 'https://example.com',
  destinationDir: './',
  sourceData: [
    { url: '/page-1/', changefreq: 'daily', priority: 0.3 },
    { url: '/page-2/', changefreq: 'weekly', priority: 0.7 },
    // ... more URLs
  ],
  // optional: limit URLs per sitemap (default: 50000, must be 1-50000)
  limit: 45000,
  // optional: gzip the output files (default: true)
  gzip: true,
  // optional: public base path for sitemap URLs (default: './')
  publicBasePath: '/sitemaps/',
  // optional: XSL stylesheet URL for XML display
  xslUrl: 'https://example.com/sitemap.xsl',
  // or read from a file
  // sourceData: lineSeparatedURLsToSitemapOptions(createReadStream('./urls.txt')),
  // or
  // sourceData: './urls.txt',
});
```

### Options

- **hostname** (required): The base URL for all sitemap entries. Must be a valid `http://` or `https://` URL.
- **sitemapHostname** (optional): The base URL for sitemap index entries if different from `hostname`. Must be a valid `http://` or `https://` URL.
- **destinationDir** (required): Directory where sitemaps and index will be written. Can be relative or absolute, but must not contain path traversal sequences (`..`).
- **sourceData** (required): URL source data. Can be:
  - Array of strings (URLs)
  - Array of `SitemapItemLoose` objects
  - String (file path to line-separated URLs)
  - Readable stream
- **limit** (optional): Maximum URLs per sitemap file. Must be between 1 and 50,000 per [sitemaps.org spec](https://www.sitemaps.org/protocol.html). Default: 50000
- **gzip** (optional): Whether to gzip compress the output files. Default: true
- **publicBasePath** (optional): Base path for sitemap URLs in the index. Must not contain path traversal sequences. Default: './'
- **xslUrl** (optional): URL to an XSL stylesheet for XML display. Must be a valid `http://` or `https://` URL.

### Security

`simpleSitemapAndIndex` includes comprehensive security validation to protect against common attacks:

**URL Validation:**
- All URLs (hostname, sitemapHostname) must use `http://` or `https://` protocols only
- Maximum URL length enforced at 2048 characters per sitemaps.org specification
- URLs are parsed and validated to ensure they are well-formed

**Path Traversal Protection:**
- `destinationDir` and `publicBasePath` are checked for path traversal sequences (`..`)
- Validation detects `..` in all positions (beginning, middle, end, standalone)
- Both Unix-style (`/`) and Windows-style (`\`) path separators are normalized and checked
- Null bytes (`\0`) are rejected to prevent path manipulation attacks

**XSL Stylesheet Security:**
- XSL URLs must use `http://` or `https://` protocols
- Case-insensitive checks block dangerous content patterns:
  - Script tags: `<script`, `<ScRiPt`, `<SCRIPT>`, etc.
  - Dangerous protocols: `javascript:`, `data:`, `vbscript:`, `file:`, `about:`
  - URL-encoded attacks: `%3cscript`, `javascript%3a`, etc.
- Maximum URL length enforced at 2048 characters

**Resource Limits:**
- Limit validated to be an integer between 1 and 50,000 per sitemaps.org specification
- Prevents resource exhaustion attacks and ensures search engine compatibility

**Data Validation:**
- Video ratings are validated to be valid numbers between 0 and 5
- Video view counts are validated to be non-negative integers
- Date values (lastmod, lastmodISO) are validated to be parseable dates

### Errors

May throw:

- `InvalidHostnameError`: Invalid or malformed hostname/sitemapHostname
- `InvalidPathError`: destinationDir contains path traversal or invalid characters
- `InvalidPublicBasePathError`: publicBasePath contains path traversal or invalid characters
- `InvalidLimitError`: limit is out of range (not 1-50,000)
- `InvalidXSLUrlError`: xslUrl is invalid or potentially malicious
- `Error`: Invalid sourceData type or file system errors

## SitemapIndexStream

Writes a sitemap index when given a stream urls.

```js
// ESM
import { SitemapIndexStream } from 'sitemap';

// CommonJS
const { SitemapIndexStream } = require('sitemap');

/**
 * writes the following
 * <?xml version="1.0" encoding="UTF-8"?>
   <sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
   <sitemap>
      <loc>https://example.com/</loc>
   </sitemap>
   <sitemap>
      <loc>https://example.com/2</loc>
   </sitemap>
 */
const smis = new SitemapIndexStream({level: 'warn'})
smis.write({url: 'https://example.com/'})
smis.write({url: 'https://example.com/2'})
smis.pipe(writestream)
smis.end()
```

## xmlLint

Resolve or reject depending on whether the passed in xml is a valid sitemap.
This is just a wrapper around the xmlLint command line tool and thus requires
xmlLint to be installed on the system.

**Security Note:** This function accepts XML content as a string or Readable stream
and always pipes it via stdin to xmllint. It does NOT accept file paths to prevent
command injection vulnerabilities.

```js
// ESM
import { createReadStream, readFileSync } from 'fs';
import { xmlLint } from 'sitemap';

// CommonJS
const { createReadStream, readFileSync } = require('fs');
const { xmlLint } = require('sitemap');

// Validate using a stream
xmlLint(createReadStream('./example.xml')).then(
  () => console.log('xml is valid'),
  ([err, stderr]) => console.error('xml is invalid', stderr)
)

// Validate using a string
const xmlContent = readFileSync('./example.xml', 'utf8');
xmlLint(xmlContent).then(
  () => console.log('xml is valid'),
  ([err, stderr]) => console.error('xml is invalid', stderr)
)
```

## parseSitemap

Read xml and resolve with an array of sitemap items or reject with an error

```js
// ESM
import { createReadStream } from 'fs';
import { parseSitemap } from 'sitemap';

// CommonJS
const { createReadStream } = require('fs');
const { parseSitemap } = require('sitemap');

parseSitemap(createReadStream('./example.xml')).then(
  (items) => {
    // items is an array of sitemap items
    console.log(items);
  },
  (err) => console.log(err)
)
```

## lineSeparatedURLsToSitemapOptions

Takes a stream of urls or sitemapoptions likely from fs.createReadStream('./path') and returns an object stream of sitemap items.

## streamToPromise

Takes a stream returns a promise that resolves when stream emits finish.

```javascript
// ESM
import { streamToPromise, SitemapStream } from 'sitemap';

// CommonJS
const { streamToPromise, SitemapStream } = require('sitemap');

const sitemap = new SitemapStream({ hostname: 'http://example.com' });
sitemap.write({ url: '/page-1/', changefreq: 'daily', priority: 0.3 })
sitemap.end()
streamToPromise(sitemap).then(buffer => console.log(buffer.toString())) // emits the full sitemap
```

## ObjectStreamToJSON

A Transform that converts a stream of objects into a JSON Array or a line separated stringified JSON.

- @param [lineSeparated=false] whether to separate entries by a new line or comma

```javascript
// ESM
import { Readable } from 'stream';
import { ObjectStreamToJSON } from 'sitemap';

// CommonJS
const { Readable } = require('stream');
const { ObjectStreamToJSON } = require('sitemap');

const stream = Readable.from([{a: 'b'}])
  .pipe(new ObjectStreamToJSON())
  .pipe(process.stdout)
stream.end()
// prints {"a":"b"}
```

## SitemapItemStream

Takes a stream of SitemapItemOptions and spits out xml for each

```js
// ESM
import { SitemapItemStream } from 'sitemap';

// CommonJS
const { SitemapItemStream } = require('sitemap');

// writes <url><loc>https://example.com</loc><url><url><loc>https://example.com/2</loc><url>
const smis = new SitemapItemStream({level: 'warn'})
smis.pipe(writestream)
smis.write({url: 'https://example.com', img: [], video: [], links: []})
smis.write({url: 'https://example.com/2', img: [], video: [], links: []})
smis.end()
```

## Sitemap Item Options

|Option|Type|eg|Description|
|------|----|--|-----------|
|url|string|`http://example.com/some/path`|The only required property for every sitemap entry|
|lastmod|string|'2019-07-29' or '2019-07-22T05:58:37.037Z'|When the page we as last modified use the W3C Datetime ISO8601 subset <https://www.sitemaps.org/protocol.html#xmlTagDefinitions>|
|changefreq|string|'weekly'|How frequently the page is likely to change. This value provides general information to search engines and may not correlate exactly to how often they crawl the page. Please note that the value of this tag is considered a hint and not a command. See <https://www.sitemaps.org/protocol.html#xmlTagDefinitions> for the acceptable values|
|priority|number|0.6|The priority of this URL relative to other URLs on your site. Valid values range from 0.0 to 1.0. This value does not affect how your pages are compared to pages on other sites—it only lets the search engines know which pages you deem most important for the crawlers. The default priority of a page is 0.5. <https://www.sitemaps.org/protocol.html#xmlTagDefinitions>|
|img|object[]|see [#SitemapImage](#sitemapimage)|<https://support.google.com/webmasters/answer/178636?hl=en&ref_topic=4581190>|
|video|object[]|see [#VideoItem](#videoitem)|<https://support.google.com/webmasters/answer/80471?hl=en&ref_topic=4581190>|
|links|object[]|see [#LinkItem](#linkitem)|Tell search engines about localized versions <https://support.google.com/webmasters/answer/189077>|
|news|object|see [#NewsItem](#newsitem)|<https://support.google.com/webmasters/answer/74288?hl=en&ref_topic=4581190>|
|ampLink|string|`http://ampproject.org/article.amp.html`||
|cdata|boolean|true|wrap url in cdata xml escape|

## SitemapImage

Sitemap image
<https://support.google.com/webmasters/answer/178636?hl=en&ref_topic=4581190>

|Option|Type|eg|Description|
|------|----|--|-----------|
|url|string|`http://example.com/image.jpg`|The URL of the image.|
|caption|string - optional|'Here we did the stuff'|The caption of the image.|
|title|string - optional|'Star Wars EP IV'|The title of the image.|
|geoLocation|string - optional|'Limerick, Ireland'|The geographic location of the image.|
|license|string - optional|`http://example.com/license.txt`|A URL to the license of the image.|

## VideoItem

Sitemap video. <https://support.google.com/webmasters/answer/80471?hl=en&ref_topic=4581190>

|Option|Type|eg|Description|
|------|----|--|-----------|
|thumbnail_loc|string|`"https://rtv3-img-roosterteeth.akamaized.net/store/0e841100-289b-4184-ae30-b6a16736960a.jpg/sm/thumb3.jpg"`|A URL pointing to the video thumbnail image file|
|title|string|'2018:E6 - GoldenEye: Source'|The title of the video. |
|description|string|'We play gun game in GoldenEye: Source with a good friend of ours. His name is Gruchy. Dan Gruchy.'|A description of the video. Maximum 2048 characters. |
|content_loc|string - optional|`"http://streamserver.example.com/video123.mp4"`|A URL pointing to the actual video media file. Should be one of the supported formats. HTML is not a supported format. Flash is allowed, but no longer supported on most mobile platforms, and so may be indexed less well. Must not be the same as the `<loc>` URL.|
|player_loc|string - optional|`"https://roosterteeth.com/embed/rouletsplay-2018-goldeneye-source"`|A URL pointing to a player for a specific video. Usually this is the information in the src element of an `<embed>` tag. Must not be the same as the `<loc>` URL|
|'player_loc:autoplay'|string - optional|'ap=1'|a string the search engine can append as a query param to enable automatic playback|
|'player_loc:allow_embed'|boolean - optional|'yes'|Whether the search engine can embed the video in search results. Allowed values are yes or no.|
|duration|number - optional| 600| duration of video in seconds|
|expiration_date| string - optional|"2012-07-16T19:20:30+08:00"|The date after which the video will no longer be available|
|view_count|number - optional|'21000000000'|The number of times the video has been viewed.|
|publication_date| string - optional|"2018-04-27T17:00:00.000Z"|The date the video was first published, in W3C format.|
|category|string - optional|"Baking"|A short description of the broad category that the video belongs to. This is a string no longer than 256 characters.|
|restriction|string - optional|"IE GB US CA"|Whether to show or hide your video in search results from specific countries.|
|restriction:relationship| string - optional|"deny"||
|gallery_loc| string - optional|`"https://roosterteeth.com/series/awhu"`|Currently not used.|
|gallery_loc:title|string - optional|"awhu series page"|Currently not used.|
|price|string - optional|"1.99"|The price to download or view the video. Omit this tag for free videos.|
|price:resolution|string - optional|"HD"|Specifies the resolution of the purchased version. Supported values are hd and sd.|
|price:currency| string - optional|"USD"|currency [Required] Specifies the currency in ISO 4217 format.|
|price:type|string - optional|"rent"|type [Optional] Specifies the purchase option. Supported values are rent and own. |
|uploader|string - optional|"GrillyMcGrillerson"|The video uploader's name. Only one <video:uploader> is allowed per video. String value, max 255 characters.|
|uploader:info|string - optional|"https://example.com/about"|Specifies the URL of a webpage with additional information about this uploader. This URL must be in the same domain as the `<loc>` tag.
|platform|string - optional|"tv"|Whether to show or hide your video in search results on specified platform types. This is a list of space-delimited platform types. See <https://support.google.com/webmasters/answer/80471?hl=en&ref_topic=4581190> for more detail|
|platform:relationship|string 'Allow'\|'Deny' - optional|'Allow'||
|id|string - optional|||
|tag|string[] - optional|['Baking']|An arbitrary string tag describing the video. Tags are generally very short descriptions of key concepts associated with a video or piece of content.|
|rating|number - optional|2.5|The rating of the video. Supported values are float numbers|
|family_friendly|string 'YES'\|'NO' - optional|'YES'||
|requires_subscription|string 'YES'\|'NO' - optional|'YES'|Indicates whether a subscription (either paid or free) is required to view the video. Allowed values are yes or no.|
|live|string 'YES'\|'NO' - optional|'NO'|Indicates whether the video is a live stream. Supported values are yes or no.|

## LinkItem

<https://support.google.com/webmasters/answer/189077>

|Option|Type|eg|Description|
|------|----|--|-----------|
|lang|string|'en'||
|url|string|`'http://example.com/en/'`||

## NewsItem

<https://support.google.com/webmasters/answer/74288?hl=en&ref_topic=4581190>

|Option|Type|eg|Description|
|------|----|--|-----------|
|access|string - 'Registration' \| 'Subscription'| 'Registration' - optional||
|publication| object|see following options||
|publication['name']| string|'The Example Times'|The `<name>` is the name of the news publication. It must exactly match the name as it appears on your articles on news.google.com, except for anything in parentheses.|
|publication['language']|string|'en'|The `<language>` is the language of your publication. Use an ISO 639 language code (2 or 3 letters).|
|genres|string - optional|'PressRelease, Blog'||
|publication_date|string|'2008-12-23'|Article publication date in W3C format, using either the "complete date" (YYYY-MM-DD) format or the "complete date plus hours, minutes, and seconds"|
|title|string|'Companies A, B in Merger Talks'|The title of the news article.|
|keywords|string - optional|"business, merger, acquisition, A, B"||
|stock_tickers|string - optional|"NASDAQ:A, NASDAQ:B"||
