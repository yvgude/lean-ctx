import { Transform } from 'node:stream';
import { InvalidAttr } from './errors.js';
import { ErrorLevel, TagNames } from './types.js';
import { element, otag, ctag } from './sitemap-xml.js';
/**
 * Builds an attributes object for XML elements from configuration object
 * Extracts attributes based on colon-delimited keys (e.g., 'price:currency' -> { currency: value })
 *
 * @param conf - Configuration object containing attribute values
 * @param keys - Single key or array of keys in format 'namespace:attribute'
 * @returns Record of attribute names to string values (may contain non-string values from conf)
 * @throws {InvalidAttr} When key format is invalid (must contain exactly one colon)
 *
 * @example
 * attrBuilder({ 'price:currency': 'USD', 'price:type': 'rent' }, ['price:currency', 'price:type'])
 * // Returns: { currency: 'USD', type: 'rent' }
 */
function attrBuilder(conf, keys) {
    if (typeof keys === 'string') {
        keys = [keys];
    }
    const iv = {};
    return keys.reduce((attrs, key) => {
        if (conf[key] !== undefined) {
            const keyAr = key.split(':');
            if (keyAr.length !== 2) {
                throw new InvalidAttr(key);
            }
            attrs[keyAr[1]] = conf[key];
        }
        return attrs;
    }, iv);
}
/**
 * Takes a stream of SitemapItemOptions and spits out xml for each
 * @example
 * // writes <url><loc>https://example.com</loc><url><url><loc>https://example.com/2</loc><url>
 * const smis = new SitemapItemStream({level: 'warn'})
 * smis.pipe(writestream)
 * smis.write({url: 'https://example.com', img: [], video: [], links: []})
 * smis.write({url: 'https://example.com/2', img: [], video: [], links: []})
 * smis.end()
 * @param level - Error level
 */
export class SitemapItemStream extends Transform {
    level;
    constructor(opts = { level: ErrorLevel.WARN }) {
        opts.objectMode = true;
        super(opts);
        this.level = opts.level || ErrorLevel.WARN;
    }
    _transform(item, encoding, callback) {
        this.push(otag(TagNames.url));
        this.push(element(TagNames.loc, item.url));
        if (item.lastmod) {
            this.push(element(TagNames.lastmod, item.lastmod));
        }
        if (item.changefreq) {
            this.push(element(TagNames.changefreq, item.changefreq));
        }
        if (item.priority !== undefined && item.priority !== null) {
            if (item.fullPrecisionPriority) {
                this.push(element(TagNames.priority, item.priority.toString()));
            }
            else {
                this.push(element(TagNames.priority, item.priority.toFixed(1)));
            }
        }
        item.video.forEach((video) => {
            this.push(otag(TagNames['video:video']));
            this.push(element(TagNames['video:thumbnail_loc'], video.thumbnail_loc));
            this.push(element(TagNames['video:title'], video.title));
            this.push(element(TagNames['video:description'], video.description));
            if (video.content_loc) {
                this.push(element(TagNames['video:content_loc'], video.content_loc));
            }
            if (video.player_loc) {
                this.push(element(TagNames['video:player_loc'], attrBuilder(video, [
                    'player_loc:autoplay',
                    'player_loc:allow_embed',
                ]), video.player_loc));
            }
            if (video.duration) {
                this.push(element(TagNames['video:duration'], video.duration.toString()));
            }
            if (video.expiration_date) {
                this.push(element(TagNames['video:expiration_date'], video.expiration_date));
            }
            if (video.rating !== undefined) {
                this.push(element(TagNames['video:rating'], video.rating.toString()));
            }
            if (video.view_count !== undefined) {
                this.push(element(TagNames['video:view_count'], String(video.view_count)));
            }
            if (video.publication_date) {
                this.push(element(TagNames['video:publication_date'], video.publication_date));
            }
            if (video.tag && video.tag.length > 0) {
                for (const tag of video.tag) {
                    this.push(element(TagNames['video:tag'], tag));
                }
            }
            if (video.category) {
                this.push(element(TagNames['video:category'], video.category));
            }
            if (video.family_friendly) {
                this.push(element(TagNames['video:family_friendly'], video.family_friendly));
            }
            if (video.restriction) {
                this.push(element(TagNames['video:restriction'], attrBuilder(video, 'restriction:relationship'), video.restriction));
            }
            if (video.gallery_loc) {
                this.push(element(TagNames['video:gallery_loc'], attrBuilder(video, 'gallery_loc:title'), video.gallery_loc));
            }
            if (video.price) {
                this.push(element(TagNames['video:price'], attrBuilder(video, [
                    'price:resolution',
                    'price:currency',
                    'price:type',
                ]), video.price));
            }
            if (video.requires_subscription) {
                this.push(element(TagNames['video:requires_subscription'], video.requires_subscription));
            }
            if (video.uploader) {
                this.push(element(TagNames['video:uploader'], attrBuilder(video, 'uploader:info'), video.uploader));
            }
            if (video.platform) {
                this.push(element(TagNames['video:platform'], attrBuilder(video, 'platform:relationship'), video.platform));
            }
            if (video.live) {
                this.push(element(TagNames['video:live'], video.live));
            }
            if (video.id) {
                this.push(element(TagNames['video:id'], { type: 'url' }, video.id));
            }
            this.push(ctag(TagNames['video:video']));
        });
        item.links.forEach((link) => {
            this.push(element(TagNames['xhtml:link'], {
                rel: 'alternate',
                hreflang: link.lang || link.hreflang,
                href: link.url,
            }));
        });
        if (item.expires) {
            this.push(element(TagNames.expires, new Date(item.expires).toISOString()));
        }
        if (item.androidLink) {
            this.push(element(TagNames['xhtml:link'], {
                rel: 'alternate',
                href: item.androidLink,
            }));
        }
        if (item.ampLink) {
            this.push(element(TagNames['xhtml:link'], {
                rel: 'amphtml',
                href: item.ampLink,
            }));
        }
        if (item.news) {
            this.push(otag(TagNames['news:news']));
            this.push(otag(TagNames['news:publication']));
            this.push(element(TagNames['news:name'], item.news.publication.name));
            this.push(element(TagNames['news:language'], item.news.publication.language));
            this.push(ctag(TagNames['news:publication']));
            if (item.news.access) {
                this.push(element(TagNames['news:access'], item.news.access));
            }
            if (item.news.genres) {
                this.push(element(TagNames['news:genres'], item.news.genres));
            }
            this.push(element(TagNames['news:publication_date'], item.news.publication_date));
            this.push(element(TagNames['news:title'], item.news.title));
            if (item.news.keywords) {
                this.push(element(TagNames['news:keywords'], item.news.keywords));
            }
            if (item.news.stock_tickers) {
                this.push(element(TagNames['news:stock_tickers'], item.news.stock_tickers));
            }
            this.push(ctag(TagNames['news:news']));
        }
        // Image handling
        item.img.forEach((image) => {
            this.push(otag(TagNames['image:image']));
            this.push(element(TagNames['image:loc'], image.url));
            if (image.caption) {
                this.push(element(TagNames['image:caption'], image.caption));
            }
            if (image.geoLocation) {
                this.push(element(TagNames['image:geo_location'], image.geoLocation));
            }
            if (image.title) {
                this.push(element(TagNames['image:title'], image.title));
            }
            if (image.license) {
                this.push(element(TagNames['image:license'], image.license));
            }
            this.push(ctag(TagNames['image:image']));
        });
        this.push(ctag(TagNames.url));
        callback();
    }
}
