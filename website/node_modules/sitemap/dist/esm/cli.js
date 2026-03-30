#!/usr/bin/env node
import { createReadStream, createWriteStream } from 'node:fs';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { xmlLint } from './lib/xmllint.js';
import { XMLLintUnavailable } from './lib/errors.js';
import { ObjectStreamToJSON, XMLToSitemapItemStream, } from './lib/sitemap-parser.js';
import { lineSeparatedURLsToSitemapOptions } from './lib/utils.js';
import { SitemapStream } from './lib/sitemap-stream.js';
import { SitemapAndIndexStream } from './lib/sitemap-index-stream.js';
import { URL } from 'node:url';
import { createGzip } from 'node:zlib';
import { ErrorLevel } from './lib/types.js';
import arg from 'arg';
// Read package.json from the project root (one level up from dist/esm or dist/cjs)
// In ESM, __dirname is not defined, so we use import.meta.url
// In CJS, __dirname is defined and import.meta is not available
let currentDir;
try {
    // eslint-disable-next-line @typescript-eslint/ban-ts-comment
    // @ts-ignore - __dirname may not be defined in ESM
    currentDir = __dirname;
}
catch {
    // ESM fallback using import.meta.url
    currentDir = new URL('.', import.meta.url).pathname;
}
const packageJson = JSON.parse(readFileSync(resolve(currentDir, '../../package.json'), 'utf8'));
const pickStreamOrArg = (argv) => {
    if (!argv._.length) {
        return process.stdin;
    }
    else {
        return createReadStream(argv._[0], { encoding: 'utf8' });
    }
};
const argSpec = {
    '--help': Boolean,
    '--version': Boolean,
    '--validate': Boolean,
    '--index': Boolean,
    '--index-base-url': String,
    '--limit': Number,
    '--parse': Boolean,
    '--single-line-json': Boolean,
    '--prepend': String,
    '--gzip': Boolean,
    '-h': '--help',
};
const argv = arg(argSpec);
function getStream() {
    if (argv._ && argv._.length) {
        return createReadStream(argv._[0]);
    }
    else {
        console.warn('Reading from stdin. If you are not piping anything in, this command is not doing anything');
        return process.stdin;
    }
}
if (argv['--version']) {
    console.log(packageJson.version);
}
else if (argv['--help']) {
    console.log(`
Turn a list of urls into a sitemap xml.
Options:
  --help                 Print this text
  --version              Print the version
  --validate             Ensure the passed in file is conforms to the sitemap spec
  --index                Create an index and stream that out. Writes out sitemaps along the way.
  --index-base-url       Base url the sitemaps will be hosted eg. https://example.com/sitemaps/
  --limit=45000          Set a custom limit to the items per sitemap
  --parse                Parse fed xml and spit out config
  --prepend=sitemap.xml  Prepend the streamed in sitemap configs to sitemap.xml
  --gzip                 Compress output
  --single-line-json     When used with parse, it spits out each entry as json rather than the whole json.

# examples

Generate a sitemap index file as well as sitemaps
  npx sitemap --gzip --index --index-base-url https://example.com/path/to/sitemaps/ < listofurls.txt > sitemap-index.xml.gz

Add to a sitemap
  npx sitemap --prepend sitemap.xml < listofurls.json

Turn an existing sitemap into configuration understood by the sitemap library
  npx sitemap --parse sitemap.xml

Use XMLLib to validate your sitemap (requires xmllib)
  npx sitemap --validate sitemap.xml
`);
}
else if (argv['--parse']) {
    let oStream = getStream()
        .pipe(new XMLToSitemapItemStream({ level: ErrorLevel.THROW }))
        .pipe(new ObjectStreamToJSON({ lineSeparated: !argv['--single-line-json'] }));
    if (argv['--gzip']) {
        oStream = oStream.pipe(createGzip());
    }
    oStream.pipe(process.stdout);
}
else if (argv['--validate']) {
    xmlLint(getStream())
        .then(() => console.log('valid'))
        .catch(([error, stderr]) => {
        if (error instanceof XMLLintUnavailable) {
            console.error(error.message);
            return;
        }
        else {
            console.log(stderr);
        }
    });
}
else if (argv['--index']) {
    const limit = argv['--limit'];
    const baseURL = argv['--index-base-url'];
    if (!baseURL) {
        throw new Error("You must specify where the sitemaps will be hosted. use --index-base-url 'https://example.com/path'");
    }
    const sms = new SitemapAndIndexStream({
        limit,
        getSitemapStream: (i) => {
            const sm = new SitemapStream();
            const path = `./sitemap-${i}.xml`;
            let ws;
            if (argv['--gzip']) {
                ws = sm.pipe(createGzip()).pipe(createWriteStream(path));
            }
            else {
                ws = sm.pipe(createWriteStream(path));
            }
            return [new URL(path, baseURL).toString(), sm, ws];
        },
    });
    let oStream = lineSeparatedURLsToSitemapOptions(pickStreamOrArg(argv)).pipe(sms);
    if (argv['--gzip']) {
        oStream = oStream.pipe(createGzip());
    }
    oStream.pipe(process.stdout);
}
else {
    const sms = new SitemapStream();
    if (argv['--prepend']) {
        createReadStream(argv['--prepend'])
            .pipe(new XMLToSitemapItemStream())
            .pipe(sms);
    }
    const oStream = lineSeparatedURLsToSitemapOptions(pickStreamOrArg(argv)).pipe(sms);
    if (argv['--gzip']) {
        oStream.pipe(createGzip()).pipe(process.stdout);
    }
    else {
        oStream.pipe(process.stdout);
    }
}
