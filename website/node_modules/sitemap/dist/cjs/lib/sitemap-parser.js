"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.ObjectStreamToJSON = exports.XMLToSitemapItemStream = void 0;
exports.parseSitemap = parseSitemap;
const sax_1 = __importDefault(require("sax"));
const node_stream_1 = require("node:stream");
const types_js_1 = require("./types.js");
const validation_js_1 = require("./validation.js");
const constants_js_1 = require("./constants.js");
function isValidTagName(tagName) {
    // This only works because the enum name and value are the same
    return tagName in types_js_1.TagNames;
}
function getAttrValue(attr) {
    if (!attr)
        return undefined;
    return typeof attr === 'string' ? attr : attr.value;
}
function tagTemplate() {
    return {
        img: [],
        video: [],
        links: [],
        url: '',
    };
}
function videoTemplate() {
    return {
        tag: [],
        thumbnail_loc: '',
        title: '',
        description: '',
    };
}
const imageTemplate = {
    url: '',
};
const linkTemplate = {
    lang: '',
    url: '',
};
function newsTemplate() {
    return {
        publication: { name: '', language: '' },
        publication_date: '',
        title: '',
    };
}
const defaultLogger = (level, ...message) => console[level](...message);
const defaultStreamOpts = {
    logger: defaultLogger,
};
// TODO does this need to end with `options`
/**
 * Takes a stream of xml and transforms it into a stream of SitemapItems
 * Use this to parse existing sitemaps into config options compatible with this library
 */
class XMLToSitemapItemStream extends node_stream_1.Transform {
    level;
    logger;
    /**
     * Errors encountered during parsing, capped at LIMITS.MAX_PARSER_ERRORS entries
     * to prevent memory DoS from malformed XML (BB-03).
     * Use errorCount for the total number of errors regardless of the cap.
     */
    errors;
    /** Total number of errors seen, including those beyond the stored cap. */
    errorCount;
    saxStream;
    urlCount;
    constructor(opts = defaultStreamOpts) {
        opts.objectMode = true;
        super(opts);
        this.errors = [];
        this.errorCount = 0;
        this.urlCount = 0;
        this.saxStream = sax_1.default.createStream(true, {
            xmlns: true,
            // eslint-disable-next-line @typescript-eslint/ban-ts-comment
            // @ts-ignore
            strictEntities: true,
            trim: true,
        });
        this.level = opts.level || types_js_1.ErrorLevel.WARN;
        if (this.level !== types_js_1.ErrorLevel.SILENT && opts.logger !== false) {
            this.logger = opts.logger ?? defaultLogger;
        }
        else {
            this.logger = () => undefined;
        }
        let currentItem = tagTemplate();
        let currentTag;
        let currentVideo = videoTemplate();
        let currentImage = { ...imageTemplate };
        let currentLink = { ...linkTemplate };
        let dontpushCurrentLink = false;
        this.saxStream.on('opentagstart', (tag) => {
            currentTag = tag.name;
            if (currentTag.startsWith('news:') && !currentItem.news) {
                currentItem.news = newsTemplate();
            }
        });
        this.saxStream.on('opentag', (tag) => {
            if (isValidTagName(tag.name)) {
                if (tag.name === 'xhtml:link') {
                    // SAX returns attributes as objects with {name, value, prefix, local, uri}
                    // Check if required attributes exist and have values
                    const rel = getAttrValue(tag.attributes.rel);
                    const href = getAttrValue(tag.attributes.href);
                    const hreflang = getAttrValue(tag.attributes.hreflang);
                    if (!rel || !href) {
                        this.logger('warn', 'xhtml:link missing required rel or href attribute');
                        this.err('xhtml:link missing required rel or href attribute');
                        return;
                    }
                    if (rel === 'alternate' && hreflang) {
                        currentLink.url = href;
                        currentLink.lang = hreflang;
                    }
                    else if (rel === 'alternate') {
                        dontpushCurrentLink = true;
                        currentItem.androidLink = href;
                    }
                    else if (rel === 'amphtml') {
                        dontpushCurrentLink = true;
                        currentItem.ampLink = href;
                    }
                    else {
                        this.logger('log', 'unhandled attr for xhtml:link', tag.attributes);
                        this.err(`unhandled attr for xhtml:link ${JSON.stringify(tag.attributes)}`);
                    }
                }
            }
            else {
                this.logger('warn', 'unhandled tag', tag.name);
                this.err(`unhandled tag: ${tag.name}`);
            }
        });
        this.saxStream.on('text', (text) => {
            switch (currentTag) {
                case 'mobile:mobile':
                    break;
                case types_js_1.TagNames.loc:
                    // Validate URL
                    if (text.length > constants_js_1.LIMITS.MAX_URL_LENGTH) {
                        this.logger('warn', `URL exceeds max length of ${constants_js_1.LIMITS.MAX_URL_LENGTH}: ${text.substring(0, 100)}...`);
                        this.err(`URL exceeds max length of ${constants_js_1.LIMITS.MAX_URL_LENGTH}`);
                    }
                    else if (!constants_js_1.LIMITS.URL_PROTOCOL_REGEX.test(text)) {
                        this.logger('warn', `URL must start with http:// or https://: ${text}`);
                        this.err(`URL must start with http:// or https://: ${text}`);
                    }
                    else {
                        currentItem.url = text;
                    }
                    break;
                case types_js_1.TagNames.changefreq:
                    if ((0, validation_js_1.isValidChangeFreq)(text)) {
                        currentItem.changefreq = text;
                    }
                    break;
                case types_js_1.TagNames.priority:
                    {
                        const priority = parseFloat(text);
                        if (isNaN(priority) ||
                            !isFinite(priority) ||
                            priority < 0 ||
                            priority > 1) {
                            this.logger('warn', `Invalid priority "${text}" - must be between 0 and 1`);
                            this.err(`Invalid priority "${text}" - must be between 0 and 1`);
                        }
                        else {
                            currentItem.priority = priority;
                        }
                    }
                    break;
                case types_js_1.TagNames.lastmod:
                    if (constants_js_1.LIMITS.ISO_DATE_REGEX.test(text)) {
                        currentItem.lastmod = text;
                    }
                    else {
                        this.logger('warn', `Invalid lastmod date format "${text}" - expected ISO 8601 format`);
                        this.err(`Invalid lastmod date format "${text}" - expected ISO 8601 format`);
                    }
                    break;
                case types_js_1.TagNames['video:thumbnail_loc']:
                    currentVideo.thumbnail_loc = text;
                    break;
                case types_js_1.TagNames['video:tag']:
                    if (currentVideo.tag.length < constants_js_1.LIMITS.MAX_TAGS_PER_VIDEO) {
                        currentVideo.tag.push(text);
                    }
                    else {
                        this.logger('warn', `video has too many tags (max ${constants_js_1.LIMITS.MAX_TAGS_PER_VIDEO})`);
                        this.err(`video has too many tags (max ${constants_js_1.LIMITS.MAX_TAGS_PER_VIDEO})`);
                    }
                    break;
                case types_js_1.TagNames['video:duration']:
                    {
                        const duration = parseInt(text, 10);
                        if (isNaN(duration) ||
                            !isFinite(duration) ||
                            duration < 0 ||
                            duration > 28800) {
                            this.logger('warn', `Invalid video duration "${text}" - must be between 0 and 28800 seconds`);
                            this.err(`Invalid video duration "${text}" - must be between 0 and 28800 seconds`);
                        }
                        else {
                            currentVideo.duration = duration;
                        }
                    }
                    break;
                case types_js_1.TagNames['video:player_loc']:
                    currentVideo.player_loc = text;
                    break;
                case types_js_1.TagNames['video:content_loc']:
                    currentVideo.content_loc = text;
                    break;
                case types_js_1.TagNames['video:requires_subscription']:
                    if ((0, validation_js_1.isValidYesNo)(text)) {
                        currentVideo.requires_subscription = text;
                    }
                    break;
                case types_js_1.TagNames['video:publication_date']:
                    if (constants_js_1.LIMITS.ISO_DATE_REGEX.test(text)) {
                        currentVideo.publication_date = text;
                    }
                    else {
                        this.logger('warn', `Invalid video publication_date format "${text}" - expected ISO 8601 format`);
                        this.err(`Invalid video publication_date format "${text}" - expected ISO 8601 format`);
                    }
                    break;
                case types_js_1.TagNames['video:id']:
                    currentVideo.id = text;
                    break;
                case types_js_1.TagNames['video:restriction']:
                    currentVideo.restriction = text;
                    break;
                case types_js_1.TagNames['video:view_count']:
                    {
                        const viewCount = parseInt(text, 10);
                        if (isNaN(viewCount) || !isFinite(viewCount) || viewCount < 0) {
                            this.logger('warn', `Invalid video view_count "${text}" - must be a positive integer`);
                            this.err(`Invalid video view_count "${text}" - must be a positive integer`);
                        }
                        else {
                            currentVideo.view_count = viewCount;
                        }
                    }
                    break;
                case types_js_1.TagNames['video:uploader']:
                    currentVideo.uploader = text;
                    break;
                case types_js_1.TagNames['video:family_friendly']:
                    if ((0, validation_js_1.isValidYesNo)(text)) {
                        currentVideo.family_friendly = text;
                    }
                    break;
                case types_js_1.TagNames['video:expiration_date']:
                    if (constants_js_1.LIMITS.ISO_DATE_REGEX.test(text)) {
                        currentVideo.expiration_date = text;
                    }
                    else {
                        this.logger('warn', `Invalid video expiration_date format "${text}" - expected ISO 8601 format`);
                        this.err(`Invalid video expiration_date format "${text}" - expected ISO 8601 format`);
                    }
                    break;
                case types_js_1.TagNames['video:platform']:
                    currentVideo.platform = text;
                    break;
                case types_js_1.TagNames['video:price']:
                    currentVideo.price = text;
                    break;
                case types_js_1.TagNames['video:rating']:
                    {
                        const rating = parseFloat(text);
                        if (isNaN(rating) ||
                            !isFinite(rating) ||
                            rating < 0 ||
                            rating > 5) {
                            this.logger('warn', `Invalid video rating "${text}" - must be between 0 and 5`);
                            this.err(`Invalid video rating "${text}" - must be between 0 and 5`);
                        }
                        else {
                            currentVideo.rating = rating;
                        }
                    }
                    break;
                case types_js_1.TagNames['video:category']:
                    currentVideo.category = text;
                    break;
                case types_js_1.TagNames['video:live']:
                    if ((0, validation_js_1.isValidYesNo)(text)) {
                        currentVideo.live = text;
                    }
                    break;
                case types_js_1.TagNames['video:gallery_loc']:
                    currentVideo.gallery_loc = text;
                    break;
                case types_js_1.TagNames['image:loc']:
                    currentImage.url = text;
                    break;
                case types_js_1.TagNames['image:geo_location']:
                    currentImage.geoLocation = text;
                    break;
                case types_js_1.TagNames['image:license']:
                    currentImage.license = text;
                    break;
                case types_js_1.TagNames['news:access']:
                    if (!currentItem.news) {
                        currentItem.news = newsTemplate();
                    }
                    if (text === 'Registration' || text === 'Subscription') {
                        currentItem.news.access = text;
                    }
                    else {
                        this.logger('warn', `Invalid news:access value "${text}" - must be "Registration" or "Subscription"`);
                        this.err(`Invalid news:access value "${text}" - must be "Registration" or "Subscription"`);
                    }
                    break;
                case types_js_1.TagNames['news:genres']:
                    if (!currentItem.news) {
                        currentItem.news = newsTemplate();
                    }
                    currentItem.news.genres = text;
                    break;
                case types_js_1.TagNames['news:publication_date']:
                    if (!currentItem.news) {
                        currentItem.news = newsTemplate();
                    }
                    if (constants_js_1.LIMITS.ISO_DATE_REGEX.test(text)) {
                        currentItem.news.publication_date = text;
                    }
                    else {
                        this.logger('warn', `Invalid news publication_date format "${text}" - expected ISO 8601 format`);
                        this.err(`Invalid news publication_date format "${text}" - expected ISO 8601 format`);
                    }
                    break;
                case types_js_1.TagNames['news:keywords']:
                    if (!currentItem.news) {
                        currentItem.news = newsTemplate();
                    }
                    currentItem.news.keywords = text;
                    break;
                case types_js_1.TagNames['news:stock_tickers']:
                    if (!currentItem.news) {
                        currentItem.news = newsTemplate();
                    }
                    currentItem.news.stock_tickers = text;
                    break;
                case types_js_1.TagNames['news:language']:
                    if (!currentItem.news) {
                        currentItem.news = newsTemplate();
                    }
                    currentItem.news.publication.language = text;
                    break;
                case types_js_1.TagNames['video:title']:
                    if (currentVideo.title.length + text.length <=
                        constants_js_1.LIMITS.MAX_VIDEO_TITLE_LENGTH) {
                        currentVideo.title += text;
                    }
                    else {
                        this.logger('warn', `video title exceeds max length of ${constants_js_1.LIMITS.MAX_VIDEO_TITLE_LENGTH}`);
                        this.err(`video title exceeds max length of ${constants_js_1.LIMITS.MAX_VIDEO_TITLE_LENGTH}`);
                    }
                    break;
                case types_js_1.TagNames['video:description']:
                    if (currentVideo.description.length + text.length <=
                        constants_js_1.LIMITS.MAX_VIDEO_DESCRIPTION_LENGTH) {
                        currentVideo.description += text;
                    }
                    else {
                        this.logger('warn', `video description exceeds max length of ${constants_js_1.LIMITS.MAX_VIDEO_DESCRIPTION_LENGTH}`);
                        this.err(`video description exceeds max length of ${constants_js_1.LIMITS.MAX_VIDEO_DESCRIPTION_LENGTH}`);
                    }
                    break;
                case types_js_1.TagNames['news:name']:
                    if (!currentItem.news) {
                        currentItem.news = newsTemplate();
                    }
                    if (currentItem.news.publication.name.length + text.length <=
                        constants_js_1.LIMITS.MAX_NEWS_NAME_LENGTH) {
                        currentItem.news.publication.name += text;
                    }
                    else {
                        this.logger('warn', `news name exceeds max length of ${constants_js_1.LIMITS.MAX_NEWS_NAME_LENGTH}`);
                        this.err(`news name exceeds max length of ${constants_js_1.LIMITS.MAX_NEWS_NAME_LENGTH}`);
                    }
                    break;
                case types_js_1.TagNames['news:title']:
                    if (!currentItem.news) {
                        currentItem.news = newsTemplate();
                    }
                    if (currentItem.news.title.length + text.length <=
                        constants_js_1.LIMITS.MAX_NEWS_TITLE_LENGTH) {
                        currentItem.news.title += text;
                    }
                    else {
                        this.logger('warn', `news title exceeds max length of ${constants_js_1.LIMITS.MAX_NEWS_TITLE_LENGTH}`);
                        this.err(`news title exceeds max length of ${constants_js_1.LIMITS.MAX_NEWS_TITLE_LENGTH}`);
                    }
                    break;
                case types_js_1.TagNames['image:caption']:
                    if (!currentImage.caption) {
                        currentImage.caption =
                            text.length <= constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH
                                ? text
                                : text.substring(0, constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH);
                        if (text.length > constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH) {
                            this.logger('warn', `image caption exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH}`);
                            this.err(`image caption exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH}`);
                        }
                    }
                    else if (currentImage.caption.length + text.length <=
                        constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH) {
                        currentImage.caption += text;
                    }
                    else {
                        this.logger('warn', `image caption exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH}`);
                        this.err(`image caption exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH}`);
                    }
                    break;
                case types_js_1.TagNames['image:title']:
                    if (!currentImage.title) {
                        currentImage.title =
                            text.length <= constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH
                                ? text
                                : text.substring(0, constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH);
                        if (text.length > constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH) {
                            this.logger('warn', `image title exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH}`);
                            this.err(`image title exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH}`);
                        }
                    }
                    else if (currentImage.title.length + text.length <=
                        constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH) {
                        currentImage.title += text;
                    }
                    else {
                        this.logger('warn', `image title exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH}`);
                        this.err(`image title exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH}`);
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
                case types_js_1.TagNames.loc:
                    // Validate URL
                    if (text.length > constants_js_1.LIMITS.MAX_URL_LENGTH) {
                        this.logger('warn', `URL exceeds max length of ${constants_js_1.LIMITS.MAX_URL_LENGTH}: ${text.substring(0, 100)}...`);
                        this.err(`URL exceeds max length of ${constants_js_1.LIMITS.MAX_URL_LENGTH}`);
                    }
                    else if (!constants_js_1.LIMITS.URL_PROTOCOL_REGEX.test(text)) {
                        this.logger('warn', `URL must start with http:// or https://: ${text}`);
                        this.err(`URL must start with http:// or https://: ${text}`);
                    }
                    else {
                        currentItem.url = text;
                    }
                    break;
                case types_js_1.TagNames['image:loc']:
                    currentImage.url = text;
                    break;
                case types_js_1.TagNames['video:title']:
                    if (currentVideo.title.length + text.length <=
                        constants_js_1.LIMITS.MAX_VIDEO_TITLE_LENGTH) {
                        currentVideo.title += text;
                    }
                    else {
                        this.logger('warn', `video title exceeds max length of ${constants_js_1.LIMITS.MAX_VIDEO_TITLE_LENGTH}`);
                        this.err(`video title exceeds max length of ${constants_js_1.LIMITS.MAX_VIDEO_TITLE_LENGTH}`);
                    }
                    break;
                case types_js_1.TagNames['video:description']:
                    if (currentVideo.description.length + text.length <=
                        constants_js_1.LIMITS.MAX_VIDEO_DESCRIPTION_LENGTH) {
                        currentVideo.description += text;
                    }
                    else {
                        this.logger('warn', `video description exceeds max length of ${constants_js_1.LIMITS.MAX_VIDEO_DESCRIPTION_LENGTH}`);
                        this.err(`video description exceeds max length of ${constants_js_1.LIMITS.MAX_VIDEO_DESCRIPTION_LENGTH}`);
                    }
                    break;
                case types_js_1.TagNames['news:name']:
                    if (!currentItem.news) {
                        currentItem.news = newsTemplate();
                    }
                    if (currentItem.news.publication.name.length + text.length <=
                        constants_js_1.LIMITS.MAX_NEWS_NAME_LENGTH) {
                        currentItem.news.publication.name += text;
                    }
                    else {
                        this.logger('warn', `news name exceeds max length of ${constants_js_1.LIMITS.MAX_NEWS_NAME_LENGTH}`);
                        this.err(`news name exceeds max length of ${constants_js_1.LIMITS.MAX_NEWS_NAME_LENGTH}`);
                    }
                    break;
                case types_js_1.TagNames['news:title']:
                    if (!currentItem.news) {
                        currentItem.news = newsTemplate();
                    }
                    if (currentItem.news.title.length + text.length <=
                        constants_js_1.LIMITS.MAX_NEWS_TITLE_LENGTH) {
                        currentItem.news.title += text;
                    }
                    else {
                        this.logger('warn', `news title exceeds max length of ${constants_js_1.LIMITS.MAX_NEWS_TITLE_LENGTH}`);
                        this.err(`news title exceeds max length of ${constants_js_1.LIMITS.MAX_NEWS_TITLE_LENGTH}`);
                    }
                    break;
                case types_js_1.TagNames['image:caption']:
                    if (!currentImage.caption) {
                        currentImage.caption =
                            text.length <= constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH
                                ? text
                                : text.substring(0, constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH);
                        if (text.length > constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH) {
                            this.logger('warn', `image caption exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH}`);
                            this.err(`image caption exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH}`);
                        }
                    }
                    else if (currentImage.caption.length + text.length <=
                        constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH) {
                        currentImage.caption += text;
                    }
                    else {
                        this.logger('warn', `image caption exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH}`);
                        this.err(`image caption exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_CAPTION_LENGTH}`);
                    }
                    break;
                case types_js_1.TagNames['image:title']:
                    if (!currentImage.title) {
                        currentImage.title =
                            text.length <= constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH
                                ? text
                                : text.substring(0, constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH);
                        if (text.length > constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH) {
                            this.logger('warn', `image title exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH}`);
                            this.err(`image title exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH}`);
                        }
                    }
                    else if (currentImage.title.length + text.length <=
                        constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH) {
                        currentImage.title += text;
                    }
                    else {
                        this.logger('warn', `image title exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH}`);
                        this.err(`image title exceeds max length of ${constants_js_1.LIMITS.MAX_IMAGE_TITLE_LENGTH}`);
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
                case types_js_1.TagNames['urlset']:
                case types_js_1.TagNames['xhtml:link']:
                case types_js_1.TagNames['video:id']:
                    break;
                case types_js_1.TagNames['video:restriction']:
                    if (attr.name === 'relationship' && (0, validation_js_1.isAllowDeny)(attr.value)) {
                        currentVideo['restriction:relationship'] = attr.value;
                    }
                    else {
                        this.logger('log', 'unhandled attr', currentTag, attr.name);
                        this.err(`unhandled attr: ${currentTag} ${attr.name}`);
                    }
                    break;
                case types_js_1.TagNames['video:price']:
                    if (attr.name === 'type' && (0, validation_js_1.isPriceType)(attr.value)) {
                        currentVideo['price:type'] = attr.value;
                    }
                    else if (attr.name === 'currency') {
                        currentVideo['price:currency'] = attr.value;
                    }
                    else if (attr.name === 'resolution' && (0, validation_js_1.isResolution)(attr.value)) {
                        currentVideo['price:resolution'] = attr.value;
                    }
                    else {
                        this.logger('log', 'unhandled attr for video:price', attr.name);
                        this.err(`unhandled attr: ${currentTag} ${attr.name}`);
                    }
                    break;
                case types_js_1.TagNames['video:player_loc']:
                    if (attr.name === 'autoplay') {
                        currentVideo['player_loc:autoplay'] = attr.value;
                    }
                    else if (attr.name === 'allow_embed' && (0, validation_js_1.isValidYesNo)(attr.value)) {
                        currentVideo['player_loc:allow_embed'] = attr.value;
                    }
                    else {
                        this.logger('log', 'unhandled attr for video:player_loc', attr.name);
                        this.err(`unhandled attr: ${currentTag} ${attr.name}`);
                    }
                    break;
                case types_js_1.TagNames['video:platform']:
                    if (attr.name === 'relationship' && (0, validation_js_1.isAllowDeny)(attr.value)) {
                        currentVideo['platform:relationship'] = attr.value;
                    }
                    else {
                        this.logger('log', 'unhandled attr for video:platform', attr.name, attr.value);
                        this.err(`unhandled attr: ${currentTag} ${attr.name} ${attr.value}`);
                    }
                    break;
                case types_js_1.TagNames['video:gallery_loc']:
                    if (attr.name === 'title') {
                        currentVideo['gallery_loc:title'] = attr.value;
                    }
                    else {
                        this.logger('log', 'unhandled attr for video:galler_loc', attr.name);
                        this.err(`unhandled attr: ${currentTag} ${attr.name}`);
                    }
                    break;
                case types_js_1.TagNames['video:uploader']:
                    if (attr.name === 'info') {
                        currentVideo['uploader:info'] = attr.value;
                    }
                    else {
                        this.logger('log', 'unhandled attr for video:uploader', attr.name);
                        this.err(`unhandled attr: ${currentTag} ${attr.name}`);
                    }
                    break;
                default:
                    this.logger('log', 'unhandled attr', currentTag, attr.name);
                    this.err(`unhandled attr: ${currentTag} ${attr.name}`);
            }
        });
        this.saxStream.on('closetag', (tag) => {
            switch (tag) {
                case types_js_1.TagNames.url:
                    this.urlCount++;
                    if (this.urlCount > constants_js_1.LIMITS.MAX_URL_ENTRIES) {
                        this.logger('error', `Sitemap exceeds maximum of ${constants_js_1.LIMITS.MAX_URL_ENTRIES} URLs`);
                        this.err(`Sitemap exceeds maximum of ${constants_js_1.LIMITS.MAX_URL_ENTRIES} URLs`);
                        currentItem = tagTemplate();
                        break;
                    }
                    this.push(currentItem);
                    currentItem = tagTemplate();
                    break;
                case types_js_1.TagNames['video:video']:
                    if (currentItem.video.length < constants_js_1.LIMITS.MAX_VIDEOS_PER_URL) {
                        currentItem.video.push(currentVideo);
                    }
                    else {
                        this.logger('warn', `URL has too many videos (max ${constants_js_1.LIMITS.MAX_VIDEOS_PER_URL})`);
                        this.err(`URL has too many videos (max ${constants_js_1.LIMITS.MAX_VIDEOS_PER_URL})`);
                    }
                    currentVideo = videoTemplate();
                    break;
                case types_js_1.TagNames['image:image']:
                    if (currentItem.img.length < constants_js_1.LIMITS.MAX_IMAGES_PER_URL) {
                        currentItem.img.push(currentImage);
                    }
                    else {
                        this.logger('warn', `URL has too many images (max ${constants_js_1.LIMITS.MAX_IMAGES_PER_URL})`);
                        this.err(`URL has too many images (max ${constants_js_1.LIMITS.MAX_IMAGES_PER_URL})`);
                    }
                    currentImage = { ...imageTemplate };
                    break;
                case types_js_1.TagNames['xhtml:link']:
                    if (!dontpushCurrentLink) {
                        if (currentItem.links.length < constants_js_1.LIMITS.MAX_LINKS_PER_URL) {
                            currentItem.links.push(currentLink);
                        }
                        else {
                            this.logger('warn', `URL has too many links (max ${constants_js_1.LIMITS.MAX_LINKS_PER_URL})`);
                            this.err(`URL has too many links (max ${constants_js_1.LIMITS.MAX_LINKS_PER_URL})`);
                        }
                    }
                    currentLink = { ...linkTemplate };
                    dontpushCurrentLink = false; // Reset flag for next link
                    break;
                default:
                    break;
            }
        });
    }
    _transform(data, encoding, callback) {
        try {
            const cb = () => callback(this.level === types_js_1.ErrorLevel.THROW && this.errors.length > 0
                ? this.errors[0]
                : null);
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
        this.errorCount++;
        if (this.errors.length < constants_js_1.LIMITS.MAX_PARSER_ERRORS) {
            this.errors.push(new Error(msg));
        }
    }
}
exports.XMLToSitemapItemStream = XMLToSitemapItemStream;
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
async function parseSitemap(xml) {
    const urls = [];
    return new Promise((resolve, reject) => {
        xml
            .pipe(new XMLToSitemapItemStream())
            .on('data', (smi) => urls.push(smi))
            .on('end', () => {
            resolve(urls);
        })
            .on('error', (error) => {
            reject(error);
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
class ObjectStreamToJSON extends node_stream_1.Transform {
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
exports.ObjectStreamToJSON = ObjectStreamToJSON;
